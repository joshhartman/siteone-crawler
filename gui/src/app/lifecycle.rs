//! The lifecycle of a run (GUI-SPEC §4.3–4.5, §5.6, §6.3, §6.4).
//!
//! # State transitions of the current run
//!
//! ```text
//! attach_run ──► Running ──Stop──► Stopping ──(runFinished cancelled)──► Stopped
//!    │              │                  └──Force quit (confirmed)──► kill ──► Aborted
//!    │              ├──(runFinished)──► Finished / Quality gate failed / Failed / No pages
//!    │              ├──(exit without runFinished)──► Crashed (exit N)
//!    │              └──(exit 101, no events)──► Configuration error: dialog, back to Setup,
//!    │                                          run folder removed, nothing recorded
//!    └─ run.json written at once (outcome: none) — a run cut short by a crash of the GUI is
//!       still listed in History (as unfinished)
//! ```
//!
//! [`RunMsg::Exited`] is always the last message of a run. When it has been applied the run is
//! *finalized* once: the final record (outcome, exit code, error, duration, summary numbers) is
//! written to `run.json` and the result JSON (located through the `json` artifact) is loaded, both
//! on one worker, in that order; when the result has quality scores the overall score is added and
//! the record written again.
//!
//! Closing the window while a run is active asks first (*Stop the crawl and close* / *Cancel*);
//! confirmed, the crawler is killed and the run recorded as aborted before the window closes.
//! [`App::abort_for_exit`] does the same when the app exits any other way.
//!
//! A past run (History) is loaded on a worker by [`load_past_run`]: its record, its
//! `events.ndjson` replayed into a [`RunState`], the end of its `console.log` and its result JSON.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Instant;

use eframe::egui;

use super::{Action, App, CrawlerStatus, CurrentRun, Page, ViewedRun, confirm, remove_dir_quietly};
use crate::backend::ansi::AnsiParser;
use crate::backend::runner::{self, RunMsg};
use crate::model::config::{AI_API_KEY, CrawlConfig};
use crate::model::console::{Stream, StyledLine};
use crate::model::history::{self, HEADER_OPTION, RecordedOutcome, RunRecord, RunSummary};
use crate::model::link_check::RunLinks;
use crate::model::results::{self, ResultLoad};
use crate::model::retry::RetryState;
use crate::model::run_plan::{self, API_KEY_ENV, RunPlan};
use crate::model::run_state::{RunOutcome, RunState};
use crate::model::secrets::SecretValues;
use crate::ui::widgets::modal::{ButtonKind, Dialog};
use crate::ui::widgets::{Tone, format};

/// A past run reads at most this much of the end of its `console.log`.
pub const PAST_CONSOLE_BYTES: u64 = 4 * 1024 * 1024;

/// An `events.ndjson` larger than this is not replayed.
pub const MAX_EVENTS_BYTES: u64 = 512 * 1024 * 1024;

/// The `--http-auth` option (a secret: its value is in the credential store).
pub(super) const HTTP_AUTH: &str = "--http-auth";

/// The crawler process of a run, as the app drives it. [`runner::RunHandle`] is the real one; the
/// UI tests drive a scripted child through the same seam.
pub trait ChildControl {
    /// The next message, without blocking.
    fn try_next(&mut self) -> Option<RunMsg>;
    /// Asks the crawler to wind down (`stop` on its stdin).
    fn stop(&mut self) -> io::Result<()>;
    /// Ends the process (tree) at once.
    fn kill(&mut self) -> io::Result<()>;
    fn is_running(&self) -> bool;
}

impl ChildControl for runner::RunHandle {
    fn try_next(&mut self) -> Option<RunMsg> {
        self.receiver().try_recv().ok()
    }

    fn stop(&mut self) -> io::Result<()> {
        runner::RunHandle::stop(self)
    }

    fn kill(&mut self) -> io::Result<()> {
        runner::RunHandle::kill(self)
    }

