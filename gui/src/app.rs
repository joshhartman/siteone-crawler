//! The application: state, page routing, the message pump of a run, background tasks, dialogs,
//! toasts, autosave and window geometry (GUI-SPEC §6.1, §7).
//!
//! # Structure (the seams later phases build on)
//!
//! - [`App`] owns everything. It is built from [`AppDeps`], so tests inject a temporary data
//!   folder ([`AppPaths`]), an in-memory [`SecretStore`], scripted [`FileDialogs`] and a fixed
//!   [`CrawlerProbe`] — they never touch the real data folder, the Credential Manager or a dialog.
//! - [`App::show`] draws one frame into a `Ui` (eframe calls it through [`eframe::App::ui`]; the
//!   UI tests call it from an `egui_kittest` harness). [`App::logic`] does the part that draws
//!   nothing (run messages, background completions, close requests); eframe also calls it while
//!   the window is minimized or covered and `ui` is not called.
//! - Pages are free functions `ui::<page>::show(&mut App, &mut Ui)`, routed by [`App::page`].
//! - Background work: [`App::spawn_task`] runs a closure on a worker thread; it returns a
//!   [`Completion`] that is applied to the `App` on the UI thread in the next frame. The UI thread
//!   never blocks on a file, a process or the network.
//! - A run: [`App::start_crawl`] validates, creates the run folder, builds the plan and starts the
//!   child; the [`CurrentRun`] (handle + [`RunState`] + plan + configuration snapshot) is pumped
//!   every frame (bounded). [`App::viewed_run`] says what the Run page shows.
//! - Dialogs: one [`Dialog`] at a time in [`App::dialog`]; its buttons carry an [`Action`] that
//!   [`App::perform`] executes. Toasts: [`App::toasts`].
//! - The lifecycle of a run (stop, force quit, the History record, the result JSON, closing the
//!   window during a run, opening a past run, Run again) is in the `lifecycle` submodule; the
//!   external link check, link texts, the retry queue and the link report of a run are in `links`,
//!   `texts`, `retry` and `report`.

use std::fs;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, CentralPanel, Frame, Key, Modifiers, Panel};

mod autosave;
mod lifecycle;
mod links;
mod report;
mod retry;
mod texts;

pub use lifecycle::{ChildControl, PastData, PastLoad, PastRun, ResultState, RunAgain, load_past_run};
pub use links::{ActiveCheck, CheckControl, MAX_CHECK_MESSAGES_PER_FRAME};
pub use retry::{ActiveRetry, RetryControl, RetrySpawner};
pub use texts::TextJob;

use self::autosave::{Autosave, Done, Report, Step};
use crate::backend::{runner, tools};
use crate::model::config::{CrawlConfig, Shell, check_url, complete_url};
use crate::model::history::RunRecord;
use crate::model::link_check::RunLinks;
use crate::model::paths::{self, AppPaths, CrawlerLocation, CrawlerSource};
use crate::model::presets::{self, Preset};
use crate::model::profiles::{self, Session};
use crate::model::retry::RetryState;
use crate::model::run_plan::{self, RunPaths, RunPlan};
use crate::model::run_state::RunState;
use crate::model::schema::OptionSchema;
use crate::model::secrets::{SESSION_SCOPE, SecretStore, SecretValues};
use crate::model::settings::{Settings, Theme, WindowGeometry};
use crate::theme;
use crate::ui::widgets::Tone;
use crate::ui::widgets::modal::{ButtonKind, Dialog, DialogOutcome};
use crate::ui::widgets::toast::Toasts;
use crate::ui::{history, nav, run, settings as settings_page, setup};

/// How long the working configuration and the settings wait for quiet before they are saved.
pub const AUTOSAVE_DELAY: Duration = Duration::from_millis(800);

/// At most this many messages of a run are applied per frame (GUI-SPEC §7.3).
pub const MAX_RUN_MESSAGES_PER_FRAME: usize = 2_000;

/// Width of the navigation rail.
pub const NAV_WIDTH: f32 = 84.0;

/// The pages of the navigation rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Setup,
    Run,
    History,
    Settings,
}

impl Page {
    pub const ALL: [Page; 4] = [Page::Setup, Page::Run, Page::History, Page::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Page::Setup => "Setup",
            Page::Run => "Run",
            Page::History => "History",
            Page::Settings => "Settings",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Dependencies
// ---------------------------------------------------------------------------------------------

/// A file dialog request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileRequest {
    pub title: String,
    /// `(name, extensions)`, e.g. `("Config files", ["conf", "txt"])`.
    pub filters: Vec<(String, Vec<String>)>,
    pub directory: Option<PathBuf>,
    pub file_name: Option<String>,
}

impl FileRequest {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }

    pub fn filter(mut self, name: &str, extensions: &[&str]) -> Self {
        self.filters
            .push((name.to_string(), extensions.iter().map(|ext| ext.to_string()).collect()));
        self
    }

