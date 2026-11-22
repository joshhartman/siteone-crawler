//! A retry pass (GUI-SPEC §4.9): a list of URLs requested again, one after another, after the crawl.
//!
//! # Rules
//!
//! - **Sequential:** one request at a time; at least [`PassSettings::host_gap`] between the end of
//!   one request to a host and the start of the next one to it.
//! - **A crawled URL** is requested by the crawler itself: one child process per URL, started with
//!   [`super::runner::start`] (no shell, no console window, a kill-on-close Job Object), argv from
//!   [`retry::crawl_argv`], working directory the run folder, its events and result JSON in
//!   `<run>/.retry/` (removed afterwards). The answer is the child's `url` event; the title of an OK
//!   HTML answer comes from the `seo` table of its result JSON. An answer the child reports as
//!   `cached` is not a retry: the URL is requested once more with the cache off. Exit code 3 ("no
//!   pages crawled") with a `url` event is an answer like any other.
//! - **An external link** is checked again with [`link_check::check_link`] (one `GET`, no site
//!   credentials).
//! - **No answer** — no crawler binary, the child ended without a `url` event, the watchdog
//!   ([`PassSettings::watchdog`]) ended it — is a [`RetryMsg::Failed`] with the reason; a panic is
//!   caught and reported the same way.
//! - **Cancel** ([`RetryHandle::cancel`], or dropping the handle) kills the child in flight and ends
//!   the pass; nothing more is sent.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;

use super::catch_panic_unchecked;
use super::link_check::{self, CheckSettings, HostHealth};
use super::runner::{self, RunMsg, RunRequest};
use crate::model::events::Event;
use crate::model::link_check::{ExternalLink, Failure};
use crate::model::results::{self, ResultLoad};
use crate::model::retry::{self, Answer, ChildFiles, CrawledAnswer};
use crate::model::run_plan::ChildEnv;
use crate::model::run_state::StatusClass;

/// The folder of a run where a child writes its files (removed after each URL).
pub const WORK_DIR: &str = ".retry";

/// How long the watchdog gives a child on top of the run's `--timeout`.
pub const WATCHDOG_EXTRA: Duration = Duration::from_secs(30);

/// How often the pass looks at the child (also the cancel latency).
const POLL: Duration = Duration::from_millis(50);

/// What is requested for one URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryTarget {
    /// A crawled URL, requested by a crawler child with the run's request options
    /// ([`retry::request_options`]).
    Crawled { options: Vec<String> },
    /// An external link, checked again.
    External(ExternalLink),
    /// Not requested, and why (e.g. the run's configuration is incomplete).
    Refused(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryItem {
    pub url: String,
    pub target: RetryTarget,
}

/// How a pass runs.
#[derive(Debug, Clone)]
pub struct PassSettings {
    /// The crawler binary; `None` when it was not found.
    pub binary: Option<PathBuf>,
    /// The run folder: the child's working directory.
    pub run_dir: PathBuf,
    /// The app's empty config file, passed as `--config-file` like in a run.
    pub config_file: PathBuf,
    /// The longest a child may run (the run's `--timeout` plus [`WATCHDOG_EXTRA`]).
    pub watchdog: Duration,
    pub host_gap: Duration,
    /// The external link check's settings (User-Agent, proxy, timeout).
    pub check: CheckSettings,
}

/// A message of a running pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryMsg {
    /// The pass starts on this URL.
    Started(String),
    Answered {
        url: String,
        answer: Answer,
    },
    /// No answer; the reason.
    Failed {
        url: String,
        reason: String,
    },
    /// Every URL was handled: always the last message (not sent after a cancel).
    Finished,
}

/// A running pass. Dropping it cancels the pass.
pub struct RetryHandle {
    receiver: Receiver<RetryMsg>,
    cancel: Arc<AtomicBool>,
}

impl RetryHandle {
    pub fn try_next(&self) -> Option<RetryMsg> {
        self.receiver.try_recv().ok()
    }

    pub fn receiver(&self) -> &Receiver<RetryMsg> {
        &self.receiver
    }

