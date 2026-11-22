//! The Run page (GUI-SPEC §6.3): one page for a live run and a finished one (also when opened from
//! History). A header (URL, outcome, elapsed time, the primary action), a banner for failures, the
//! phase stepper with a progress bar, and tabs: Overview, URLs, Findings, Tables, AI, Files,
//! Console.
//!
//! The page draws from a [`RunView`] — borrowed from [`App::current_run`] or from the loaded
//! [`App::past_run`] — and collects what the user asks for as [`RunAction`]s, which are performed
//! after drawing (so drawing never needs the whole `App` mutably).

mod ai;
pub mod broken;
mod console;
mod files;
mod findings;
mod header;
mod overview;
mod tables;
pub mod urls;

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui::{self, CentralPanel, Frame, Margin, Panel, Ui};
use egui_material_icons::{MaterialIcon, icons};

use crate::app::{ActiveCheck, ActiveRetry, App, PastLoad, ResultState, ViewedRun};
use crate::backend::shell;
use crate::model::coverage::CrawlScope;
use crate::model::history::{RecordedOutcome, RunRecord};
use crate::model::link_check::{CheckReport, LinkList, LinkResult, RunLinks};
use crate::model::link_data::{CrawlerRef, LinkGraph};
use crate::model::link_text::LinkTexts;
use crate::model::results::{ResultLoad, Severity};
use crate::model::retry::{self, RetryState};
use crate::model::run_state::{RunOutcome, RunState, StatusClass};
use crate::theme;
use crate::ui::widgets::modal::{ButtonKind, Dialog};
use crate::ui::widgets::tabs::{self, TabSpec};
use crate::ui::widgets::{self, Tone, progress};

/// Horizontal padding of the page.
pub const PAGE_MARGIN: i8 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Overview,
    Urls,
    Broken,
    Findings,
    Tables,
    Ai,
    Files,
    Console,
}

/// UI state of the Run page (filters, sort orders, caches of the shown run).
#[derive(Debug, Default)]
pub struct RunUi {
    pub tab: Tab,
    pub urls: urls::UrlsUi,
    pub broken: broken::BrokenUi,
    pub findings: findings::FindingsUi,
    pub tables: tables::TablesUi,
    pub console: console::ConsoleUi,
    pub files: files::FilesUi,
    /// The score category whose deductions are shown.
    pub expanded_score: Option<String>,
    /// The run folder of the run the state belongs to.
    shown: Option<PathBuf>,
}

impl RunUi {
    /// A new run starts: everything back to the start.
    pub fn new_run(&mut self) {
        *self = Self::default();
    }

    /// Resets the per-run state when another run is shown.
    fn follow(&mut self, run_dir: &Path) {
        if self.shown.as_deref() != Some(run_dir) {
            let tab = self.tab;
            let by_page = self.broken.by_page;
            *self = Self::default();
            // Switching between runs keeps the tab the user is on, and how links are laid out.
            self.tab = tab;
            self.broken.by_page = by_page;
            self.shown = Some(run_dir.to_path_buf());
        }
    }
}

/// What the Run page shows of a run, live or past.
pub struct RunView<'a> {
    pub url: &'a str,
    pub run_dir: &'a Path,
    pub state: &'a RunState,
    pub outcome: RunOutcome,
    /// `None` while the run goes on or the result is being loaded.
    pub result: Option<&'a ResultLoad>,
    pub result_loading: bool,
    pub elapsed: Duration,
    /// When it started, for people.
    pub started_at: Option<String>,
    pub profile: Option<&'a str>,
    /// The current run is still going.
    pub live: bool,
    /// A past run whose record never got an outcome (the app closed during it).
    pub unfinished: bool,
    /// A past run is shown while this window has a current run to go back to.
    pub current_elsewhere: bool,
    /// Its external links and their check.
    pub links: LinksView<'a>,
    /// Its retry queue and retry results.
    pub retry: RetryView<'a>,
}

/// What the Run page shows of a run's retry queue (GUI-SPEC §4.9).
pub struct RetryView<'a> {
    pub state: &'a RetryState,
    /// The URL this run's pass requests now.
    pub current: Option<&'a str>,
    /// `(handled, total)` of this run's pass.
    pub running: Option<(usize, usize)>,
    /// Why no pass can start now.
    pub blocker: Option<&'static str>,
    /// What the run's configuration lacks for retrying crawled URLs.
    pub missing: Option<String>,
    /// The run's HTTP cache is on.
    pub cache_on: bool,
}