    fn is_running(&self) -> bool {
        runner::RunHandle::is_running(self)
    }
}

/// The result JSON of a run.
#[derive(Debug, Clone, PartialEq)]
pub enum ResultState {
    /// The run has not ended yet.
    Pending,
    Loading,
    Ready(ResultLoad),
}

impl ResultState {
    pub fn ready(&self) -> Option<&ResultLoad> {
        match self {
            ResultState::Ready(load) => Some(load),
            _ => None,
        }
    }
}

/// A run opened from History.
pub struct PastRun {
    pub run_dir: PathBuf,
    pub load: PastLoad,
}

pub enum PastLoad {
    Loading,
    Ready(Box<PastData>),
    /// Why the run cannot be shown.
    Failed(String),
}

/// Everything the Run page shows of a past run.
pub struct PastData {
    pub record: RunRecord,
    pub state: RunState,
    pub result: ResultLoad,
    /// Its external links and their stored check.
    pub links: RunLinks,
    /// Its retry queue and retry results (`retries.json`), already merged into `state`.
    pub retries: RetryState,
    /// `retries.json` could not be read (it was set aside).
    pub retry_notice: Option<String>,
}

/// A configuration to load into Setup (Run again), with what it cannot bring back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAgain {
    pub config: CrawlConfig,
    pub profile: Option<String>,
    /// The passwords that replace Setup's: the current run's own (kept in memory); none for a run
    /// from History, which keeps no secret. Never the API key (Setup's stays).
    pub secrets: SecretValues,
    /// Values the user has to enter again (passwords History does not keep, headers, an API key
    /// missing from this session).
    pub notes: Vec<String>,
}

impl CurrentRun {
    /// The History record of this run as it stands.
    fn build_record(&self, outcome: Option<RecordedOutcome>, error: Option<String>) -> RunRecord {
        let ended = self.ended.is_some() || outcome.is_some();
        let duration_ms = self
            .state
            .finished
            .as_ref()
            .map(|finished| finished.ms)
            .filter(|ms| *ms > 0)
            .unwrap_or_else(|| u64::try_from(self.elapsed().as_millis()).unwrap_or(u64::MAX));
        RunRecord {
            run_id: self.plan.run_id.clone(),
            url: history::config_snapshot(&self.config).url,
            started_at: self.started_at.to_rfc3339(),
            finished_at: ended.then(|| chrono::Local::now().to_rfc3339()),
            duration_ms: ended.then_some(duration_ms),
            outcome,
            exit_code: self.state.exit.flatten(),
            error,
            crawler_version: self
                .state
                .started
                .as_ref()
                .map(|started| started.version.clone())
                .filter(|version| !version.is_empty())
                .or_else(|| self.crawler_version.clone()),
            profile: self.profile.clone(),
            argv: self.plan.redacted_argv.clone(),
            config: history::config_snapshot(&self.config),
            summary: RunSummary {
                urls: self.state.urls.len() as u64,
                errors: self.state.status_counts.errors(),
                total_bytes: self.state.total_bytes,
                overall_score: self.record.summary.overall_score,
                overall_label: self.record.summary.overall_label.clone(),
            },
        }
    }
}

/// The error text a record keeps for an outcome.
fn outcome_error(outcome: &RunOutcome) -> Option<String> {
    match outcome {
        RunOutcome::Failed { error } => error.clone(),
        RunOutcome::Crashed { exit_code: Some(code) } => {
            Some(format!("The crawler ended unexpectedly (exit code {code})."))
        }
        RunOutcome::Crashed { exit_code: None } => Some("The crawler ended unexpectedly.".to_string()),
        _ => None,
    }
}

/// The result JSON of a run: the `json` artifact, or where the plan put it.
fn result_path(state: &RunState, fallback: &Path) -> PathBuf {
    state
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "json")
        .and_then(|artifact| artifact.path.clone())
        .unwrap_or_else(|| fallback.to_path_buf())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

