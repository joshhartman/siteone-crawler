//! The crawler child process (GUI-SPEC §4.2–4.4).
//!
//! [`start`] spawns the binary with the argv as a vector (no shell), the run directory as working
//! directory, `stdin`/`stdout`/`stderr` piped and, on Windows, `CREATE_NO_WINDOW`. On Windows the
//! child is put into a Job Object with kill-on-close, so the crawler and everything it starts
//! (Chromium in `--browser` mode) die with [`RunHandle::kill`], with the drop of the handle and
//! with the GUI process itself.
//!
//! Worker threads feed one channel of [`RunMsg`]:
//!
//! - events tail: follows `events_file` from offset 0 (it may not exist yet; poll every
//!   [`EVENTS_POLL`]), splits on `\n`, keeps a partial last line, parses each line
//!   ([`crate::model::events::parse_line`]); a line that is not an event is reported as
//!   [`RunMsg::MalformedEvent`] and skipped;
//! - stdout / stderr readers: raw bytes → [`super::ansi::AnsiParser`] → [`RunMsg::Console`]; the
//!   raw bytes are appended to `console_log` (at most [`CONSOLE_LOG_LIMIT`] bytes);
//! - supervisor: waits for the exit, ends what is left of the process tree, lets the readers
//!   drain, has the tail read the events file to its end one last time, and only then sends
//!   [`RunMsg::Exited`] — **the last message of every run**. A consumer that stops at `Exited` has
//!   seen every event and every console line.
//!
//! The `waker` is called after messages were sent, throttled to one call per [`WAKE_INTERVAL`]
//! (leading edge, and a trailing call for messages sent in between), from a thread of its own.
//! The UI drains the channel each frame (bounded) and applies the messages with
//! [`RunMsg::apply_to`].

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryIter};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::ansi::AnsiParser;
use crate::model::console::{Stream, StyledLine};
use crate::model::events::{self, Event};
use crate::model::run_plan::{ChildEnv, RunPlan};
use crate::model::run_state::RunState;

/// How often the events file is polled while the crawler runs.
pub const EVENTS_POLL: Duration = Duration::from_millis(50);

/// The shortest time between two calls of the waker.
pub const WAKE_INTERVAL: Duration = Duration::from_millis(16);

/// `console.log` is cut at this size (a note says so at its end).
pub const CONSOLE_LOG_LIMIT: u64 = 64 * 1024 * 1024;

/// How often the supervisor checks whether the process has exited.
const EXIT_POLL: Duration = Duration::from_millis(20);

/// How long the supervisor waits for the console readers after the exit.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// A line of the events file longer than this is skipped as malformed.
const MAX_EVENT_LINE: usize = 16 * 1024 * 1024;

const READ_CHUNK: usize = 16 * 1024;

/// The exit code a killed crawler gets on Windows (as `Child::kill` gives it).
#[cfg(windows)]
const KILLED_EXIT_CODE: u32 = 1;

/// What [`start`] needs.
#[derive(Clone)]
pub struct RunRequest {
    pub binary: PathBuf,
    /// Arguments without the program name.
    pub argv: Vec<String>,
    /// Variables added to the child's environment (e.g. the API key).
    pub env: ChildEnv,
    /// The child's working directory.
    pub run_dir: PathBuf,
    pub events_file: PathBuf,
    pub console_log: PathBuf,
}

impl fmt::Debug for RunRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The argv may hold secrets (`--http-auth`); the environment shows names only.
        f.debug_struct("RunRequest")
            .field("binary", &self.binary)
            .field("argc", &self.argv.len())
            .field("env", &self.env)
            .field("run_dir", &self.run_dir)
            .finish()
    }
}

impl RunRequest {
    pub fn from_plan(binary: PathBuf, plan: &RunPlan) -> Self {
        Self {
            binary,
            argv: plan.argv.clone(),
            env: plan.env.clone(),
            run_dir: plan.run_dir.clone(),
            events_file: plan.events_file.clone(),
            console_log: plan.console_log.clone(),
        }
    }
}