impl<'a> RetryView<'a> {
    /// `state` of the run in `run_dir`; `active` is the running pass, if any.
    fn new(
        state: &'a RetryState,
        active: Option<&'a ActiveRetry>,
        run_dir: &Path,
        argv: &[String],
        blocker: Option<&'static str>,
        missing: Option<String>,
    ) -> Self {
        let active = active.filter(|active| active.run_dir == run_dir);
        Self {
            state,
            current: active.and_then(|active| active.current.as_deref()),
            running: active.map(|active| (active.done, active.urls.len())),
            blocker,
            missing,
            cache_on: !retry::cache_off(argv),
        }
    }

    /// The retries gave a crawled URL another answer than the crawl: the crawler's outputs are
    /// older than the app's views.
    pub fn outputs_older(&self) -> bool {
        self.state.crawled_changed()
    }
}

/// Where the external link check of the shown run stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckStatus {
    /// Not asked for, or never run (e.g. the app closed before it started).
    NotRequested,
    /// Asked for; starts once the crawl has ended and its result is loaded (`another`: after the
    /// check of another run).
    Waiting {
        another: bool,
    },
    Running {
        done: usize,
        total: usize,
    },
    Done,
    /// Stopped before every link was checked.
    Stopped,
    /// The check could not run, or its stored results cannot be read.
    Failed(String),
}

/// What the Run page shows of a run's external links.
pub struct LinksView<'a> {
    /// The run's configuration asks for the check.
    pub requested: bool,
    /// The links found (from the result JSON).
    pub list: Option<&'a LinkList>,
    pub report: Option<&'a CheckReport>,
    pub status: CheckStatus,
    /// The links checked: the running check's so far, or the stored ones.
    pub results: &'a [LinkResult],
    /// Why *Check external links now* is not possible now.
    pub blocker: Option<&'static str>,
    /// The link texts read from the source pages.
    pub texts: &'a LinkTexts,
    /// Source pages of this run are being fetched.
    pub texts_pending: bool,
    /// The crawl's scope and limits (from the result JSON).
    pub scope: Option<&'a CrawlScope>,
    /// The crawler's `links` table and link facts (`--link-details`, from the result JSON).
    pub graph: Option<&'a LinkGraph>,
    /// The links whose scheme is a typo of `http`/`https` (from the result JSON).
    pub mistyped: &'a [(String, CrawlerRef)],
}

impl<'a> LinksView<'a> {
    /// `links` of a run; `active` is the running check when it is this run's, `other_running`
    /// whether a check of another run is running.
    pub fn new(
        links: &'a RunLinks,
        active: Option<&'a ActiveCheck>,
        other_running: bool,
        requested: bool,
        waiting: bool,
        blocker: Option<&'static str>,
    ) -> Self {
        let results = link_results(links, active);
        let status = if let Some(active) = active {
            CheckStatus::Running {
                done: active.results.len(),
                total: active.total,
            }
        } else if let Some(report) = &links.report {
            if report.complete {
                CheckStatus::Done
            } else {
                CheckStatus::Stopped
            }
        } else if let Some(error) = &links.error {
            CheckStatus::Failed(error.clone())
        } else if waiting {
            CheckStatus::Waiting { another: other_running }
        } else {
            CheckStatus::NotRequested
        };
        Self {
            requested,
            list: links.list.as_ref(),
            report: links.report.as_ref(),
            status,
            results,
            blocker,
            texts: &links.texts,
            texts_pending: false,
            scope: links.scope.as_ref(),
            graph: links.graph.as_deref(),
            mistyped: &links.mistyped,
        }
    }

    /// Marks the source pages of this run as being fetched.
    pub fn fetching(mut self, pending: bool) -> Self {
        self.texts_pending = pending;
        self
    }

    /// Whether the phase stepper shows the External links step.
    pub fn shows_step(&self) -> bool {
        self.requested || self.report.is_some() || matches!(self.status, CheckStatus::Running { .. })
    }

    /// Links found that no check covers (none, or a stopped one).
    pub fn unchecked(&self) -> usize {
        let found = self.list.map_or(0, |list| list.links.len());
        match self.status {
            CheckStatus::Done | CheckStatus::Running { .. } => 0,
            _ => found.saturating_sub(self.results.len()),
        }
    }