    pub fn in_directory(mut self, directory: Option<PathBuf>) -> Self {
        self.directory = directory.filter(|dir| dir.is_dir());
        self
    }

    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = Some(name.into());
        self
    }
}

/// Native file dialogs, behind a seam so tests never open one.
pub trait FileDialogs {
    fn open_file(&self, request: &FileRequest) -> Option<PathBuf>;
    fn save_file(&self, request: &FileRequest) -> Option<PathBuf>;
    fn pick_folder(&self, request: &FileRequest) -> Option<PathBuf>;
}

/// The real dialogs (`rfd`). They are modal to the window, like every native file dialog.
#[derive(Debug, Default, Clone, Copy)]
pub struct RfdDialogs;

impl RfdDialogs {
    fn dialog(request: &FileRequest) -> rfd::FileDialog {
        let mut dialog = rfd::FileDialog::new().set_title(&request.title);
        for (name, extensions) in &request.filters {
            dialog = dialog.add_filter(name, extensions);
        }
        if let Some(directory) = &request.directory {
            dialog = dialog.set_directory(directory);
        }
        if let Some(name) = &request.file_name {
            dialog = dialog.set_file_name(name);
        }
        dialog
    }
}

impl FileDialogs for RfdDialogs {
    fn open_file(&self, request: &FileRequest) -> Option<PathBuf> {
        Self::dialog(request).pick_file()
    }

    fn save_file(&self, request: &FileRequest) -> Option<PathBuf> {
        Self::dialog(request).save_file()
    }

    fn pick_folder(&self, request: &FileRequest) -> Option<PathBuf> {
        Self::dialog(request).pick_folder()
    }
}

/// Dialogs that are always cancelled.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDialogs;

impl FileDialogs for NoDialogs {
    fn open_file(&self, _: &FileRequest) -> Option<PathBuf> {
        None
    }

    fn save_file(&self, _: &FileRequest) -> Option<PathBuf> {
        None
    }

    fn pick_folder(&self, _: &FileRequest) -> Option<PathBuf> {
        None
    }
}

/// How the crawler binary is found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrawlerProbe {
    /// GUI-SPEC §4.1: the configured path, next to the GUI, `PATH`; then `--version`. On a worker.
    Discover,
    /// A fixed answer, without a process (tests).
    Fixed {
        path: Option<PathBuf>,
        version: Option<String>,
    },
}

/// Everything [`App::new`] needs from outside.
pub struct AppDeps {
    pub paths: AppPaths,
    pub schema: OptionSchema,
    pub settings: Settings,
    /// Messages from loading (e.g. a settings file that had to be set aside).
    pub notices: Vec<String>,
    pub secret_store: Box<dyn SecretStore>,
    pub dialogs: Box<dyn FileDialogs>,
    pub crawler_probe: CrawlerProbe,
}

impl AppDeps {
    /// Loads the settings from `paths`; real dialogs and crawler discovery.
    pub fn load(paths: AppPaths, schema: OptionSchema, secret_store: Box<dyn SecretStore>) -> Self {
        let loaded = Settings::load(&paths);
        Self {
            paths,
            schema,
            settings: loaded.value,
            notices: loaded.notice.into_iter().collect(),
            secret_store,
            dialogs: Box::new(RfdDialogs),
            crawler_probe: CrawlerProbe::Discover,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// State types
// ---------------------------------------------------------------------------------------------

/// Where the crawler binary stands (GUI-SPEC §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrawlerStatus {
    Detecting,
    Missing,
    Found {
        path: PathBuf,
        source: Option<CrawlerSource>,
        /// `--version`, or why it is unknown.
        version: Result<String, String>,
    },
}

impl CrawlerStatus {
    pub fn path(&self) -> Option<&Path> {
        match self {
            CrawlerStatus::Found { path, .. } => Some(path),
            _ => None,
        }
    }

    /// The version differs from the linked crawler library (option list may differ).
    pub fn version_mismatch(&self) -> Option<&str> {
        match self {
            CrawlerStatus::Found {
                version: Ok(version), ..
            } if version != siteone_crawler::version::CODE => Some(version),
            _ => None,
        }
    }
}

/// A profile in the profile list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileItem {
    pub slug: String,
    pub name: String,
    /// Why the file cannot be loaded.
    pub error: Option<String>,
}

/// The saved state of the selected profile, to tell unsaved changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    pub config: CrawlConfig,
    pub secrets: SecretValues,
}