    /// Ends the pass: the child in flight is killed, nothing more is sent.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for RetryHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Starts a pass over `items` on a worker thread; returns at once. `waker` is called after every
/// message.
pub fn start(
    items: Vec<RetryItem>,
    settings: PassSettings,
    waker: impl Fn() + Send + Sync + 'static,
) -> io::Result<RetryHandle> {
    let (sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    thread::Builder::new()
        .name("siteone-gui-retry".to_string())
        .spawn(move || run_pass(items, &settings, &worker_cancel, &sender, &waker))?;
    Ok(RetryHandle { receiver, cancel })
}

/// The result of one URL.
enum Handled {
    Answer(Answer),
    Failed(String),
    Cancelled,
}

fn run_pass(
    items: Vec<RetryItem>,
    settings: &PassSettings,
    cancel: &AtomicBool,
    sender: &Sender<RetryMsg>,
    waker: &(impl Fn() + Send + Sync),
) {
    let send = |message: RetryMsg| {
        let sent = sender.send(message).is_ok();
        waker();
        sent
    };
    let mut client: Option<Result<Client, String>> = None;
    let mut ready_at: HashMap<String, Instant> = HashMap::new();
    // A host that does not answer is asked a few times, not once per URL.
    let mut health = HostHealth::default();
    for item in items {
        let host = link_check::host_key(&item.url);
        let origin = link_check::origin_key(&item.url);
        if !matches!(item.target, RetryTarget::Refused(_))
            && let Some(failure) = health.given_up(&origin)
        {
            if cancel.load(Ordering::SeqCst)
                || !send(RetryMsg::Started(item.url.clone()))
                || !send(RetryMsg::Failed {
                    url: item.url,
                    reason: link_check::not_requested_text(&host, failure),
                })
            {
                return;
            }
            continue;
        }
        if !matches!(item.target, RetryTarget::Refused(_)) {
            while let Some(ready) = ready_at.get(&host).copied() {
                let now = Instant::now();
                if now >= ready || cancel.load(Ordering::SeqCst) {
                    break;
                }
                thread::sleep((ready - now).min(POLL));
            }
        }
        if cancel.load(Ordering::SeqCst) || !send(RetryMsg::Started(item.url.clone())) {
            return;
        }
        let handled = catch_panic_unchecked(|| match &item.target {
            RetryTarget::Crawled { options } => retry_crawled(&item.url, options, settings, cancel),
            RetryTarget::External(link) => {
                let client = client.get_or_insert_with(|| link_check::build_client(&settings.check));
                match client {
                    Ok(client) => Handled::Answer(Answer::External(link_check::check_link(client, link, cancel))),
                    Err(error) => Handled::Failed(error.clone()),
                }
            }
            RetryTarget::Refused(reason) => Handled::Failed(reason.clone()),
        })
        .unwrap_or_else(|_| {
            Handled::Failed("The retry failed with an internal error; see crash.log in the data folder.".to_string())
        });
        if let Handled::Answer(answer) = &handled {
            match answer {
                Answer::External(result) => health.record(&origin, result),
                Answer::Crawled(crawled) if crawled.status > 0 => health.answered(&origin),
                // The crawler reports a timeout as -2; its other negative codes are failed connections.
                Answer::Crawled(crawled) if crawled.status == -2 => health.failed(&origin, Failure::Timeout),
                Answer::Crawled(_) => health.failed(&origin, Failure::Connect),
            }
        }
        if !matches!(item.target, RetryTarget::Refused(_)) {
            ready_at.insert(host, Instant::now() + settings.host_gap);
        }
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        let message = match handled {
            Handled::Answer(answer) => RetryMsg::Answered { url: item.url, answer },
            Handled::Failed(reason) => RetryMsg::Failed { url: item.url, reason },
            Handled::Cancelled => return,
        };
        if !send(message) {
            return;
        }
    }
    send(RetryMsg::Finished);
}

/// Why a child gave no run to look at.
enum NoRun {
    Cancelled,
    NotStarted(String),
}

/// What one child run reported.
struct ChildRun {
    url: Option<crate::model::events::UrlEvent>,
    exit: Option<Option<i32>>,
    /// The last `ERROR:` text on its console.
    error: Option<String>,
    timed_out: bool,
}

fn retry_crawled(url: &str, options: &[String], settings: &PassSettings, cancel: &AtomicBool) -> Handled {
    let Some(binary) = settings.binary.as_deref() else {
        return Handled::Failed("The crawler program (siteone-crawler.exe) was not found.".to_string());
    };
    let work = settings.run_dir.join(WORK_DIR);
    // Never into a run folder that was deleted meanwhile.
    match fs::create_dir(&work) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Handled::Failed("The run folder no longer exists.".to_string());
        }
        Err(error) => return Handled::Failed(format!("The run folder cannot be written: {error}.")),
    }
    let files = ChildFiles {
        events: work.join("events.ndjson"),
        result: work.join("result.json"),
        config: settings.config_file.clone(),
    };
    let handled = (|| {
        let mut use_cache = true;
        loop {
            let _ = fs::remove_file(&files.events);
            let _ = fs::remove_file(&files.result);
            let argv = retry::crawl_argv(options, url, &files, use_cache);
            let run = match run_child(binary, argv, settings, &work, &files, cancel) {
                Ok(run) => run,
                Err(NoRun::Cancelled) => return Handled::Cancelled,
                Err(NoRun::NotStarted(reason)) => return Handled::Failed(reason),
            };
            let Some(event) = run.url else {
                return Handled::Failed(no_answer(&run, settings.watchdog));
            };
            if event.cached && use_cache {
                // Not a request: once more with the cache off.
                use_cache = false;
                continue;
            }
            let mut answer = CrawledAnswer {
                status: i32::try_from(event.status).unwrap_or(i32::MIN),
                content_type: event.content_type,
                time_ms: event.time_ms,
                size: event.size,
                cached: event.cached,
                title: None,
            };
            if StatusClass::of(event.status) == StatusClass::Success && answer.content_type == "HTML" {
                answer.title = page_title(&files.result, url);
            }
            return Handled::Answer(Answer::Crawled(answer));
        }
    })();
    let _ = fs::remove_dir_all(&work);
    handled
}