    /// External files of the run, which are not checked.
    pub fn files(&self) -> usize {
        self.list
            .map(|list| list.files)
            .or(self.report.map(|report| report.files_not_checked))
            .unwrap_or(0)
    }

    /// Links left out by the cap, and how many were found.
    pub fn truncated(&self) -> (usize, usize) {
        match (self.list, self.report) {
            (Some(list), _) => (list.truncated, list.found()),
            (None, Some(report)) => (report.truncated, report.total + report.truncated),
            (None, None) => (0, 0),
        }
    }
}

/// The checked links of a run: the running check's so far (`active`, when it is this run's), or
/// the stored ones.
fn link_results<'a>(links: &'a RunLinks, active: Option<&'a ActiveCheck>) -> &'a [LinkResult] {
    match (active, &links.report) {
        (Some(active), _) => &active.results,
        (None, Some(report)) => &report.results,
        (None, None) => &[],
    }
}

/// What the Link checker tab reads for the run on the Run page.
fn shown_from(app: &App) -> Option<broken::ShownFrom<'_>> {
    let active_for = |run_dir: &Path| app.link_check.as_ref().filter(|active| active.run_dir == run_dir);
    let (state, links, retries, run_dir) = match &app.viewed_run {
        ViewedRun::Current => {
            let run = app.current_run.as_ref()?;
            (&run.state, &run.links, &run.retries, run.plan.run_dir.as_path())
        }
        ViewedRun::Past { .. } => match app.past_run.as_ref().map(|past| (past, &past.load)) {
            Some((past, PastLoad::Ready(data))) => (&data.state, &data.links, &data.retries, past.run_dir.as_path()),
            _ => return None,
        },
    };
    Some(broken::ShownFrom {
        urls: &state.urls,
        links: link_results(links, active_for(run_dir)),
        texts: &links.texts,
        graph: links.graph.as_deref(),
        mistyped: &links.mistyped,
        retries,
    })
}

/// The rows the Link checker tab shows for the run on the Run page, as of the last frame.
pub fn broken_rows(app: &App) -> Vec<broken::ShownRow> {
    shown_from(app).map_or_else(Vec::new, |from| app.run_ui.broken.shown(from))
}

/// The groups the Link checker tab shows by page for the run on the Run page, as of the last frame
/// (empty while it is laid out by link): `(page, its rows)`.
pub fn broken_pages(app: &App) -> Vec<(Option<String>, Vec<broken::ShownRow>)> {
    let broken = &app.run_ui.broken;
    if !broken.shows_by_page() {
        return Vec::new();
    }
    shown_from(app).map_or_else(Vec::new, |from| broken.shown_by_page(from))
}

impl RunView<'_> {
    pub fn data(&self) -> Option<&crate::model::results::ResultData> {
        match self.result {
            Some(ResultLoad::Loaded(data)) => Some(data),
            _ => None,
        }
    }

    /// The HTML report written by the run, if any.
    pub fn html_report(&self) -> Option<&Path> {
        self.state
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "html")
            .and_then(|artifact| artifact.path.as_deref())
    }
}