impl App {
    /// Takes over a started crawler: writes the initial record, makes it the current run and shows
    /// the Run page. `start_crawl` calls it; tests attach a scripted child. The plan was built from
    /// the working secrets just before: the run keeps a copy of them (in memory).
    pub fn attach_run(
        &mut self,
        handle: Box<dyn ChildControl>,
        plan: RunPlan,
        config: CrawlConfig,
        started_at: chrono::DateTime<chrono::Local>,
    ) {
        // A crawl stops a retry pass; what was not retried stays queued.
        self.stop_retry();
        let crawler_version = match &self.crawler {
            CrawlerStatus::Found {
                version: Ok(version), ..
            } => Some(version.clone()),
            _ => None,
        };
        let mut run = CurrentRun {
            handle,
            state: RunState::new(),
            plan,
            config,
            secrets: self.secrets.clone(),
            profile: self.session.profile.clone(),
            crawler_version,
            started_at,
            started: Instant::now(),
            ended: None,
            result: ResultState::Pending,
            record: RunRecord::default(),
            links: RunLinks::default(),
            retries: RetryState::default(),
            auto_check: false,
            finalized: false,
        };
        run.record = run.build_record(None, None);
        // A small file, written at once: a run cut short by a crash of the GUI is still listed.
        if let Err(error) = run.record.write(&run.plan.run_dir) {
            self.toasts.warning(format!(
                "This run will not be listed in History: run.json could not be written ({error})."
            ));
        }
        self.current_run = Some(run);
        self.viewed_run = ViewedRun::Current;
        self.run_ui.new_run();
        self.history_ui.stale = true;
        self.page = Page::Run;
    }

    /// Applies the messages of the current run (bounded per frame), handles a configuration error
    /// of the child (GUI-SPEC §4.5: dialog, back to Setup, run folder removed) and finalizes a run
    /// that has ended.
    pub(super) fn pump_run(&mut self, ctx: &egui::Context) {
        let Some(run) = &mut self.current_run else {
            return;
        };
        let now = Instant::now();
        let mut applied = 0;
        while applied < super::MAX_RUN_MESSAGES_PER_FRAME {
            let Some(message) = run.handle.try_next() else {
                break;
            };
            message.apply_to(&mut run.state, now);
            applied += 1;
        }
        if applied == super::MAX_RUN_MESSAGES_PER_FRAME {
            ctx.request_repaint();
        }
        if run.state.exit.is_none() || run.finalized {
            return;
        }
        run.ended.get_or_insert(now);
        if let RunOutcome::ConfigError { message } = run.state.outcome() {
            let run_dir = run.plan.run_dir.clone();
            self.current_run = None;
            self.spawn_task(move || {
                remove_dir_quietly(&run_dir);
                Box::new(|app: &mut App| app.history_ui.stale = true)
            });
            self.page = Page::Setup;
            self.error_dialog(
                "The crawler rejected the configuration",
                "The crawl did not start. Change the setting the message names and start again.",
                Some(message.unwrap_or_else(|| "The crawler gave no message.".to_string())),
            );
            return;
        }
        self.finalize_run();
    }