/// The run started from this window (live or finished).
pub struct CurrentRun {
    /// The crawler process ([`runner::RunHandle`]; a scripted child in tests).
    pub handle: Box<dyn ChildControl>,
    pub state: RunState,
    pub plan: RunPlan,
    /// The configuration the run was started with.
    pub config: CrawlConfig,
    /// The secrets the run was started with — in memory only, never written. Its follow-up
    /// requests (link texts) and *Run again* use them, not what Setup holds now.
    pub secrets: SecretValues,
    /// The profile selected at start.
    pub profile: Option<String>,
    /// The crawler's `--version` at start (the `runStarted` event has it too).
    pub crawler_version: Option<String>,
    pub started_at: chrono::DateTime<chrono::Local>,
    pub started: Instant,
    /// When the exit was observed.
    pub ended: Option<Instant>,
    /// The result JSON, once the run has ended.
    pub result: ResultState,
    /// The History record as last written to `run.json`.
    pub record: RunRecord,
    /// The external links of the run and their check (once the result JSON is loaded).
    pub links: RunLinks,
    /// Its retry queue and retry results (`retries.json`), merged into `state`.
    pub retries: RetryState,
    /// The automatic external link check was started (or found impossible) for this run.
    pub auto_check: bool,
    /// The end was handled (final record written, result loading started).
    finalized: bool,
}

impl CurrentRun {
    pub fn is_active(&self) -> bool {
        self.state.outcome().is_active()
    }

    /// The run's duration: the crawler's own once it reported it (`runFinished`), else the time
    /// since the start until the exit (or now).
    pub fn elapsed(&self) -> Duration {
        if self.ended.is_some()
            && let Some(finished) = &self.state.finished
            && finished.ms > 0
        {
            return Duration::from_millis(finished.ms);
        }
        self.ended
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.started)
    }

    pub fn url(&self) -> &str {
        &self.config.url
    }
}

/// What the Run page shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ViewedRun {
    /// The run started from this window ([`App::current_run`]), or the empty state.
    #[default]
    Current,
    /// A run from History ([`App::past_run`]).
    Past { run_dir: PathBuf },
}

/// What a dialog button does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    LoadProfile(String),
    ApplyPreset(usize),
    /// Saves under the name in the dialog's input.
    SaveProfileAs,
    /// Saves under this name, replacing the profile of the same slug.
    OverwriteProfile(String),
    /// Renames the selected profile to the name in the input.
    RenameProfile,
    DeleteProfile,
    /// Opens the file dialog of the config-file import.
    ImportConfigFile,
    /// Opens the dialog to paste a command line.
    PasteCommandLine,
    /// Imports the command line in the input, read with the quoting rules of this shell.
    ImportCommandLine(Shell),
    /// Exports a config file; the check box says whether secrets are included.
    ExportConfigFile,
    ResetAll,
    ClearCaches,
    LocateCrawler,
    ShowPage(Page),
    /// Kills the running crawler (after the Force quit confirmation).
    ForceQuit,
    /// Kills the running crawler, records the run as aborted and closes the window.
    StopAndClose,
    /// Loads a run's configuration into Setup (after confirming unsaved changes).
    RunAgain(Box<RunAgain>),
    /// Deletes a run folder (History, confirmed).
    DeleteRun(PathBuf),
}

/// Applied to the [`App`] on the UI thread when a background task finishes.
pub type Completion = Box<dyn FnOnce(&mut App) + Send>;

/// Wakes the UI from worker threads (filled with the `egui::Context` on the first frame).
#[derive(Clone, Default)]
pub struct Waker(Arc<Mutex<Option<egui::Context>>>);

impl Waker {
    pub fn wake(&self) {
        if let Ok(guard) = self.0.lock()
            && let Some(ctx) = guard.as_ref()
        {
            ctx.request_repaint();
        }
    }

    fn set(&self, ctx: &egui::Context) {
        if let Ok(mut guard) = self.0.lock()
            && guard.is_none()
        {
            *guard = Some(ctx.clone());
        }
    }
}

struct Background {
    tx: Sender<Completion>,
    rx: Receiver<Completion>,
    pending: usize,
}

// ---------------------------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------------------------