/// A message from the workers of a run.
#[derive(Debug, Clone, PartialEq)]
pub enum RunMsg {
    Event(Event),
    /// A line of the events file that is not an event (counted, skipped).
    MalformedEvent {
        error: String,
    },
    /// Complete console lines of one stream.
    Console {
        stream: Stream,
        lines: Vec<StyledLine>,
    },
    /// A worker failed (e.g. the events file could not be read); the run goes on.
    WorkerError {
        message: String,
    },
    /// The process exited; `None` without an exit code. Always the last message of a run.
    Exited {
        code: Option<i32>,
    },
}

impl RunMsg {
    /// Applies the message to the run's state; `now` is when it was received.
    pub fn apply_to(self, state: &mut RunState, now: Instant) {
        match self {
            RunMsg::Event(event) => state.apply(event, now),
            RunMsg::MalformedEvent { .. } => state.malformed_event(),
            RunMsg::Console { lines, .. } => lines.into_iter().for_each(|line| state.push_console(line)),
            RunMsg::WorkerError { message } => {
                state.push_console(StyledLine::plain(Stream::Stderr, format!("[GUI] {message}")))
            }
            RunMsg::Exited { code } => state.process_exited(code),
        }
    }
}

/// A running (or finished) child process. Dropping it kills the process (tree) if it still runs.
pub struct RunHandle {
    receiver: Receiver<RunMsg>,
    child: Arc<Mutex<Child>>,
    /// Kept open for the whole run: its end tells the crawler to stop (`--control-stdin`).
    stdin: Option<ChildStdin>,
    /// Cleared by the supervisor when the process exits.
    running: Arc<AtomicBool>,
    pid: u32,
    #[cfg(windows)]
    job: Option<Arc<job::Job>>,
}