    /// Writes the final record and loads the result JSON (one worker, in this order).
    fn finalize_run(&mut self) {
        let Some(run) = &mut self.current_run else {
            return;
        };
        run.finalized = true;
        let outcome = run.state.outcome();
        let record = run.build_record(RecordedOutcome::from_outcome(&outcome), outcome_error(&outcome));
        run.record = record.clone();
        run.result = ResultState::Loading;
        let run_dir = run.plan.run_dir.clone();
        let path = result_path(&run.state, &run.plan.result_json);
        let site_url = site_url(&run.state, &run.config.url);
        self.history_ui.stale = true;
        self.spawn_task(move || {
            let mut record = record;
            let mut error = record.write(&run_dir).err();
            let result = results::load_result(&path);
            if let ResultLoad::Loaded(data) = &result
                && let Some(scores) = &data.scores
            {
                record.summary.overall_score = Some(scores.overall.score);
                record.summary.overall_label = Some(scores.overall.label.clone()).filter(|label| !label.is_empty());
                if let Err(failed) = record.write(&run_dir) {
                    error.get_or_insert(failed);
                }
            }
            let data = match &result {
                ResultLoad::Loaded(data) => Some(data.as_ref()),
                _ => None,
            };
            let links = RunLinks::load(&run_dir, data, &site_url);
            // A run deleted from History in the meantime is gone on purpose: no warning.
            let error = error.filter(|_| run_dir.is_dir());
            Box::new(move |app: &mut App| {
                if let Some(run) = &mut app.current_run
                    && run.plan.run_dir == run_dir
                {
                    run.result = ResultState::Ready(result);
                    run.record = record;
                    run.links = links;
                    app.write_link_report(&run_dir);
                }
                app.history_ui.stale = true;
                if let Some(error) = error {
                    app.toasts.warning(format!(
                        "The run could not be saved for History: run.json could not be written ({error})."
                    ));
                }
            })
        });
    }

    /// Stop: asks the crawler to wind down; it still analyses and reports what it has.
    pub fn stop_run(&mut self) {
        let Some(run) = &mut self.current_run else {
            return;
        };
        if !run.is_active() || run.state.stop_requested {
            return;
        }
        // Stopping either way: the button becomes Force quit, which always works.
        run.state.stop_requested = true;
        if let Err(error) = run.handle.stop() {
            self.toasts.error(format!(
                "The crawler could not be asked to stop ({error}). Use Force quit to end it."
            ));
        }
    }

    /// Force quit: asks first.
    pub fn request_force_quit(&mut self) {
        if !self.has_active_run() {
            return;
        }
        self.open_dialog(confirm(
            "Force quit the crawl?",
            "The crawler is ended at once. Nothing more is analysed and no report is written; the run is kept in History as aborted.",
            "End the crawl now",
            ButtonKind::Danger,
            Action::ForceQuit,
        ));
    }

    /// Kills the crawler (confirmed). The exit follows as a message; the outcome is Aborted.
    pub(super) fn force_quit(&mut self) {
        let Some(run) = &mut self.current_run else {
            return;
        };
        if !run.is_active() {
            return;
        }
        let running = run.handle.is_running();
        match run.handle.kill() {
            Ok(()) => run.state.killed = running,
            Err(error) => self.toasts.error(format!("The crawler could not be ended: {error}")),
        }
    }