/// Why a child gave no answer.
fn no_answer(run: &ChildRun, watchdog: Duration) -> String {
    let detail = run
        .error
        .as_deref()
        .map(|error| format!(": {error}"))
        .unwrap_or_default();
    if run.timed_out {
        format!("No answer within {} s; the crawler was ended.", watchdog.as_secs())
    } else {
        match run.exit {
            Some(Some(101)) => format!("The crawler rejected the options{detail}"),
            Some(Some(code)) => format!("The crawler ended without an answer (exit code {code}){detail}"),
            _ => format!("The crawler ended without an answer{detail}"),
        }
    }
}

/// Runs one child to its exit (or kills it on cancel or by the watchdog).
fn run_child(
    binary: &Path,
    argv: Vec<String>,
    settings: &PassSettings,
    work: &Path,
    files: &ChildFiles,
    cancel: &AtomicBool,
) -> Result<ChildRun, NoRun> {
    let request = RunRequest {
        binary: binary.to_path_buf(),
        argv,
        env: ChildEnv::default(),
        run_dir: settings.run_dir.clone(),
        events_file: files.events.clone(),
        console_log: work.join("console.log"),
    };
    let mut handle = runner::start(request, || {})
        .map_err(|error| NoRun::NotStarted(format!("The crawler could not be started: {error}.")))?;
    let deadline = Instant::now() + settings.watchdog;
    let mut run = ChildRun {
        url: None,
        exit: None,
        error: None,
        timed_out: false,
    };
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = handle.kill();
            return Err(NoRun::Cancelled);
        }
        if !run.timed_out && Instant::now() >= deadline {
            run.timed_out = true;
            let _ = handle.kill();
        }
        match handle.receiver().recv_timeout(POLL) {
            Ok(RunMsg::Event(Event::Url(event))) => {
                run.url.get_or_insert(event);
            }
            Ok(RunMsg::Console { lines, .. }) => {
                for line in lines {
                    let text = line.text();
                    if let Some(at) = text.find("ERROR:") {
                        let error = text[at + "ERROR:".len()..].trim().to_string();
                        if !error.is_empty() {
                            run.error = Some(error);
                        }
                    }
                }
            }
            Ok(RunMsg::Exited { code }) => {
                run.exit = Some(code);
                return Ok(run);
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(run),
        }
    }
}

/// The title of `url` in the `seo` table of a child's result JSON.
fn page_title(result: &Path, url: &str) -> Option<String> {
    let ResultLoad::Loaded(data) = results::load_result(result) else {
        return None;
    };
    let table = data.table("seo")?;
    let column = |key: &str| table.columns.iter().position(|column| column.key == key);
    let (path, title) = (column("urlPathAndQuery")?, column("title")?);
    let parsed = url::Url::parse(url).ok()?;
    let key = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_string(),
    };
    table
        .rows
        .iter()
        .find(|row| row.get(path).is_some_and(|cell| *cell == key))
        .and_then(|row| row.get(title))
        .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|title| !title.is_empty())
}