impl RunHandle {
    /// The messages received so far, without blocking.
    pub fn try_iter(&self) -> TryIter<'_, RunMsg> {
        self.receiver.try_iter()
    }

    pub fn receiver(&self) -> &Receiver<RunMsg> {
        &self.receiver
    }

    /// The OS process id of the crawler.
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Asks the crawler to wind down: writes `stop\n` to its stdin and flushes; stdin stays open.
    /// The crawler still analyses and reports what it has (`runFinished.outcome = "cancelled"`).
    /// A process that has already exited is not an error.
    pub fn stop(&mut self) -> io::Result<()> {
        if !self.is_running() {
            return Ok(());
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return Ok(());
        };
        match stdin.write_all(b"stop\n").and_then(|()| stdin.flush()) {
            Ok(()) => Ok(()),
            // The crawler is exiting already.
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe || !self.is_running() => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Kills the process and, on Windows, every process it started. A process that has already
    /// exited is not an error. [`RunMsg::Exited`] follows as usual.
    pub fn kill(&mut self) -> io::Result<()> {
        if !self.is_running() {
            return Ok(());
        }
        #[cfg(windows)]
        if let Some(job) = &self.job
            && job.terminate(KILLED_EXIT_CODE).is_ok()
        {
            return Ok(());
        }
        match lock(&self.child).kill() {
            Ok(()) => Ok(()),
            Err(_) if !self.is_running() => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Whether the process is still running (until its exit has been observed).
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
}

impl Drop for RunHandle {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

/// Starts a run. `waker` is called (throttled, from a worker thread) after messages were sent.
///
/// An error means the process could not be started (e.g. the binary or the run directory does not
/// exist); nothing is left running then.
pub fn start(request: RunRequest, waker: impl Fn() + Send + Sync + 'static) -> io::Result<RunHandle> {
    let mut command = Command::new(&request.binary);
    command
        .args(&request.argv)
        .envs(request.env.0.iter().map(|(name, value)| (name, value)))
        .current_dir(&request.run_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    super::hide_console_window(&mut command);
    let mut child = command.spawn()?;
    let pid = child.id();

    let (sender, receiver) = mpsc::channel();
    let throttle = Throttle::start(waker);
    let out = Outbox {
        sender,
        throttle: throttle.clone(),
    };

    #[cfg(windows)]
    let job = match job::Job::contain(&child) {
        Ok(job) => Some(Arc::new(job)),
        Err(error) => {
            out.send(RunMsg::WorkerError {
                message: format!(
                    "The crawler's child processes could not be tied to the GUI (Job Object: {error}); \
                     stopping a run still works."
                ),
            });
            None
        }
    };

    let stdin = child.stdin.take();
    let log = Arc::new(Mutex::new(ConsoleLog::new(request.console_log.clone())));
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(spawn_reader(stdout, Stream::Stdout, &log, &out));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(spawn_reader(stderr, Stream::Stderr, &log, &out));
    }

    let exited = Arc::new(AtomicBool::new(false));
    let tail = {
        let path = request.events_file.clone();
        let exited = Arc::clone(&exited);
        let out = out.clone();
        thread::Builder::new()
            .name("crawler-events".to_string())
            .spawn(move || tail_events(&path, &exited, &out))
    };

    let child = Arc::new(Mutex::new(child));
    let running = Arc::new(AtomicBool::new(true));
    let supervisor = Supervisor {
        child: Arc::clone(&child),
        running: Arc::clone(&running),
        #[cfg(windows)]
        job: job.clone(),
        readers,
        exited,
        out,
        throttle,
    };
    let handle = RunHandle {
        receiver,
        child,
        stdin,
        running,
        pid,
        #[cfg(windows)]
        job,
    };
    // A thread that cannot be spawned leaves the run unobservable: end it (the drop kills it).
    let tail = tail?;
    thread::Builder::new()
        .name("crawler-supervisor".to_string())
        .spawn(move || supervisor.run(tail))?;
    Ok(handle)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The sending side shared by the workers: every send requests a (throttled) wake.
#[derive(Clone)]
struct Outbox {
    sender: Sender<RunMsg>,
    throttle: Arc<Throttle>,
}

impl Outbox {
    fn send(&self, msg: RunMsg) {
        // A dropped receiver only means nobody listens any more.
        if self.sender.send(msg).is_ok() {
            self.throttle.request();
        }
    }
}

/// Calls the waker from its own thread: at once for the first request, then at most once per
/// [`WAKE_INTERVAL`], with a trailing call for requests made in between.
struct Throttle {
    /// (a wake is pending, the run has ended)
    state: Mutex<(bool, bool)>,
    signal: Condvar,
}

impl Throttle {
    fn start(waker: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        let throttle = Arc::new(Self {
            state: Mutex::new((false, false)),
            signal: Condvar::new(),
        });
        let worker = Arc::clone(&throttle);
        let spawned = thread::Builder::new()
            .name("crawler-waker".to_string())
            .spawn(move || worker.run(waker));
        if spawned.is_err() {
            // Without the thread nobody would wake the UI; it still drains every frame it paints.
            lock(&throttle.state).1 = true;
        }
        throttle
    }

    fn request(&self) {
        let mut state = lock(&self.state);
        if !state.0 {
            state.0 = true;
            self.signal.notify_one();
        }
    }

    /// No more requests follow; a pending wake is still delivered.
    fn close(&self) {
        lock(&self.state).1 = true;
        self.signal.notify_one();
    }

    fn run(&self, waker: impl Fn()) {
        loop {
            {
                let mut state = lock(&self.state);
                while !state.0 && !state.1 {
                    state = self.signal.wait(state).unwrap_or_else(PoisonError::into_inner);
                }
                if !state.0 {
                    return;
                }
                state.0 = false;
            }
            waker();
            thread::sleep(WAKE_INTERVAL);
        }
    }
}

/// `console.log`: the raw bytes of both streams, opened on the first write, cut at the limit.
struct ConsoleLog {
    path: PathBuf,
    file: Option<File>,
    written: u64,
    /// Writing failed or the limit was reached: nothing more is written.
    closed: bool,
}

impl ConsoleLog {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            written: 0,
            closed: false,
        }
    }

    /// Appends `bytes`; returns an error message the first time writing fails.
    fn write(&mut self, bytes: &[u8]) -> Option<String> {
        if self.closed {
            return None;
        }
        let result = self.append(bytes);
        result.err().map(|error| {
            self.closed = true;
            format!("console.log could not be written ({error}); the console view is not affected.")
        })
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.file.is_none() {
            self.file = Some(OpenOptions::new().create(true).append(true).open(&self.path)?);
        }
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        let room = CONSOLE_LOG_LIMIT.saturating_sub(self.written);
        let take = bytes.len().min(usize::try_from(room).unwrap_or(usize::MAX));
        file.write_all(&bytes[..take])?;
        self.written += take as u64;
        if take < bytes.len() {
            self.closed = true;
            writeln!(file, "\n[console.log cut at {CONSOLE_LOG_LIMIT} bytes]")?;
        }
        Ok(())
    }
}

fn spawn_reader(
    pipe: impl Read + Send + 'static,
    stream: Stream,
    log: &Arc<Mutex<ConsoleLog>>,
    out: &Outbox,
) -> io::Result<JoinHandle<()>> {
    let log = Arc::clone(log);
    let out = out.clone();
    let name = match stream {
        Stream::Stdout => "crawler-stdout",
        Stream::Stderr => "crawler-stderr",
    };
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || read_console(pipe, stream, &log, &out))
}

fn read_console(mut pipe: impl Read, stream: Stream, log: &Mutex<ConsoleLog>, out: &Outbox) {
    let mut parser = AnsiParser::new(stream);
    let mut buffer = vec![0; READ_CHUNK];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &buffer[..n];
                if let Some(message) = lock(log).write(chunk) {
                    out.send(RunMsg::WorkerError { message });
                }
                let lines = parser.feed(chunk);
                if !lines.is_empty() {
                    out.send(RunMsg::Console { stream, lines });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                out.send(RunMsg::WorkerError {
                    message: format!("Reading the crawler's {stream:?} failed: {error}"),
                });
                break;
            }
        }
    }
    if let Some(line) = parser.finish() {
        out.send(RunMsg::Console {
            stream,
            lines: vec![line],
        });
    }
}