/// What the user asked for on the Run page.
#[derive(Debug, Clone, PartialEq)]
pub enum RunAction {
    Stop,
    ForceQuit,
    RunAgain,
    GoToSetup,
    BackToHistory,
    /// Back from a past run to the run of this window.
    ShowCurrent,
    OpenPath(PathBuf),
    RevealPath(PathBuf),
    OpenUrl(String),
    /// Copies the text; the label names it in the toast.
    Copy {
        text: String,
        what: String,
    },
    ShowFindings(Severity),
    /// The Link checker tab, optionally for one status class.
    ShowBroken(Option<StatusClass>),
    /// The Link checker tab with every external link (checked ones included).
    ShowExternal,
    /// *Check external links now* for the shown run.
    CheckLinks,
    /// Stops the running external link check.
    StopCheck,
    ShowTab(Tab),
    /// *Retry now* for these URLs.
    RetryNow(Vec<String>),
    /// *Add to retry queue*.
    Enqueue(Vec<String>),
    /// *Remove from retry queue*.
    Dequeue(Vec<String>),
    RetryQueue,
    ClearQueue,
    StopRetry,
    /// Writes the link report of the run and opens it (`true`: shows it in Explorer).
    OpenLinkReport(PathBuf, bool),
    /// Lists the pages linking to a URL in a dialog (the URLs tab's row menu).
    ShowLinkingPages(urls::LinkingPages),
    /// Checks which artifact files exist (on a worker).
    CheckFiles {
        run_dir: PathBuf,
        paths: Vec<Option<PathBuf>>,
    },
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let mut actions = Vec::new();
    let mut past_record: Option<RunRecord> = None;
    let blocker = app.link_check_blocker();
    let current_dir = app.current_run.as_ref().map(|run| run.plan.run_dir.clone());
    let past_dir = app.past_run.as_ref().map(|past| past.run_dir.clone());
    let current_retry_blocker = current_dir.as_deref().and_then(|dir| app.retry_blocker(dir));
    let past_retry_blocker = past_dir.as_deref().and_then(|dir| app.retry_blocker(dir));
    let past_missing = past_dir.as_deref().and_then(|dir| app.retry_missing(dir));
    {
        let App {
            current_run,
            past_run,
            run_ui,
            viewed_run,
            focus_search,
            link_check,
            text_job,
            retry,
            ..
        } = app;
        let retry = retry.as_ref();
        let fetching = |run_dir: &Path| text_job.as_ref().is_some_and(|job| job.run_dir == run_dir);
        let link_check = link_check.as_ref();
        // The running check, when it is the check of the run in `run_dir`.
        let active_for = |run_dir: &Path| link_check.filter(|active| active.run_dir == run_dir);
        let focus = std::mem::take(focus_search);
        match viewed_run {
            ViewedRun::Past { .. } => match past_run.as_ref().map(|past| &past.load) {
                Some(PastLoad::Ready(data)) => {
                    past_record = Some(data.record.clone());
                    let run_dir = past_run.as_ref().map_or(Path::new(""), |past| past.run_dir.as_path());
                    let active = active_for(run_dir);
                    let view = RunView {
                        url: &data.record.url,
                        run_dir: past_run.as_ref().map_or(Path::new(""), |past| past.run_dir.as_path()),
                        state: &data.state,
                        outcome: data.state.outcome(),
                        result: Some(&data.result),
                        result_loading: false,
                        elapsed: Duration::from_millis(
                            data.record
                                .duration_ms
                                .or_else(|| data.state.finished.as_ref().map(|finished| finished.ms))
                                .unwrap_or(0),
                        ),
                        started_at: started_label(&data.record.started_at),
                        profile: data.record.profile.as_deref(),
                        live: false,
                        unfinished: data.record.outcome.is_none(),
                        current_elsewhere: current_run.is_some(),
                        links: LinksView::new(
                            &data.links,
                            active,
                            link_check.is_some() && active.is_none(),
                            data.record.config.check_external_links,
                            false,
                            blocker,
                        )
                        .fetching(fetching(run_dir)),
                        retry: RetryView::new(
                            &data.retries,
                            retry,
                            run_dir,
                            &data.record.argv,
                            past_retry_blocker,
                            past_missing.clone(),
                        ),
                    };
                    draw(ui, &view, run_ui, focus, &mut actions);
                }
                Some(PastLoad::Loading) => loading(ui, "Opening the run…"),
                Some(PastLoad::Failed(error)) => {
                    if widgets::empty_state(
                        ui,
                        icons::ICON_BROKEN_IMAGE,
                        "This run cannot be shown",
                        &format!("{error} You can still delete it in History."),
                        Some("Back to History"),
                    ) {
                        actions.push(RunAction::BackToHistory);
                    }
                }
                None => no_run(ui, &mut actions),
            },
            ViewedRun::Current => match current_run.as_ref() {
                Some(run) => {
                    let outcome = run.state.outcome();
                    let live = outcome.is_active();
                    let active = active_for(&run.plan.run_dir);
                    let requested = run.config.check_external_links;
                    let view = RunView {
                        url: run.url(),
                        run_dir: &run.plan.run_dir,
                        state: &run.state,
                        result: run.result.ready(),
                        result_loading: matches!(run.result, ResultState::Loading),
                        outcome,
                        elapsed: run.elapsed(),
                        started_at: Some(run.started_at.format("%d %b %Y, %H:%M").to_string()),
                        profile: run.profile.as_deref(),
                        live,
                        unfinished: false,
                        current_elsewhere: false,
                        links: LinksView::new(
                            &run.links,
                            active,
                            link_check.is_some() && active.is_none(),
                            requested,
                            requested && !run.auto_check,
                            blocker,
                        )
                        .fetching(fetching(&run.plan.run_dir)),
                        retry: RetryView::new(
                            &run.retries,
                            retry,
                            &run.plan.run_dir,
                            &run.plan.argv,
                            current_retry_blocker,
                            None,
                        ),
                    };
                    draw(ui, &view, run_ui, focus, &mut actions);
                    if live {
                        // The elapsed time and the rate change without new messages.
                        ui.ctx().request_repaint_after(Duration::from_millis(500));
                    }
                }
                None => no_run(ui, &mut actions),
            },
        }
    }
    for action in actions {
        perform(app, action, past_record.as_ref());
    }
}