pub struct App {
    pub paths: AppPaths,
    pub schema: OptionSchema,
    pub presets: Vec<Preset>,
    pub settings: Settings,
    pub secret_store: Arc<dyn SecretStore>,
    pub dialogs: Box<dyn FileDialogs>,
    pub page: Page,
    /// The working configuration and the selected profile (autosaved).
    pub session: Session,
    /// The working secrets (autosaved to the credential store, session scope).
    pub secrets: SecretValues,
    pub profiles: Vec<ProfileItem>,
    pub baseline: Option<Baseline>,
    pub crawler: CrawlerStatus,
    pub current_run: Option<CurrentRun>,
    /// The external link check that is running (at most one at a time, for any run).
    pub link_check: Option<ActiveCheck>,
    /// The fetch of source pages for link texts that is running (one at a time, for any run).
    pub text_job: Option<TextJob>,
    /// The retry pass that is running (one at a time, for any run).
    pub retry: Option<ActiveRetry>,
    /// Starts the backend of a pass instead of [`crate::backend::retry::start`] (UI tests).
    pub retry_spawner: Option<RetrySpawner>,
    /// Orders the writes of a run's files from background tasks (the newest wins).
    latest_writes: retry::LatestWrites,
    /// Link texts are read from the source pages (tests whose runs name real sites turn it off).
    pub fetch_link_texts: bool,
    pub viewed_run: ViewedRun,
    /// The run opened from History (when [`Self::viewed_run`] is `Past`).
    pub past_run: Option<PastRun>,
    pub toasts: Toasts,
    pub dialog: Option<Dialog<Action>>,
    pub setup: setup::SetupUi,
    pub settings_ui: settings_page::SettingsUi,
    pub run_ui: run::RunUi,
    pub history_ui: history::HistoryUi,
    /// The user confirmed closing the window during a run: the next close request goes through.
    pub close_confirmed: bool,
    /// Send the close command at the end of this frame.
    close_now: bool,
    /// Text to put on the clipboard at the end of this frame.
    pub pending_copy: Option<String>,
    past_generation: u64,
    /// `Ctrl+F` was pressed: the page focuses its search field.
    pub focus_search: bool,
    background: Background,
    waker: Waker,
    probe: CrawlerProbe,
    crawler_generation: u64,
    session_autosave: Autosave<(Session, SecretValues)>,
    settings_autosave: Autosave<Settings>,
    applied_appearance: Option<(Theme, f32)>,
    /// [`Self::logic`] ran for the frame [`Self::show`] is about to draw.
    logic_done: bool,
}

impl App {
    pub fn new(deps: AppDeps) -> Self {
        let AppDeps {
            paths,
            schema,
            settings,
            notices,
            secret_store,
            dialogs,
            crawler_probe,
        } = deps;
        let secret_store: Arc<dyn SecretStore> = Arc::from(secret_store);
        let mut toasts = Toasts::default();
        for notice in notices {
            toasts.warning(notice);
        }

        let (loaded, dropped) = Session::load(&paths, &schema);
        if let Some(notice) = loaded.notice {
            toasts.warning(notice);
        }
        if !dropped.is_empty() {
            toasts.warning(format!(
                "Some saved settings no longer apply to this crawler version and were dropped: {}.",
                dropped.join("; ")
            ));
        }
        let mut session = loaded.value;
        let (secrets, errors) = SecretValues::load(secret_store.as_ref(), SESSION_SCOPE, schema.secret_options());
        if let Some(error) = errors.first() {
            toasts.warning(format!("Saved passwords could not be read ({error})."));
        }

        let (tx, rx) = mpsc::channel();
        let presets = presets::presets(&schema);
        let mut app = Self {
            presets,
            settings: settings.clone(),
            secret_store,
            dialogs,
            page: Page::Setup,
            session: session.clone(),
            secrets: secrets.clone(),
            profiles: Vec::new(),
            baseline: None,
            crawler: CrawlerStatus::Detecting,
            current_run: None,
            link_check: None,
            text_job: None,
            retry: None,
            retry_spawner: None,
            latest_writes: retry::LatestWrites::default(),
            fetch_link_texts: true,
            viewed_run: ViewedRun::Current,
            past_run: None,
            toasts,
            dialog: None,
            setup: setup::SetupUi::default(),
            settings_ui: settings_page::SettingsUi::default(),
            run_ui: run::RunUi::default(),
            history_ui: history::HistoryUi::default(),
            close_confirmed: false,
            close_now: false,
            pending_copy: None,
            past_generation: 0,
            focus_search: false,
            background: Background { tx, rx, pending: 0 },
            waker: Waker::default(),
            probe: crawler_probe,
            crawler_generation: 0,
            session_autosave: Autosave::new((session.clone(), secrets)),
            settings_autosave: Autosave::new(settings),
            applied_appearance: None,
            logic_done: false,
            paths,
            schema,
        };
        app.refresh_profiles();
        if let Some(slug) = session.profile.clone() {
            match app.load_baseline(&slug) {
                Ok(()) => {}
                Err(error) => {
                    app.toasts.warning(format!(
                        "The profile “{slug}” of the last session could not be loaded ({error}). The configuration is kept without a profile."
                    ));
                    session.profile = None;
                    app.session.profile = None;
                }
            }
        }
        app.detect_crawler();
        app
    }

    // -----------------------------------------------------------------------------------------
    // Frame
    // -----------------------------------------------------------------------------------------

    /// The work of a frame that draws nothing: background completions, the messages of the run
    /// (its end is recorded here), a close request during a run. eframe calls it through
    /// [`eframe::App::logic`] before every frame and also while the window is minimized or
    /// covered, when it does not call [`Self::show`].
    pub fn logic(&mut self, ctx: &egui::Context) {
        self.logic_done = true;
        self.tick(ctx);
    }