/// Follows the events file until `exited` is set, then reads it to its end once more.
fn tail_events(path: &Path, exited: &AtomicBool, out: &Outbox) {
    let mut lines = LineSplitter::default();
    let mut file: Option<File> = None;
    let mut failed = false;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        // Read before checking: everything written before the exit is read after this flag.
        let last_round = exited.load(Ordering::Acquire);
        if file.is_none() && !failed {
            match File::open(path) {
                Ok(opened) => file = Some(opened),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    failed = true;
                    out.send(RunMsg::WorkerError {
                        message: format!("The events file cannot be opened: {error}"),
                    });
                }
            }
        }
        if let Some(reader) = file.as_mut() {
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => lines.push(&buffer[..n], &mut |line| out.send(line_message(line))),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        failed = true;
                        file = None;
                        out.send(RunMsg::WorkerError {
                            message: format!("The events file cannot be read: {error}"),
                        });
                        break;
                    }
                }
            }
        }
        if last_round {
            lines.finish(&mut |line| out.send(line_message(line)));
            return;
        }
        thread::park_timeout(EVENTS_POLL);
    }
}

/// A line of the events file, or a reason it is not one.
enum Line {
    Text(Vec<u8>),
    TooLong,
}

fn line_message(line: Line) -> RunMsg {
    let bytes = match line {
        Line::Text(bytes) => bytes,
        Line::TooLong => {
            return RunMsg::MalformedEvent {
                error: format!("line longer than {MAX_EVENT_LINE} bytes"),
            };
        }
    };
    match std::str::from_utf8(&bytes) {
        Ok(text) => match events::parse_line(text) {
            Ok(event) => RunMsg::Event(event),
            Err(error) => RunMsg::MalformedEvent {
                error: error.to_string(),
            },
        },
        Err(error) => RunMsg::MalformedEvent {
            error: format!("not UTF-8: {error}"),
        },
    }
}