fn started_label(rfc3339: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .ok()
        .map(|time| time.with_timezone(&chrono::Local).format("%d %b %Y, %H:%M").to_string())
}

fn no_run(ui: &mut Ui, actions: &mut Vec<RunAction>) {
    if widgets::empty_state(
        ui,
        icons::ICON_MONITORING,
        "No crawl yet",
        "Enter a website on the Setup page and start a crawl. Its progress, results and reports appear here.",
        Some("Go to Setup"),
    ) {
        actions.push(RunAction::GoToSetup);
    }
}

/// A centred spinner with a line of text.
pub fn loading(ui: &mut Ui, text: &str) {
    let p = theme::palette(ui);
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.3).clamp(24.0, 200.0));
        progress::spinner(ui, 28.0, p.accent);
        ui.add_space(theme::GRID);
        ui.label(widgets::weak(ui, text));
    });
}

fn draw(ui: &mut Ui, view: &RunView<'_>, run_ui: &mut RunUi, focus: bool, actions: &mut Vec<RunAction>) {
    run_ui.follow(view.run_dir);
    let p = theme::palette(ui);
    let uses_ai = view.state.uses_ai();
    if run_ui.tab == Tab::Ai && !uses_ai {
        run_ui.tab = Tab::Overview;
    }
    // Kept up to date every frame (cheap): the tab's badge counts it.
    run_ui.broken.sync(view);
    Panel::top("run-header")
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new().fill(p.bg).inner_margin(Margin {
            left: PAGE_MARGIN,
            right: PAGE_MARGIN,
            top: 16,
            bottom: 0,
        }))
        .show(ui, |ui| {
            header::show(ui, view, actions);
            ui.add_space(theme::GRID * 1.5);
            tab_bar(ui, view, run_ui, uses_ai);
        });
    CentralPanel::default()
        .frame(Frame::new().fill(p.bg))
        .show(ui, |ui| match run_ui.tab {
            Tab::Overview => overview::show(ui, view, run_ui, actions),
            Tab::Urls => urls::show(ui, view, &mut run_ui.urls, focus, actions),
            Tab::Broken => broken::show(ui, view, &mut run_ui.broken, focus, actions),
            Tab::Findings => findings::show(ui, view, &mut run_ui.findings, focus),
            Tab::Tables => tables::show(ui, view, &mut run_ui.tables, focus, actions),
            Tab::Ai => ai::show(ui, view),
            Tab::Files => files::show(ui, view, &mut run_ui.files, actions),
            Tab::Console => console::show(ui, view, &mut run_ui.console, focus, actions),
        });
}

fn tab_bar(ui: &mut Ui, view: &RunView<'_>, run_ui: &mut RunUi, uses_ai: bool) {
    let data = view.data();
    let mut specs = vec![
        TabSpec {
            value: Tab::Overview,
            label: "Overview",
            count: None,
            alert: false,
        },
        TabSpec {
            value: Tab::Urls,
            label: "URLs",
            count: Some(view.state.urls.len() as u64),
            alert: false,
        },
        TabSpec {
            value: Tab::Broken,
            label: "Link checker",
            count: Some(run_ui.broken.broken_total()),
            alert: true,
        },
        TabSpec {
            value: Tab::Findings,
            label: "Findings",
            count: data.map(|data| data.summary.len() as u64),
            alert: false,
        },
        TabSpec {
            value: Tab::Tables,
            label: "Tables",
            count: data.map(|data| data.tables.len() as u64),
            alert: false,
        },
    ];
    if uses_ai {
        specs.push(TabSpec {
            value: Tab::Ai,
            label: "AI",
            count: None,
            alert: false,
        });
    }
    specs.push(TabSpec {
        value: Tab::Files,
        label: "Files",
        count: Some(view.state.artifacts.len() as u64),
        alert: false,
    });
    specs.push(TabSpec {
        value: Tab::Console,
        label: "Console",
        count: None,
        alert: false,
    });
    tabs::tab_bar(ui, &mut run_ui.tab, &specs);
}