    fn tick(&mut self, ctx: &egui::Context) {
        self.waker.set(ctx);
        self.drain_background();
        self.pump_run(ctx);
        self.pump_link_check(ctx);
        self.auto_check_links();
        self.pump_text_job(ctx);
        self.auto_link_texts();
        self.pump_retry(ctx);
        self.handle_close_request(ctx);
    }

    /// Draws one frame (after [`Self::logic`], which it runs itself when it was not called, as in
    /// the UI tests).
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !std::mem::take(&mut self.logic_done) {
            self.tick(&ctx);
        }
        theme::install_fonts(&ctx);
        self.apply_appearance(&ctx);
        if self.dialog.is_none() {
            self.handle_shortcuts(&ctx);
        }

        let p = theme::palette(ui);
        Panel::left("navigation")
            .exact_size(NAV_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(p.surface_alt))
            .show(ui, |ui| nav::show(self, ui));
        CentralPanel::default()
            .frame(Frame::new().fill(p.bg))
            .show(ui, |ui| match self.page {
                Page::Setup => setup::show(self, ui),
                Page::Run => run::show(self, ui),
                Page::History => history::show(self, ui),
                Page::Settings => settings_page::show(self, ui),
            });

        // A page that has a search field took the Ctrl+F request; the others ignore it.
        self.focus_search = false;
        self.show_dialog(&ctx);
        self.toasts.show(&ctx);
        self.autosave(&ctx);
        self.track_window(&ctx);
        if let Some(text) = self.pending_copy.take() {
            ctx.copy_text(text);
        }
        if std::mem::take(&mut self.close_now) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn apply_appearance(&mut self, ctx: &egui::Context) {
        let wanted = (self.settings.theme, self.settings.ui_scale);
        if self.applied_appearance != Some(wanted) {
            theme::apply(ctx, wanted.0, wanted.1);
            self.applied_appearance = Some(wanted);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (pages, start, find) = ctx.input_mut(|input| {
            let keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4];
            let page = keys.iter().position(|key| input.consume_key(Modifiers::COMMAND, *key));
            let start = input.consume_key(Modifiers::COMMAND, Key::Enter);
            let find = input.consume_key(Modifiers::COMMAND, Key::F);
            (page, start, find)
        });
        if let Some(index) = pages
            && let Some(page) = Page::ALL.get(index)
        {
            self.page = *page;
        }
        if start {
            self.start_crawl();
        }
        if find {
            self.focus_search = true;
        }
    }