/// Splits a byte stream on `\n`, keeping a partial last line for the next push. Blank lines are
/// skipped.
#[derive(Default)]
struct LineSplitter {
    partial: Vec<u8>,
    /// The current line grew over [`MAX_EVENT_LINE`]: skip to its end.
    skipping: bool,
}

impl LineSplitter {
    fn push(&mut self, mut bytes: &[u8], emit: &mut impl FnMut(Line)) {
        while let Some(at) = bytes.iter().position(|byte| *byte == b'\n') {
            if self.skipping {
                self.skipping = false;
                emit(Line::TooLong);
            } else {
                self.partial.extend_from_slice(&bytes[..at]);
                self.emit_partial(emit);
            }
            bytes = &bytes[at + 1..];
        }
        if !self.skipping {
            self.partial.extend_from_slice(bytes);
            if self.partial.len() > MAX_EVENT_LINE {
                self.partial = Vec::new();
                self.skipping = true;
            }
        }
    }

    /// The end of the stream: an unterminated last line is still a line.
    fn finish(&mut self, emit: &mut impl FnMut(Line)) {
        if std::mem::take(&mut self.skipping) {
            emit(Line::TooLong);
        }
        self.emit_partial(emit);
    }

    fn emit_partial(&mut self, emit: &mut impl FnMut(Line)) {
        let line = std::mem::take(&mut self.partial);
        if !line.iter().all(u8::is_ascii_whitespace) {
            emit(Line::Text(line));
        }
    }
}

/// Waits for the exit and sends [`RunMsg::Exited`] once everything before it has been sent.
struct Supervisor {
    child: Arc<Mutex<Child>>,
    running: Arc<AtomicBool>,
    #[cfg(windows)]
    job: Option<Arc<job::Job>>,
    readers: Vec<io::Result<JoinHandle<()>>>,
    exited: Arc<AtomicBool>,
    out: Outbox,
    throttle: Arc<Throttle>,
}

impl Supervisor {
    fn run(self, tail: JoinHandle<()>) {
        let code = self.wait_for_exit();
        self.running.store(false, Ordering::Release);

        // What the crawler started (e.g. Chromium) does not outlive it; this also closes the
        // console pipes such a process may have inherited.
        #[cfg(windows)]
        if let Some(job) = &self.job {
            let _ = job.terminate(KILLED_EXIT_CODE);
        }

        let deadline = Instant::now() + DRAIN_TIMEOUT;
        for reader in self.readers {
            match reader {
                Ok(reader) => {
                    while !reader.is_finished() && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(5));
                    }
                    if reader.is_finished() {
                        let _ = reader.join();
                    } else {
                        self.out.send(RunMsg::WorkerError {
                            message: "The crawler's console output did not end with the process; \
                                      its last lines may be missing."
                                .to_string(),
                        });
                    }
                }
                Err(error) => self.out.send(RunMsg::WorkerError {
                    message: format!("The console reader could not be started: {error}"),
                }),
            }
        }

        self.exited.store(true, Ordering::Release);
        tail.thread().unpark();
        let _ = tail.join();

        self.out.send(RunMsg::Exited { code });
        self.throttle.close();
    }

    fn wait_for_exit(&self) -> Option<i32> {
        loop {
            let status = lock(&self.child).try_wait();
            match status {
                Ok(Some(status)) => return status.code(),
                Ok(None) => thread::sleep(EXIT_POLL),
                Err(error) => {
                    self.out.send(RunMsg::WorkerError {
                        message: format!("The crawler's state cannot be read ({error}); it was ended."),
                    });
                    let mut child = lock(&self.child);
                    let _ = child.kill();
                    return child.wait().ok().and_then(|status| status.code());
                }
            }
        }
    }
}