fn perform(app: &mut App, action: RunAction, past_record: Option<&RunRecord>) {
    match action {
        RunAction::Stop => app.stop_run(),
        RunAction::ForceQuit => app.request_force_quit(),
        RunAction::RunAgain => match (&app.viewed_run, past_record) {
            (ViewedRun::Past { .. }, Some(record)) => app.run_again_record(record),
            _ => app.run_again_current(),
        },
        RunAction::GoToSetup => app.page = crate::app::Page::Setup,
        RunAction::BackToHistory => app.page = crate::app::Page::History,
        RunAction::ShowCurrent => app.show_current_run(),
        RunAction::OpenPath(path) => {
            if let Err(error) = shell::open_path(&path) {
                app.toasts
                    .error(format!("{} could not be opened: {error}", path.display()));
            }
        }
        RunAction::RevealPath(path) => {
            if let Err(error) = shell::reveal_in_explorer(&path) {
                app.toasts
                    .error(format!("{} could not be shown: {error}", path.display()));
            }
        }
        RunAction::OpenUrl(url) => {
            if let Err(error) = shell::open_url(&url) {
                app.toasts.error(format!("The URL could not be opened: {error}"));
            }
        }
        RunAction::Copy { text, what } => {
            app.toasts.success(format!("Copied {what}."));
            copy_text(app, text);
        }
        RunAction::ShowFindings(severity) => {
            app.run_ui.findings.only(severity);
            app.run_ui.tab = Tab::Findings;
        }
        RunAction::ShowBroken(class) => {
            app.run_ui.broken.filter = broken::BrokenFilter {
                class,
                ..broken::BrokenFilter::default()
            };
            app.run_ui.tab = Tab::Broken;
        }
        RunAction::ShowExternal => {
            app.run_ui.broken.filter = broken::BrokenFilter {
                scope: broken::Scope::AllExternal,
                ..broken::BrokenFilter::default()
            };
            app.run_ui.tab = Tab::Broken;
        }
        RunAction::CheckLinks => app.check_links_now(),
        RunAction::StopCheck => app.stop_link_check(),
        RunAction::ShowTab(tab) => app.run_ui.tab = tab,
        RunAction::CheckFiles { run_dir, paths } => files::check(app, run_dir, paths),
        RunAction::RetryNow(urls) => app.retry_now(&urls),
        RunAction::Enqueue(urls) => app.enqueue_retries(&urls),
        RunAction::Dequeue(urls) => app.dequeue_retries(&urls),
        RunAction::RetryQueue => app.retry_queue(),
        RunAction::ClearQueue => app.clear_retry_queue(),
        RunAction::StopRetry => app.stop_retry(),
        RunAction::OpenLinkReport(run_dir, reveal) => app.open_link_report(run_dir, reveal),
        RunAction::ShowLinkingPages(pages) => show_linking_pages(app, &pages),
    }
}

/// The dialog of *Pages linking here* (the URLs tab's row menu): the pages, Copy list, Close.
pub fn show_linking_pages(app: &mut App, pages: &urls::LinkingPages) {
    let copy = crate::app::Action::CopyText {
        text: pages.copy_text(),
        what: format!("the pages linking to {}", pages.url),
    };
    let mut lines = pages.lines.clone();
    if pages.more > 0 {
        lines.push(broken::more_pages(pages.more));
    }
    app.open_dialog(
        Dialog::new("Pages linking here")
            .message(pages.message())
            .details(lines)
            .button("Copy list", ButtonKind::Secondary, copy)
            .cancel("Close"),
    );
}

/// Puts text on the clipboard (through egui, at the end of the frame).
fn copy_text(app: &mut App, text: String) {
    app.pending_copy = Some(text);
}