    /// Shows the open dialog and performs the chosen action.
    fn show_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        match dialog.show(ctx) {
            DialogOutcome::Open => self.dialog = Some(dialog),
            DialogOutcome::Dismissed => {}
            DialogOutcome::Chosen(action) => self.perform(action, Some(dialog)),
        }
    }

    /// Opens a dialog (replacing an open one).
    pub fn open_dialog(&mut self, dialog: Dialog<Action>) {
        self.dialog = Some(dialog);
    }

    /// An error dialog with a single OK button.
    pub fn error_dialog(&mut self, title: &str, message: impl Into<String>, detail: Option<String>) {
        let mut dialog = Dialog::new(title).tone(Tone::Danger).message(message).cancel("OK");
        if let Some(detail) = detail {
            dialog = dialog.code(detail);
        }
        self.dialog = Some(dialog);
    }

    /// Performs a dialog action; `dialog` is the dialog it was chosen in (for its input).
    pub fn perform(&mut self, action: Action, dialog: Option<Dialog<Action>>) {
        match action {
            Action::ShowPage(page) => self.page = page,
            Action::LocateCrawler => self.locate_crawler(),
            Action::ClearCaches => settings_page::clear_caches(self),
            Action::ForceQuit => self.force_quit(),
            Action::StopAndClose => self.stop_and_close(),
            Action::RunAgain(run_again) => self.apply_run_again(*run_again),
            Action::DeleteRun(dir) => history::delete(self, dir),
            other => setup::perform(self, other, dialog),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Background tasks
    // -----------------------------------------------------------------------------------------

    /// Runs `work` on a worker thread; its [`Completion`] is applied on the UI thread. A panic in
    /// `work` becomes an error toast.
    pub fn spawn_task<W>(&mut self, work: W)
    where
        W: FnOnce() -> Completion + Send + 'static,
    {
        let tx = self.background.tx.clone();
        let waker = self.waker.clone();
        let spawned = std::thread::Builder::new()
            .name("siteone-gui-task".to_string())
            .spawn(move || {
                let completion = crate::backend::catch_panic(AssertUnwindSafe(work)).unwrap_or_else(|_| {
                    Box::new(|app: &mut App| {
                        app.toasts
                            .error("An internal task failed; see crash.log in the data folder.")
                    })
                });
                let _ = tx.send(completion);
                waker.wake();
            });
        match spawned {
            Ok(_) => self.background.pending += 1,
            Err(error) => self
                .toasts
                .error(format!("A background task could not be started: {error}")),
        }
    }

    /// Number of background tasks that have not completed.
    pub fn pending_tasks(&self) -> usize {
        self.background.pending
    }

    fn drain_background(&mut self) {
        let done: Vec<Completion> = self.background.rx.try_iter().collect();
        for completion in done {
            self.background.pending = self.background.pending.saturating_sub(1);
            completion(self);
        }
    }

    /// Blocks until every background task has completed (tests). Returns `false` on timeout.
    pub fn wait_for_background(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.background.pending > 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.background.rx.recv_timeout(remaining) {
                Ok(completion) => {
                    self.background.pending = self.background.pending.saturating_sub(1);
                    completion(self);
                }
                Err(_) => return false,
            }
        }
        true
    }

    // -----------------------------------------------------------------------------------------
    // Crawler binary
    // -----------------------------------------------------------------------------------------

    /// (Re)detects the crawler binary (GUI-SPEC §4.1).
    pub fn detect_crawler(&mut self) {
        self.crawler_generation += 1;
        let generation = self.crawler_generation;
        match self.probe.clone() {
            CrawlerProbe::Fixed { path, version } => {
                self.crawler = match path {
                    Some(path) => CrawlerStatus::Found {
                        path,
                        source: None,
                        version: version.ok_or_else(|| "not checked".to_string()),
                    },
                    None => CrawlerStatus::Missing,
                };
            }
            CrawlerProbe::Discover => {
                self.crawler = CrawlerStatus::Detecting;
                let configured = self.settings.crawler_path.clone();
                self.spawn_task(move || {
                    let location = paths::locate_crawler(configured.as_deref());
                    let version = location
                        .as_ref()
                        .map(|found| tools::crawler_version(&found.path, tools::VERSION_TIMEOUT));
                    Box::new(move |app: &mut App| app.crawler_detected(generation, location, version))
                });
            }
        }
    }

    fn crawler_detected(
        &mut self,
        generation: u64,
        location: Option<CrawlerLocation>,
        version: Option<Result<String, tools::ToolError>>,
    ) {
        if generation != self.crawler_generation {
            return;
        }
        self.crawler = match location {
            Some(location) => CrawlerStatus::Found {
                path: location.path,
                source: Some(location.source),
                version: match version {
                    Some(Ok(version)) => Ok(version),
                    Some(Err(error)) => Err(error.to_string()),
                    None => Err("not checked".to_string()),
                },
            },
            None => CrawlerStatus::Missing,
        };
    }

    /// Lets the user pick the crawler binary; stores it in the settings and detects again.
    pub fn locate_crawler(&mut self) {
        let request = FileRequest::new("Locate siteone-crawler")
            .filter("Programs", &["exe"])
            .in_directory(self.crawler.path().and_then(Path::parent).map(Path::to_path_buf));
        if let Some(path) = self.dialogs.open_file(&request) {
            self.settings.crawler_path = Some(path);
            self.detect_crawler();
        }
    }

    // -----------------------------------------------------------------------------------------
    // Profiles and the working configuration (helpers shared by the pages)
    // -----------------------------------------------------------------------------------------

    pub fn config(&self) -> &CrawlConfig {
        &self.session.config
    }

    /// The working configuration as a crawler gets it (runs, tools, the command preview and its
    /// copies): a relative input path names a file in the data folder, the app's working directory
    /// (GUI-SPEC §5.2), and is made absolute.
    pub fn run_config(&self) -> CrawlConfig {
        self.session.config.with_absolute_inputs(&self.schema, &self.paths.data)
    }

    /// Reloads the profile list from disk.
    pub fn refresh_profiles(&mut self) {
        self.profiles = profiles::list(&self.paths.profiles_dir())
            .into_iter()
            .map(|entry| match entry.profile {
                Ok(profile) => ProfileItem {
                    slug: entry.slug,
                    name: if profile.name.trim().is_empty() {
                        "(unnamed)".to_string()
                    } else {
                        profile.name
                    },
                    error: None,
                },
                Err(error) => ProfileItem {
                    name: entry.slug.clone(),
                    slug: entry.slug,
                    error: Some(error.to_string()),
                },
            })
            .collect();
    }

    /// Sets the baseline from the saved profile `slug` (its file and its secrets).
    fn load_baseline(&mut self, slug: &str) -> Result<(), String> {
        let (profile, _) =
            profiles::load(&self.paths.profiles_dir(), slug, &self.schema).map_err(|error| error.to_string())?;
        let (secrets, _) = SecretValues::load(self.secret_store.as_ref(), slug, self.schema.secret_options());
        self.baseline = Some(Baseline {
            config: profile.config,
            secrets,
        });
        Ok(())
    }

    /// The name of the selected profile.
    pub fn profile_name(&self) -> Option<&str> {
        let slug = self.session.profile.as_deref()?;
        self.profiles
            .iter()
            .find(|item| item.slug == slug)
            .map(|item| item.name.as_str())
    }

    /// Whether the working configuration has changes that are not saved in a profile. Without a
    /// profile: whether anything differs from the defaults (the URL aside).
    pub fn is_modified(&self) -> bool {
        let config = &self.session.config;
        match (&self.session.profile, &self.baseline) {
            (Some(_), Some(baseline)) => baseline.config != *config || baseline.secrets != self.secrets,
            (Some(_), None) => true,
            (None, _) => {
                !config.values.is_empty()
                    || config.ai_enabled
                    || config.check_external_links
                    || !config.skip_action_links
                    || !self.secrets.is_empty()
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Starting a run
    // -----------------------------------------------------------------------------------------

    pub fn has_active_run(&self) -> bool {
        self.current_run.as_ref().is_some_and(CurrentRun::is_active)
    }

    /// Why Start is not possible now, as the tooltip of the disabled button.
    pub fn start_blocker(&self) -> Option<&'static str> {
        if self.has_active_run() {
            Some("A crawl is running")
        } else if self.crawler.path().is_none() {
            Some(match self.crawler {
                CrawlerStatus::Detecting => "Looking for the crawler…",
                _ => "The crawler program was not found — locate it first",
            })
        } else if self.session.config.url.trim().is_empty() && !self.session.config.values.contains_key("--url-list") {
            Some("Enter the URL of the website to crawl")
        } else {
            None
        }
    }

    /// Start (button, `Enter` in the URL field, `Ctrl+Enter`): completes the URL, validates with
    /// the CLI's parser, creates the run folder, starts the crawler and switches to the Run page.
    /// A validation failure is shown next to the URL field; a spawn error in a dialog.
    pub fn start_crawl(&mut self) {
        if self.has_active_run() {
            return;
        }
        let completed = complete_url(&self.session.config.url);
        if completed != self.session.config.url {
            self.session.config.url = completed;
        }
        self.setup.url_touched = true;
        let Some(binary) = self.crawler.path().map(Path::to_path_buf) else {
            self.setup.start_error =
                Some("The crawler program (siteone-crawler.exe) was not found. Use Locate… to choose it.".to_string());
            return;
        };
        // The URL's shape first: the CLI's own message for it is less specific.
        let url = self.session.config.url.trim().to_string();
        if !url.is_empty()
            && let Err(message) = check_url(&url)
        {
            self.setup.start_error = Some(message);
            return;
        }
        let config = self.run_config();
        let config_file = self.paths.empty_config_file();
        if let Err(message) = config.validate(&self.schema, &self.secrets, &config_file) {
            self.setup.start_error = Some(message);
            return;
        }

        let runs_dir = self.settings.runs_dir(&self.paths);
        let now = chrono::Local::now();
        let (run_id, run_dir) = match paths::create_run_dir(&runs_dir, &paths::run_id_base(&now, &url)) {
            Ok(created) => created,
            Err(error) => {
                self.setup.start_error = Some(format!(
                    "The run folder could not be created in {}: {error}",
                    runs_dir.display()
                ));
                return;
            }
        };
        let plan = run_plan::build_plan(
            &config,
            &self.schema,
            &self.secrets,
            &RunPaths {
                run_id,
                run_dir: run_dir.clone(),
                http_cache_dir: self.paths.http_cache_dir(),
                ai_cache_dir: self.paths.ai_cache_dir(),
                config_file,
            },
        );
        if let Err(message) = plan.validate() {
            remove_dir_quietly(&run_dir);
            self.setup.start_error = Some(message);
            return;
        }
        let request = runner::RunRequest::from_plan(binary.clone(), &plan);
        let waker = self.waker.clone();
        match runner::start(request, move || waker.wake()) {
            Ok(handle) => {
                self.setup.start_error = None;
                self.attach_run(Box::new(handle), plan, config, now);
            }
            Err(error) => {
                remove_dir_quietly(&run_dir);
                self.error_dialog(
                    "The crawl could not be started",
                    format!("Windows could not start {}.", binary.display()),
                    Some(error.to_string()),
                );
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------------------------

    /// Starts the writes of the working configuration and of the settings that are due (GUI-SPEC
    /// §5.6): ordered, retried after a failure, saved only when the write succeeded.
    fn autosave(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let session = (self.session.clone(), self.secrets.clone());
        match self.session_autosave.poll(&session, AUTOSAVE_DELAY, now) {
            Step::Save(ticket) => {
                let paths = self.paths.clone();
                let schema = self.schema.clone();
                let store = Arc::clone(&self.secret_store);
                self.spawn_task(move || {
                    let done = ticket.run(|value| store_session(&paths, &schema, store.as_ref(), value));
                    Box::new(move |app: &mut App| app.session_written(done))
                });
            }
            Step::Wait(wait) => ctx.request_repaint_after(wait),
            Step::Idle => {}
        }
        match self.settings_autosave.poll(&self.settings, AUTOSAVE_DELAY, now) {
            Step::Save(ticket) => {
                let paths = self.paths.clone();
                self.spawn_task(move || {
                    let done = ticket.run(|settings| store_settings(&paths, settings));
                    Box::new(move |app: &mut App| app.settings_written(done))
                });
            }
            Step::Wait(wait) => ctx.request_repaint_after(wait),
            Step::Idle => {}
        }
    }

    fn session_written(&mut self, done: Done<(Session, SecretValues)>) {
        let report = self.session_autosave.finish(done, Instant::now());
        self.report_save(report, "The working configuration");
    }

    fn settings_written(&mut self, done: Done<Settings>) {
        let report = self.settings_autosave.finish(done, Instant::now());
        self.report_save(report, "The settings");
    }

    /// A failure is shown on every failed attempt (the retries are seconds apart), a recovery once.
    fn report_save(&mut self, report: Report, what: &str) {
        match report {
            Report::Nothing => {}
            Report::Failed(error) => self.toasts.error(format!("{error} The app tries again.")),
            Report::Recovered => self.toasts.success(format!("{what} is saved again.")),
        }
    }

    /// Writes what the autosave has not saved yet, synchronously and after every write that has
    /// started (on exit). A write that has not started yet is skipped: this one is newer.
    pub fn flush(&mut self) {
        let session = (self.session.clone(), self.secrets.clone());
        let (paths, schema, store) = (&self.paths, &self.schema, self.secret_store.as_ref());
        let report = self
            .session_autosave
            .flush(&session, |value| store_session(paths, schema, store, value));
        self.report_save(report, "The working configuration");
        let report = self
            .settings_autosave
            .flush(&self.settings, |settings| store_settings(&self.paths, settings));
        self.report_save(report, "The settings");
    }

    /// Remembers the window's position and size (not while minimized or maximized).
    fn track_window(&mut self, ctx: &egui::Context) {
        let (outer, inner, minimized, maximized) = ctx.input(|input| {
            let viewport = input.viewport();
            (
                viewport.outer_rect,
                viewport.inner_rect,
                viewport.minimized.unwrap_or(false),
                viewport.maximized.unwrap_or(false),
            )
        });
        let (Some(outer), Some(inner)) = (outer, inner) else {
            return;
        };
        if minimized || maximized {
            return;
        }
        // The rects are in UI points; the viewport builder takes native points.
        let zoom = ctx.zoom_factor();
        let geometry = WindowGeometry {
            x: (outer.min.x * zoom).round(),
            y: (outer.min.y * zoom).round(),
            width: (inner.width() * zoom).round(),
            height: (inner.height() * zoom).round(),
        };
        if self.settings.window != Some(geometry) && geometry.width >= 200.0 && geometry.height >= 150.0 {
            self.settings.window = Some(geometry);
        }
    }
}

/// Writes the working configuration and its secrets (session scope). Both parts are written even
/// when one fails; a failure fails the whole value, which is then written again whole.
fn store_session(
    paths: &AppPaths,
    schema: &OptionSchema,
    store: &dyn SecretStore,
    (session, secrets): &(Session, SecretValues),
) -> Result<(), String> {
    let file = session
        .save(paths, schema)
        .map_err(|error| format!("The working configuration could not be saved: {error}."));
    let stored = secrets
        .save(store, SESSION_SCOPE, schema.secret_options())
        .map_err(|error| format!("Passwords could not be saved: {error}."));
    match (file, stored) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(file), Err(stored)) => Err(format!("{file} {stored}")),
    }
}

fn store_settings(paths: &AppPaths, settings: &Settings) -> Result<(), String> {
    settings
        .save(paths)
        .map_err(|error| format!("The settings could not be saved: {error}."))
}

/// Removes a folder this window just created (best effort).
pub(crate) fn remove_dir_quietly(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        App::logic(self, ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn on_exit(&mut self) {
        self.flush();
        // A crawl never outlives the GUI (GUI-SPEC §1.4): killed and recorded as aborted.
        self.abort_for_exit();
        // A running link check is stopped; what it found is kept (marked as stopped).
        self.end_link_check_for_exit();
        self.end_text_job_for_exit();
        // A retry pass is stopped; the queue is kept.
        self.end_retry_for_exit();
    }
}

/// A confirmation dialog: `title`, `message`, a `confirm` button doing `action`, and Cancel.
pub fn confirm(
    title: &str,
    message: impl Into<String>,
    confirm: &str,
    kind: ButtonKind,
    action: Action,
) -> Dialog<Action> {
    Dialog::new(title)
        .tone(if kind == ButtonKind::Danger {
            Tone::Warning
        } else {
            Tone::Info
        })
        .message(message)
        .button(confirm, kind, action)
        .cancel("Cancel")
}