/// A Job Object with kill-on-close: the crawler and every process it starts are in it, so
/// terminating the job, closing its last handle or the death of the GUI ends them all.
#[cfg(windows)]
mod job {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    pub struct Job(HANDLE);

    // SAFETY: a job handle is a kernel handle; it may be used and closed from any thread.
    unsafe impl Send for Job {}
    // SAFETY: as above; the job functions used here are thread-safe.
    unsafe impl Sync for Job {}

    impl Job {
        /// A new kill-on-close job containing `child`. Processes `child` starts later join it.
        pub fn contain(child: &Child) -> io::Result<Self> {
            // SAFETY: no security attributes (not inheritable), no name; the handle is owned by
            // the returned `Job` and closed on its drop.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self(handle);
            let info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
                BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                    LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                    ..Default::default()
                },
                ..Default::default()
            };
            // SAFETY: `info` is a valid JOBOBJECT_EXTENDED_LIMIT_INFORMATION of the size passed.
            let set = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if set == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: both handles are valid: the job above, the process owned by `child`.
            let assigned = unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) };
            if assigned == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        /// Ends every process in the job.
        pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
            // SAFETY: the handle is valid until drop.
            if unsafe { TerminateJobObject(self.0, exit_code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is owned by this value and closed exactly once. With
            // kill-on-close, processes still in the job end now.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_apply_to_the_run_state() {
        let now = Instant::now();
        let mut state = RunState::new();
        RunMsg::Console {
            stream: Stream::Stdout,
            lines: vec![StyledLine::plain(Stream::Stdout, "hello")],
        }
        .apply_to(&mut state, now);
        RunMsg::MalformedEvent {
            error: "bad".to_string(),
        }
        .apply_to(&mut state, now);
        RunMsg::Exited { code: Some(0) }.apply_to(&mut state, now);
        assert_eq!(state.console.len(), 1);
        assert_eq!(state.malformed_events, 1);
        assert_eq!(state.exit, Some(Some(0)));
    }

    fn request(binary: &str) -> RunRequest {
        RunRequest {
            binary: PathBuf::from(binary),
            argv: vec!["--http-auth=user:secret".to_string()],
            env: ChildEnv(vec![("K".to_string(), "secret".to_string())]),
            run_dir: PathBuf::from("."),
            events_file: PathBuf::from("events.ndjson"),
            console_log: PathBuf::from("console.log"),
        }
    }

    #[test]
    fn the_request_debug_hides_secrets_and_a_missing_binary_is_a_spawn_error() {
        let request = request("./definitely-not-a-crawler-binary.exe");
        assert!(!format!("{request:?}").contains("secret"));
        let error = start(request, || {}).err().expect("no such binary");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    fn split(chunks: &[&[u8]]) -> Vec<Result<String, ()>> {
        let mut splitter = LineSplitter::default();
        let mut lines = Vec::new();
        let mut emit = |line: Line| {
            lines.push(match line {
                Line::Text(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
                Line::TooLong => Err(()),
            })
        };
        for chunk in chunks {
            splitter.push(chunk, &mut emit);
        }
        splitter.finish(&mut emit);
        lines
    }

    #[test]
    fn the_splitter_keeps_partial_lines_and_skips_blank_ones() {
        assert_eq!(
            split(&[b"{\"a\"", b":1}\n\n  \r\n{\"b\":2}\r\n{\"c\""]),
            vec![
                Ok("{\"a\":1}".to_string()),
                Ok("{\"b\":2}\r".to_string()),
                Ok("{\"c\"".to_string())
            ]
        );
        let huge = vec![b'x'; MAX_EVENT_LINE + 10];
        assert_eq!(split(&[&huge, b"x\n{}\n"]), vec![Err(()), Ok("{}".to_string())]);
    }

    #[test]
    fn lines_become_events_or_malformed_reports() {
        assert!(matches!(
            line_message(Line::Text(
                br#"{"type":"phase","name":"crawl","state":"started"}"#.to_vec()
            )),
            RunMsg::Event(Event::Phase(_))
        ));
        assert_eq!(
            line_message(Line::Text(br#"{"type":"fromTheFuture","x":1}"#.to_vec())),
            RunMsg::Event(Event::Unknown)
        );
        assert!(matches!(
            line_message(Line::Text(b"not json".to_vec())),
            RunMsg::MalformedEvent { .. }
        ));
        assert!(matches!(
            line_message(Line::Text(b"\xff\xfe".to_vec())),
            RunMsg::MalformedEvent { .. }
        ));
        assert!(matches!(line_message(Line::TooLong), RunMsg::MalformedEvent { .. }));
    }

    #[test]
    fn the_events_tail_follows_a_growing_file_and_reads_to_its_end_after_the_exit() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let path = tmp.path().join("events.ndjson");
        let (sender, receiver) = mpsc::channel();
        let out = Outbox {
            sender,
            throttle: Throttle::start(|| {}),
        };
        let exited = Arc::new(AtomicBool::new(false));
        let tail = {
            let (path, exited) = (path.clone(), Arc::clone(&exited));
            thread::spawn(move || tail_events(&path, &exited, &out))
        };
        // The file does not exist yet.
        thread::sleep(EVENTS_POLL * 2);
        let mut file = File::create(&path).expect("create");
        file.write_all(b"{\"type\":\"runStarted\",\"protocol\":1}\n{\"type\":\"pha")
            .expect("write");
        let first = receiver.recv_timeout(Duration::from_secs(5)).expect("first event");
        assert!(matches!(first, RunMsg::Event(Event::RunStarted(_))));
        file.write_all(b"se\",\"name\":\"crawl\",\"state\":\"started\"}\ngarbage\n{\"type\":\"runFinished\"}")
            .expect("write");
        drop(file);
        exited.store(true, Ordering::Release);
        tail.thread().unpark();
        tail.join().expect("tail");
        let rest: Vec<RunMsg> = receiver.try_iter().collect();
        assert!(matches!(rest[0], RunMsg::Event(Event::Phase(_))));
        assert!(matches!(rest[1], RunMsg::MalformedEvent { .. }));
        assert!(matches!(rest[2], RunMsg::Event(Event::RunFinished(_))));
        assert_eq!(rest.len(), 3);
    }

    #[test]
    fn the_throttle_coalesces_bursts_and_delivers_a_trailing_wake() {
        use std::sync::atomic::AtomicUsize;
        let calls = Arc::new(AtomicUsize::new(0));
        let throttle = {
            let calls = Arc::clone(&calls);
            Throttle::start(move || {
                calls.fetch_add(1, Ordering::SeqCst);
            })
        };
        let started = Instant::now();
        for _ in 0..100_000 {
            throttle.request();
        }
        let burst = started.elapsed();
        throttle.close();
        let deadline = Instant::now() + Duration::from_secs(5);
        while calls.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        thread::sleep(WAKE_INTERVAL * 3);
        let count = calls.load(Ordering::SeqCst);
        let bound = (burst.as_millis() / WAKE_INTERVAL.as_millis()) as usize + 3;
        assert!((1..=bound).contains(&count), "{count} wakes for a burst of {burst:?}");
    }

    #[test]
    fn the_console_log_is_cut_at_the_limit() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let path = tmp.path().join("console.log");
        let mut log = ConsoleLog::new(path.clone());
        log.written = CONSOLE_LOG_LIMIT - 3;
        assert_eq!(log.write(b"abcdef"), None);
        assert_eq!(log.write(b"more"), None);
        let text = std::fs::read_to_string(&path).expect("log");
        assert!(text.starts_with("abc\n[console.log cut at"), "{text}");
        assert!(!text.contains("more"));

        let mut broken = ConsoleLog::new(tmp.path().join("missing-dir").join("console.log"));
        assert!(broken.write(b"x").is_some());
        assert_eq!(broken.write(b"y"), None, "reported once");
    }
}