/// The tone and icon of an outcome (chip, stepper, History).
pub fn outcome_style(outcome: &RunOutcome) -> (Tone, MaterialIcon) {
    match outcome {
        RunOutcome::Running => (Tone::Accent, icons::ICON_PLAY_CIRCLE),
        RunOutcome::Stopping => (Tone::Warning, icons::ICON_PAUSE_CIRCLE),
        RunOutcome::Finished => (Tone::Success, icons::ICON_CHECK_CIRCLE),
        RunOutcome::Stopped => (Tone::Warning, icons::ICON_STOP_CIRCLE),
        RunOutcome::QualityGateFailed => (Tone::Warning, icons::ICON_RULE),
        RunOutcome::Failed { .. } => (Tone::Danger, icons::ICON_ERROR),
        RunOutcome::NoPagesCrawled => (Tone::Danger, icons::ICON_CLOUD_OFF),
        RunOutcome::ConfigError { .. } => (Tone::Danger, icons::ICON_ERROR),
        RunOutcome::Crashed { .. } => (Tone::Danger, icons::ICON_REPORT),
        RunOutcome::Aborted => (Tone::Warning, icons::ICON_CANCEL),
    }
}

/// The tone and icon of a stored outcome (History).
pub fn recorded_style(outcome: RecordedOutcome) -> (Tone, MaterialIcon) {
    match outcome {
        RecordedOutcome::Finished => outcome_style(&RunOutcome::Finished),
        RecordedOutcome::Stopped => outcome_style(&RunOutcome::Stopped),
        RecordedOutcome::QualityGateFailed => outcome_style(&RunOutcome::QualityGateFailed),
        RecordedOutcome::Failed => outcome_style(&RunOutcome::Failed { error: None }),
        RecordedOutcome::NoPagesCrawled => outcome_style(&RunOutcome::NoPagesCrawled),
        RecordedOutcome::Crashed => outcome_style(&RunOutcome::Crashed { exit_code: None }),
        RecordedOutcome::Aborted => outcome_style(&RunOutcome::Aborted),
    }
}

/// The colour of a severity and its icon (Findings, the overview counts).
pub fn severity_style(severity: Severity) -> (Tone, MaterialIcon) {
    match severity {
        Severity::Critical => (Tone::Danger, icons::ICON_ERROR),
        Severity::Warning => (Tone::Warning, icons::ICON_WARNING),
        Severity::Notice => (Tone::Info, icons::ICON_INFO),
        Severity::Ok => (Tone::Success, icons::ICON_CHECK_CIRCLE),
        Severity::Info => (Tone::Neutral, icons::ICON_LIGHTBULB),
        Severity::Other => (Tone::Neutral, icons::ICON_HELP),
    }
}

/// A scroll area with the page's padding (the tabs that scroll as a whole).
pub fn padded_scroll(ui: &mut Ui, id: &str, add_contents: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::new()
                .inner_margin(Margin {
                    left: PAGE_MARGIN,
                    right: PAGE_MARGIN,
                    top: 16,
                    bottom: 24,
                })
                .show(ui, add_contents);
        });
}

/// A frame with the page's padding for tabs that manage their own scrolling (tables, console).
pub fn padded(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    Frame::new()
        .inner_margin(Margin {
            left: PAGE_MARGIN,
            right: PAGE_MARGIN,
            top: 12,
            bottom: 16,
        })
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            add_contents(ui)
        });
}

/// The explanation of a tab whose data comes from the result JSON, when there is none (yet).
/// Returns `true` when it drew an empty state.
pub fn result_unavailable(ui: &mut Ui, view: &RunView<'_>, icon: MaterialIcon, what: &str) -> bool {
    if view.live {
        widgets::empty_state(
            ui,
            icon,
            &format!("{what} are available when the crawl finishes"),
            "The crawler analyses the pages after crawling them; the results appear here then.",
            None,
        );
        return true;
    }
    if view.result_loading || view.result.is_none() {
        loading(ui, "Reading the results…");
        return true;
    }
    match view.result {
        Some(ResultLoad::Missing) => {
            widgets::empty_state(
                ui,
                icon,
                &format!("No {} for this run", what.to_lowercase()),
                "The run wrote no result file. That happens when a crawl crashes, is force-quit or crawls no page at all.",
                None,
            );
            true
        }
        Some(ResultLoad::Unreadable(error)) => {
            widgets::empty_state(
                ui,
                icons::ICON_BROKEN_IMAGE,
                "The result file cannot be read",
                &format!("{error}. The HTML report and the console may still have the details."),
                None,
            );
            true
        }
        _ => false,
    }
}