    /// The window's close button during a run: cancel the close and ask (GUI-SPEC §4.4).
    pub(super) fn handle_close_request(&mut self, ctx: &egui::Context) {
        let requested = ctx.input(|input| input.viewport().close_requested());
        if !requested || self.close_confirmed || !self.has_active_run() {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        // Closed from the taskbar while minimized: bring the window back to show the question.
        if ctx.input(|input| input.viewport().minimized == Some(true)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        self.open_dialog(
            Dialog::new("A crawl is running")
                .tone(Tone::Warning)
                .message(
                    "Closing the window ends the crawl now. What it has crawled is not analysed; the run is kept in History as aborted.",
                )
                .button("Stop the crawl and close", ButtonKind::Danger, Action::StopAndClose)
                .cancel("Cancel"),
        );
    }

    /// *Stop the crawl and close*: kill, record as aborted, close the window.
    pub(super) fn stop_and_close(&mut self) {
        self.abort_for_exit();
        self.close_confirmed = true;
        self.close_now = true;
    }

    /// Kills an active run and records it as aborted, synchronously (the app is about to exit).
    /// What the crawler reported before is applied first: a run that has already ended is recorded
    /// with its own outcome (without the overall score, which needs the result JSON).
    pub fn abort_for_exit(&mut self) {
        let Some(run) = &mut self.current_run else {
            return;
        };
        let now = Instant::now();
        while let Some(message) = run.handle.try_next() {
            message.apply_to(&mut run.state, now);
        }
        if run.finalized {
            return;
        }
        if run.state.exit.is_some() {
            run.ended.get_or_insert(now);
            run.finalized = true;
            let outcome = run.state.outcome();
            if let RunOutcome::ConfigError { .. } = outcome {
                remove_dir_quietly(&run.plan.run_dir);
                return;
            }
            let record = run.build_record(RecordedOutcome::from_outcome(&outcome), outcome_error(&outcome));
            let _ = record.write(&run.plan.run_dir);
            run.record = record;
            return;
        }
        if !run.is_active() {
            return;
        }
        let running = run.handle.is_running();
        if run.handle.kill().is_ok() && running {
            run.state.killed = true;
        }
        run.ended.get_or_insert_with(Instant::now);
        run.finalized = true;
        let record = run.build_record(Some(RecordedOutcome::Aborted), None);
        if let Err(error) = record.write(&run.plan.run_dir) {
            self.toasts
                .warning(format!("The run could not be recorded as aborted: {error}"));
        }
        if let Some(run) = &mut self.current_run {
            run.record = record;
        }
    }

    /// Opens a run of History on the Run page; it is loaded on a worker.
    pub fn open_past_run(&mut self, run_dir: PathBuf) {
        self.page = Page::Run;
        if self
            .current_run
            .as_ref()
            .is_some_and(|run| same_dir(&run.plan.run_dir, &run_dir))
        {
            self.viewed_run = ViewedRun::Current;
            return;
        }
        self.leave_past_run();
        self.past_generation += 1;
        let generation = self.past_generation;
        self.past_run = Some(PastRun {
            run_dir: run_dir.clone(),
            load: PastLoad::Loading,
        });
        self.viewed_run = ViewedRun::Past {
            run_dir: run_dir.clone(),
        };
        self.spawn_task(move || {
            let loaded = load_past_run(&run_dir);
            Box::new(move |app: &mut App| {
                if app.past_generation != generation {
                    return;
                }
                if let Some(past) = &mut app.past_run {
                    past.load = match loaded {
                        Ok(mut data) => {
                            if let Some(notice) = data.retry_notice.take() {
                                app.toasts.warning(notice);
                            }
                            PastLoad::Ready(Box::new(data))
                        }
                        Err(error) => PastLoad::Failed(error),
                    };
                }
            })
        });
    }

    /// Shows the current run (or the empty state) on the Run page.
    pub fn show_current_run(&mut self) {
        self.leave_past_run();
        self.viewed_run = ViewedRun::Current;
        self.past_run = None;
        self.page = Page::Run;
    }

    /// The run from History is no longer shown: a retry pass of it stops (its results live in the
    /// run shown; what was not retried stays queued).
    fn leave_past_run(&mut self) {
        if let (Some(past), Some(active)) = (&self.past_run, &self.retry)
            && past.run_dir == active.run_dir
        {
            self.stop_retry();
        }
    }

    /// Run again for the current run: its configuration and its passwords (in memory, complete).
    pub fn run_again_current(&mut self) {
        let Some(run) = &self.current_run else {
            return;
        };
        let config = run.config.clone();
        let profile = run.profile.clone();
        let argv = run.plan.redacted_argv.clone();
        let mut secrets = run.secrets.clone();
        secrets.remove(AI_API_KEY);
        self.request_run_again(config, profile, &argv, false, secrets);
    }

    /// Run again for a recorded run: its stored configuration (without secrets and headers).
    pub fn run_again_record(&mut self, record: &RunRecord) {
        let mut config = record.config.clone();
        if config.url.trim().is_empty() {
            config.url = record.url.clone();
        }
        self.request_run_again(
            config,
            record.profile.clone(),
            &record.argv,
            headers_withheld(record),
            SecretValues::default(),
        );
    }

    /// The values of a run's argv that Run again cannot bring back: the passwords it used that are
    /// not in `kept` (History keeps none; no value of this session takes their place), the HTTP
    /// headers History does not keep, and the AI API key when this session has none.
    fn missing_values(&self, argv: &[String], headers_withheld: bool, kept: &SecretValues) -> Vec<String> {
        let mut notes = Vec::new();
        for name in self.schema.secret_options() {
            if name != AI_API_KEY && uses_option(argv, name) && kept.get(name).is_none() {
                notes.push(format!("{name} (History does not keep passwords)"));
            }
        }
        if argv.iter().any(|arg| *arg == format!("--ai-api-key-env={API_KEY_ENV}"))
            && self.secrets.get(AI_API_KEY).is_none()
        {
            notes.push("the AI API key (not saved in this session)".to_string());
        }
        if headers_withheld {
            notes.push("the HTTP headers (--header), which History does not keep".to_string());
        }
        notes
    }

    fn request_run_again(
        &mut self,
        config: CrawlConfig,
        profile: Option<String>,
        argv: &[String],
        headers_withheld: bool,
        secrets: SecretValues,
    ) {
        let notes = self.missing_values(argv, headers_withheld, &secrets);
        let run_again = RunAgain {
            config,
            profile,
            secrets,
            notes,
        };
        let replaced = run_again.secrets.clone().replacing(&self.secrets);
        if self.is_modified() && (self.session.config != run_again.config || self.secrets != replaced) {
            self.open_dialog(confirm(
                "Replace the current configuration?",
                "Setup has changes that are not saved in a profile. Run again replaces them with the configuration of this run.",
                "Replace",
                ButtonKind::Danger,
                Action::RunAgain(Box::new(run_again)),
            ));
        } else {
            self.apply_run_again(run_again);
        }
    }

    /// Loads a run's configuration into Setup and shows it. Its passwords replace Setup's (a run
    /// from History has none: Setup's are cleared, never sent to this run's site); the API key
    /// stays.
    pub(super) fn apply_run_again(&mut self, run_again: RunAgain) {
        let RunAgain {
            config,
            profile,
            secrets,
            notes,
        } = run_again;
        self.session.config = config;
        self.secrets = secrets.replacing(&self.secrets);
        let profile = profile.filter(|slug| {
            self.profiles
                .iter()
                .any(|item| item.slug == *slug && item.error.is_none())
        });
        match profile {
            Some(slug) if self.load_baseline(&slug).is_ok() => self.session.profile = Some(slug),
            _ => {
                self.session.profile = None;
                self.baseline = None;
            }
        }
        self.setup.editor.clear();
        self.setup.start_error = None;
        self.page = Page::Setup;
        if notes.is_empty() {
            self.toasts
                .success("The configuration of the run is loaded. Review it and start the crawl.");
        } else {
            self.toasts.warning(format!(
                "The configuration of the run is loaded. Enter again: {}.",
                notes.join("; ")
            ));
        }
    }
}

/// Loads a past run (call it on a worker): its record, its events replayed, the end of its console
/// output and its result JSON. A run without `runFinished` is completed with the recorded exit;
/// one that never ended (the GUI closed during it) reads as aborted.
pub fn load_past_run(run_dir: &Path) -> Result<PastData, String> {
    let record = RunRecord::read(run_dir).map_err(|error| format!("Its run.json cannot be read: {error}."))?;
    let events_path = run_dir.join(run_plan::EVENTS_FILE);
    let text = match fs::metadata(&events_path) {
        Ok(metadata) if metadata.len() > MAX_EVENTS_BYTES => {
            return Err(format!(
                "Its events file is too large to show ({}).",
                format::bytes(metadata.len())
            ));
        }
        Ok(_) => fs::read(&events_path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|error| format!("Its events file cannot be read: {error}."))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("Its events file cannot be read: {error}.")),
    };
    let mut state = RunState::replay(text.lines(), Instant::now());
    drop(text);
    if state.finished.is_none() && matches!(record.outcome, Some(RecordedOutcome::Aborted) | None) {
        state.killed = true;
    }
    state.process_exited(record.exit_code);
    // The run folder may have moved since (another runs folder): artifacts follow it.
    if let Some(working_dir) = state
        .started
        .as_ref()
        .map(|started| PathBuf::from(&started.working_dir))
        .filter(|dir| !dir.as_os_str().is_empty() && dir != run_dir)
    {
        for artifact in &mut state.artifacts {
            if let Some(path) = &artifact.path
                && let Ok(relative) = path.strip_prefix(&working_dir)
            {
                artifact.path = Some(run_dir.join(relative));
            }
        }
    }
    for line in read_console_tail(&run_dir.join(run_plan::CONSOLE_LOG)) {
        state.push_console(line);
    }
    let result = results::load_result(&result_path(&state, &run_dir.join(run_plan::RESULT_JSON)));
    let data = match &result {
        ResultLoad::Loaded(data) => Some(data.as_ref()),
        _ => None,
    };
    let links = RunLinks::load(run_dir, data, &site_url(&state, &record.url));
    let (retries, retry_notice) = RetryState::load(run_dir);
    retries.apply_to_state(&mut state);
    Ok(PastData {
        record,
        state,
        result,
        links,
        retries,
        retry_notice,
    })
}

/// Whether a recorded run used `--header` values its record does not keep.
pub(super) fn headers_withheld(record: &RunRecord) -> bool {
    record.argv.iter().any(|arg| arg.starts_with(HEADER_OPTION)) && !record.config.values.contains_key(HEADER_OPTION)
}

/// Whether `argv` sets the option `name`.
fn uses_option(argv: &[String], name: &str) -> bool {
    argv.iter()
        .any(|arg| arg.starts_with(&format!("{name}=")) || arg.starts_with(&format!("{name}:=")))
}

/// The request credentials of a recorded run that History does not keep — `--http-auth` and the
/// `--header` values. A follow-up request that would send them (a retry of a crawled URL, a
/// source page for link texts) is not made, and no value of this session takes their place:
/// Setup's password may belong to another site.
pub(super) fn credentials_not_kept(record: &RunRecord) -> Vec<String> {
    let mut missing = Vec::new();
    if uses_option(&record.argv, HTTP_AUTH) {
        missing.push(format!("{HTTP_AUTH} (History does not keep passwords)"));
    }
    if headers_withheld(record) {
        missing.push("the HTTP headers (--header), which History does not keep".to_string());
    }
    missing
}

/// The start URL of a run, against which the result JSON's `skipped` table resolves its pages.
pub(super) fn site_url(state: &RunState, fallback: &str) -> String {
    state
        .started
        .as_ref()
        .map(|started| started.url.clone())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

/// The last [`PAST_CONSOLE_BYTES`] of a `console.log`, parsed into lines.
fn read_console_tail(path: &Path) -> Vec<StyledLine> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let skip = length.saturating_sub(PAST_CONSOLE_BYTES);
    let mut lines = Vec::new();
    if skip > 0 {
        if file.seek(SeekFrom::Start(skip)).is_err() {
            return lines;
        }
        lines.push(StyledLine::plain(
            Stream::Stdout,
            format!(
                "[GUI] The first {} of the output are only in console.log.",
                format::bytes(skip)
            ),
        ));
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return lines;
    }
    let mut parser = AnsiParser::new(Stream::Stdout);
    let mut parsed = parser.feed(&bytes);
    if skip > 0 && !parsed.is_empty() {
        // The first line was cut by the seek.
        parsed.remove(0);
    }
    lines.extend(parsed);
    lines.extend(parser.finish());
    lines
}
