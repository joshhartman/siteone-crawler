//! The live state of a run: a reducer over the events of the stream, the console lines and the
//! process exit. It is a pure function of what it is given (times included), so a finished run is
//! rebuilt from its `events.ndjson` the same way (History).

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::console::{ConsoleBuffer, StyledLine};
use super::events::{self, AiRequest, AiUsage, Event, Issue, RunFinished, RunStarted, UrlEvent};
use super::link_data::{CrawlerRef, Interner};

/// The window of the URLs-per-second rate.
pub const RATE_WINDOW: Duration = Duration::from_secs(5);

/// The AI request log keeps at most this many requests (the oldest are dropped and counted).
pub const AI_LOG_CAPACITY: usize = 100_000;

/// The exit code of a run that crawled no page.
pub const EXIT_NO_PAGES: i32 = 3;
/// The exit code of a configuration error.
pub const EXIT_CONFIG_ERROR: i32 = 101;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PhaseStatus {
    #[default]
    NotStarted,
    Running,
    Finished {
        ms: Option<u64>,
    },
    Failed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Phases {
    pub crawl: PhaseStatus,
    pub ai: PhaseStatus,
    pub analysis: PhaseStatus,
}

/// The content types of the `url` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContentKind {
    Html,
    Js,
    Css,
    Image,
    Audio,
    Video,
    Font,
    Document,
    Json,
    Xml,
    Redirect,
    /// `Other`, and any type this version does not know.
    Other,
}

impl ContentKind {
    pub fn parse(text: &str) -> Self {
        match text {
            "HTML" => Self::Html,
            "JS" => Self::Js,
            "CSS" => Self::Css,
            "Image" => Self::Image,
            "Audio" => Self::Audio,
            "Video" => Self::Video,
            "Font" => Self::Font,
            "Document" => Self::Document,
            "JSON" => Self::Json,
            "XML" => Self::Xml,
            "Redirect" => Self::Redirect,
            _ => Self::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Html => "HTML",
            Self::Js => "JS",
            Self::Css => "CSS",
            Self::Image => "Image",
            Self::Audio => "Audio",
            Self::Video => "Video",
            Self::Font => "Font",
            Self::Document => "Document",
            Self::Json => "JSON",
            Self::Xml => "XML",
            Self::Redirect => "Redirect",
            Self::Other => "Other",
        }
    }
}

/// The class of a status code; negative codes are transport errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StatusClass {
    Success,
    Redirect,
    ClientError,
    ServerError,
    /// Connection error, timeout, anything outside 200–599.
    Error,
}

impl StatusClass {
    pub fn of(status: i64) -> Self {
        match status {
            200..=299 => Self::Success,
            300..=399 => Self::Redirect,
            400..=499 => Self::ClientError,
            500..=599 => Self::ServerError,
            _ => Self::Error,
        }
    }
}

/// One crawled URL, kept compact (100 000 rows must stay cheap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlRow {
    pub url: Box<str>,
    pub status: i32,
    pub kind: ContentKind,
    pub time_ms: u32,
    pub size: u64,
    pub cached: bool,
    /// Link distance from a start URL as known when the URL was crawled (`None` from a crawler
    /// before link data).
    pub depth: Option<u32>,
    /// The page whose reference queued the URL, as the event names it (`None` for a start URL and
    /// from a crawler before link data). Its strings are shared between rows.
    pub link: Option<Box<CrawlerRef>>,
}

impl UrlRow {
    /// A row without link data (tests, scripted runs).
    pub fn new(url: &str, status: i32, kind: ContentKind, time_ms: u32, size: u64, cached: bool) -> Self {
        Self {
            url: url.into(),
            status,
            kind,
            time_ms,
            size,
            cached,
            depth: None,
            link: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiTaskState {
    Started,
    Progress,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiTask {
    pub key: String,
    pub label: String,
    pub done: u64,
    pub total: u64,
    pub state: AiTaskState,
}

/// An artifact with its path resolved against the run's working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactEntry {
    pub kind: String,
    pub label: String,
    /// As announced (a URL for `online`).
    pub raw: String,
    /// The file or directory; `None` for a URL.
    pub path: Option<PathBuf>,
}

/// How a run ended, or where it stands (GUI-SPEC §4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Running,
    Stopping,
    Finished,
    Stopped,
    QualityGateFailed,
    Failed {
        error: Option<String>,
    },
    /// Exit code 3.
    NoPagesCrawled,
    /// Exit code 101 without events; the message is the CLI's `ERROR:` text.
    ConfigError {
        message: Option<String>,
    },
    /// Exited without `runFinished`.
    Crashed {
        exit_code: Option<i32>,
    },
    /// Killed by the user.
    Aborted,
}

impl RunOutcome {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Running | Self::Stopping)
    }

    /// The label of the outcome chip.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Stopping => "Stopping…",
            Self::Finished => "Finished",
            Self::Stopped => "Stopped",
            Self::QualityGateFailed => "Quality gate failed",
            Self::Failed { .. } => "Failed",
            Self::NoPagesCrawled => "No pages crawled",
            Self::ConfigError { .. } => "Configuration error",
            Self::Crashed { .. } => "Crashed",
            Self::Aborted => "Aborted",
        }
    }

    /// Whether the run is kept in History (GUI-SPEC §4.5): all but configuration errors.
    pub fn is_recorded(&self) -> bool {
        !matches!(self, Self::ConfigError { .. })
    }
}

/// Counts per status class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusCounts {
    pub success: u64,
    pub redirect: u64,
    pub client_error: u64,
    pub server_error: u64,
    pub error: u64,
}

impl StatusCounts {
    fn add(&mut self, class: StatusClass) {
        match class {
            StatusClass::Success => self.success += 1,
            StatusClass::Redirect => self.redirect += 1,
            StatusClass::ClientError => self.client_error += 1,
            StatusClass::ServerError => self.server_error += 1,
            StatusClass::Error => self.error += 1,
        }
    }

    fn remove(&mut self, class: StatusClass) {
        let count = match class {
            StatusClass::Success => &mut self.success,
            StatusClass::Redirect => &mut self.redirect,
            StatusClass::ClientError => &mut self.client_error,
            StatusClass::ServerError => &mut self.server_error,
            StatusClass::Error => &mut self.error,
        };
        *count = count.saturating_sub(1);
    }

    /// 4xx, 5xx and transport errors.
    pub fn errors(&self) -> u64 {
        self.client_error + self.server_error + self.error
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunState {
    pub started: Option<RunStarted>,
    pub phases: Phases,
    pub done: u64,
    pub total: u64,
    pub urls: Vec<UrlRow>,
    pub status_counts: StatusCounts,
    pub content_counts: BTreeMap<ContentKind, u64>,
    pub total_bytes: u64,
    time_sum_ms: u64,
    pub max_time_ms: u64,
    /// Arrival times of the URLs of the last [`RATE_WINDOW`].
    recent: VecDeque<Instant>,
    /// AI tasks in the order they started.
    pub ai_tasks: Vec<AiTask>,
    pub ai_requests: VecDeque<AiRequest>,
    pub ai_requests_dropped: u64,
    pub ai_usage: Option<AiUsage>,
    pub artifacts: Vec<ArtifactEntry>,
    pub issues: Vec<Issue>,
    pub console: ConsoleBuffer,
    /// Lines of the stream that were not events.
    pub malformed_events: u64,
    /// Events of a protocol newer than [`events::PROTOCOL`] are still read; this notes it.
    pub newer_protocol: Option<u64>,
    pub finished: Option<RunFinished>,
    /// Set once the process has exited (`Some(None)`: no exit code, e.g. killed).
    pub exit: Option<Option<i32>>,
    pub stop_requested: bool,
    pub killed: bool,
    /// Counts the rows changed in place ([`Self::replace_answer`]); views rebuild on it.
    pub revision: u64,
    /// Shares the referring pages and attributes of the URL rows.
    strings: Interner,
}

impl RunState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one event that arrived at `now`.
    pub fn apply(&mut self, event: Event, now: Instant) {
        match event {
            Event::RunStarted(started) => {
                if started.protocol > events::PROTOCOL {
                    self.newer_protocol = Some(started.protocol);
                }
                self.started = Some(started);
            }
            Event::Phase(phase) => {
                let status = match phase.state.as_str() {
                    "started" => PhaseStatus::Running,
                    "finished" => PhaseStatus::Finished { ms: phase.ms },
                    "failed" => PhaseStatus::Failed,
                    _ => return,
                };
                match phase.name.as_str() {
                    "crawl" => self.phases.crawl = status,
                    "ai" => self.phases.ai = status,
                    "analysis" => self.phases.analysis = status,
                    _ => {}
                }
            }
            Event::Url(url) => self.add_url(url, now),
            Event::AiRequest(request) => {
                if self.ai_requests.len() == AI_LOG_CAPACITY {
                    self.ai_requests.pop_front();
                    self.ai_requests_dropped += 1;
                }
                self.ai_requests.push_back(*request);
            }
            Event::AiProgress(progress) => {
                let state = match progress.state.as_str() {
                    "started" => AiTaskState::Started,
                    "finished" => AiTaskState::Finished,
                    _ => AiTaskState::Progress,
                };
                match self.ai_tasks.iter_mut().find(|task| task.key == progress.task) {
                    Some(task) => {
                        task.label = progress.label;
                        task.done = progress.done;
                        task.total = progress.total;
                        task.state = state;
                    }
                    None => self.ai_tasks.push(AiTask {
                        key: progress.task,
                        label: progress.label,
                        done: progress.done,
                        total: progress.total,
                        state,
                    }),
                }
            }
            Event::AiUsage(usage) => self.ai_usage = Some(usage),
            Event::Artifact(artifact) => {
                let path = (artifact.kind != "online").then(|| self.resolve(&artifact.path));
                self.artifacts.push(ArtifactEntry {
                    kind: artifact.kind,
                    label: artifact.label,
                    raw: artifact.path,
                    path,
                });
            }
            Event::Issue(issue) => self.issues.push(issue),
            Event::RunFinished(finished) => self.finished = Some(finished),
            Event::Unknown => {}
        }
    }

    fn add_url(&mut self, url: UrlEvent, now: Instant) {
        self.done = url.done;
        self.total = url.total.max(url.done);
        let class = StatusClass::of(url.status);
        self.status_counts.add(class);
        let kind = ContentKind::parse(&url.content_type);
        *self.content_counts.entry(kind).or_insert(0) += 1;
        self.total_bytes = self.total_bytes.saturating_add(url.size);
        self.time_sum_ms = self.time_sum_ms.saturating_add(url.time_ms);
        self.max_time_ms = self.max_time_ms.max(url.time_ms);
        self.recent.push_back(now);
        while self
            .recent
            .front()
            .is_some_and(|first| now.saturating_duration_since(*first) > RATE_WINDOW)
        {
            self.recent.pop_front();
        }
        let link = url
            .source
            .as_deref()
            .map(str::trim)
            .filter(|source| !source.is_empty())
            .map(|source| {
                let small = |value: Option<u64>| value.and_then(|value| u32::try_from(value).ok()).filter(|v| *v > 0);
                Box::new(CrawlerRef {
                    source: self.strings.get(source),
                    attr: self.strings.get(url.source_attr.as_deref().unwrap_or("").trim()),
                    text: url
                        .link_text
                        .as_deref()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(Box::from),
                    line: small(url.source_line),
                    column: small(url.source_column),
                })
            });
        self.urls.push(UrlRow {
            url: url.url.into_boxed_str(),
            status: i32::try_from(url.status).unwrap_or(i32::MIN),
            kind,
            time_ms: u32::try_from(url.time_ms).unwrap_or(u32::MAX),
            size: url.size,
            cached: url.cached,
            depth: url.depth.and_then(|depth| u32::try_from(depth).ok()),
            link,
        });
    }

    /// Replaces the answer of URL row `index` (a retry after the crawl, GUI-SPEC §4.9): the row,
    /// the counters by status class and content type, the bytes and the times follow; `done`,
    /// `total` and the rate stay as the crawl left them. Returns whether the row changed.
    pub fn replace_answer(
        &mut self,
        index: usize,
        status: i64,
        content_type: &str,
        time_ms: u64,
        size: u64,
        cached: bool,
    ) -> bool {
        let new = UrlRow::new(
            "",
            i32::try_from(status).unwrap_or(i32::MIN),
            ContentKind::parse(content_type),
            u32::try_from(time_ms).unwrap_or(u32::MAX),
            size,
            cached,
        );
        let Some(row) = self.urls.get_mut(index) else {
            return false;
        };
        if (row.status, row.kind, row.time_ms, row.size, row.cached)
            == (new.status, new.kind, new.time_ms, new.size, new.cached)
        {
            return false;
        }
        // The answer changes; where the URL was found does not.
        let url = std::mem::take(&mut row.url);
        let link = row.link.take();
        let depth = row.depth;
        let old = std::mem::replace(
            row,
            UrlRow {
                url,
                link,
                depth,
                ..new
            },
        );
        self.status_counts.remove(StatusClass::of(i64::from(old.status)));
        self.status_counts.add(StatusClass::of(status));
        if let Some(count) = self.content_counts.get_mut(&old.kind) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.content_counts.remove(&old.kind);
            }
        }
        *self.content_counts.entry(new.kind).or_insert(0) += 1;
        self.total_bytes = self.total_bytes.saturating_sub(old.size).saturating_add(size);
        self.time_sum_ms = self
            .time_sum_ms
            .saturating_sub(u64::from(old.time_ms))
            .saturating_add(u64::from(new.time_ms));
        if u64::from(old.time_ms) == self.max_time_ms {
            self.max_time_ms = self.urls.iter().map(|row| u64::from(row.time_ms)).max().unwrap_or(0);
        } else {
            self.max_time_ms = self.max_time_ms.max(u64::from(new.time_ms));
        }
        self.revision += 1;
        true
    }

    /// Resolves an artifact path against the run's working directory.
    fn resolve(&self, path: &str) -> PathBuf {
        let path = Path::new(path);
        match &self.started {
            Some(started) if path.is_relative() && !started.working_dir.is_empty() => {
                Path::new(&started.working_dir).join(path)
            }
            _ => path.to_path_buf(),
        }
    }

    /// Counts a line of the stream that was not an event.
    pub fn malformed_event(&mut self) {
        self.malformed_events += 1;
    }

    pub fn push_console(&mut self, line: StyledLine) {
        self.console.push(line);
    }

    /// The process exited; `None` when it has no exit code (e.g. killed).
    pub fn process_exited(&mut self, code: Option<i32>) {
        self.exit = Some(code);
    }

    /// Average response time in ms.
    pub fn avg_time_ms(&self) -> Option<f64> {
        let count = self.urls.len();
        (count > 0).then(|| self.time_sum_ms as f64 / count as f64)
    }

    /// URLs per second over the last [`RATE_WINDOW`] before `now`.
    pub fn urls_per_second(&self, now: Instant) -> f64 {
        let in_window = self
            .recent
            .iter()
            .filter(|at| now.saturating_duration_since(**at) <= RATE_WINDOW)
            .count();
        in_window as f64 / RATE_WINDOW.as_secs_f64()
    }

    pub fn uses_ai(&self) -> bool {
        self.phases.ai != PhaseStatus::NotStarted || !self.ai_tasks.is_empty() || self.ai_usage.is_some()
    }

    /// The last `ERROR:` message on the console (what the CLI prints for a configuration error).
    pub fn console_error(&self) -> Option<String> {
        self.console.lines().iter().rev().find_map(|line| {
            let text = line.text();
            let at = text.find("ERROR:")?;
            Some(text[at + "ERROR:".len()..].trim().to_string()).filter(|message| !message.is_empty())
        })
    }

    pub fn outcome(&self) -> RunOutcome {
        let exit_code = self.exit.flatten();
        if let Some(finished) = &self.finished {
            if finished.exit_code == i64::from(EXIT_NO_PAGES) || exit_code == Some(EXIT_NO_PAGES) {
                return RunOutcome::NoPagesCrawled;
            }
            return match finished.outcome.as_str() {
                "success" => RunOutcome::Finished,
                "cancelled" => RunOutcome::Stopped,
                "qualityGate" => RunOutcome::QualityGateFailed,
                _ => RunOutcome::Failed {
                    error: finished.error.clone(),
                },
            };
        }
        match self.exit {
            None if self.stop_requested => RunOutcome::Stopping,
            None => RunOutcome::Running,
            Some(_) if self.killed => RunOutcome::Aborted,
            Some(Some(EXIT_NO_PAGES)) => RunOutcome::NoPagesCrawled,
            Some(Some(EXIT_CONFIG_ERROR)) if self.started.is_none() => RunOutcome::ConfigError {
                message: self.console_error(),
            },
            Some(code) => RunOutcome::Crashed { exit_code: code },
        }
    }

    /// Rebuilds a run from the lines of its `events.ndjson` (History). Malformed lines are counted.
    pub fn replay<'a>(lines: impl IntoIterator<Item = &'a str>, now: Instant) -> Self {
        let mut state = Self::new();
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            match events::parse_line(line) {
                Ok(event) => state.apply(event, now),
                Err(_) => state.malformed_event(),
            }
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::console::Stream;
    use crate::model::test_support::DOC_LINES;

    fn url(done: u64, status: i64, kind: &str, time_ms: u64, size: u64) -> Event {
        Event::Url(UrlEvent {
            url: format!("https://example.com/{done}"),
            status,
            content_type: kind.to_string(),
            time_ms,
            size,
            cached: false,
            done,
            total: done + 5,
            ..UrlEvent::default()
        })
    }

    #[test]
    fn the_documented_stream_builds_the_expected_state() {
        let now = Instant::now();
        let mut lines: Vec<&str> = DOC_LINES.to_vec();
        lines.insert(3, "this is not json");
        let state = RunState::replay(lines, now);
        assert_eq!(
            state.started.as_ref().map(|s| s.working_dir.as_str()),
            Some("/tmp/evdoc")
        );
        assert_eq!(state.phases.crawl, PhaseStatus::Finished { ms: Some(113) });
        assert_eq!((state.done, state.total, state.urls.len()), (2, 2, 1));
        assert_eq!(state.status_counts.success, 1);
        assert_eq!(state.content_counts.get(&ContentKind::Html), Some(&1));
        assert_eq!(state.total_bytes, 172);
        assert_eq!(state.ai_requests.len(), 3);
        assert_eq!(
            state.ai_tasks,
            vec![AiTask {
                key: "seo".to_string(),
                label: "SEO".to_string(),
                done: 2,
                total: 2,
                state: AiTaskState::Finished,
            }]
        );
        assert_eq!(state.ai_usage.as_ref().map(|usage| usage.calls), Some(4));
        assert_eq!(state.artifacts.len(), 1);
        assert_eq!(state.issues.len(), 1);
        assert_eq!(state.malformed_events, 1);
        assert!(state.uses_ai());
        assert_eq!(state.outcome(), RunOutcome::Finished);
    }

    #[test]
    fn counters_rates_and_times_follow_the_url_events() {
        let start = Instant::now();
        let mut state = RunState::new();
        state.apply(url(1, 200, "HTML", 100, 1000), start);
        state.apply(url(2, 404, "HTML", 300, 10), start + Duration::from_secs(1));
        state.apply(url(3, -2, "Image", 5000, 0), start + Duration::from_secs(2));
        state.apply(url(4, 301, "Redirect", 20, 0), start + Duration::from_secs(7));
        state.apply(url(5, 503, "Unheard", 80, 5), start + Duration::from_secs(7));
        assert_eq!(
            state.status_counts,
            StatusCounts {
                success: 1,
                redirect: 1,
                client_error: 1,
                server_error: 1,
                error: 1
            }
        );
        assert_eq!(state.status_counts.errors(), 3);
        assert_eq!(state.content_counts.get(&ContentKind::Other), Some(&1));
        assert_eq!((state.done, state.total), (5, 10));
        assert_eq!(state.max_time_ms, 5000);
        assert_eq!(state.avg_time_ms(), Some(1100.0));
        // In the 5 s before t=7: the URLs of t=2 and t=7 (twice).
        assert_eq!(state.urls_per_second(start + Duration::from_secs(7)), 3.0 / 5.0);
        assert_eq!(state.urls_per_second(start + Duration::from_secs(60)), 0.0);
        assert_eq!(state.urls[2].status, -2);
    }

    #[test]
    fn url_rows_keep_where_the_crawler_found_them_and_a_retry_keeps_it_too() {
        use crate::model::test_support::DOC_LINK_DETAIL_LINES;
        let now = Instant::now();
        let mut lines: Vec<&str> = DOC_LINES.to_vec();
        lines.extend(DOC_LINK_DETAIL_LINES);
        lines.push(r#"{"type":"url","url":"http://127.0.0.1:8891/","status":200,"depth":0}"#);
        let mut state = RunState::replay(lines, now);
        let rows = &state.urls;
        // Without link details: the page and how it referenced the URL.
        let first = rows[0].link.as_deref().expect("a source");
        assert_eq!(
            (&*first.source, &*first.attr, first.text.as_deref(), first.line),
            ("http://127.0.0.1:8888/", "<a href>", None, None)
        );
        assert_eq!(rows[0].depth, Some(1));
        // With: the link text and where the page references it.
        let page = rows[1].link.as_deref().expect("a source");
        assert_eq!(
            (page.text.as_deref(), page.line, page.column),
            (Some("article 141"), Some(45), Some(257))
        );
        let image = rows[2].link.as_deref().expect("a source");
        assert_eq!(
            (
                &*image.attr,
                image.text.as_deref(),
                image.position().map(|p| p.suffix())
            ),
            ("css url()", None, Some(":1".to_string()))
        );
        assert_eq!(rows[2].depth, Some(2));
        // The referring pages are shared, not copied per row.
        assert!(std::sync::Arc::ptr_eq(
            &page.source,
            &state.urls[1].link.as_ref().expect("source").source
        ));
        // A start URL has none.
        assert_eq!((rows[3].link.is_none(), rows[3].depth), (true, Some(0)));
        // An older crawler's event: no link data at all.
        assert!(
            RunState::replay([r#"{"type":"url","url":"/a","status":404}"#], now).urls[0]
                .link
                .is_none()
        );
        assert!(state.replace_answer(1, 404, "HTML", 1, 1, false));
        assert_eq!(
            state.urls[1].link.as_deref().and_then(|link| link.text.as_deref()),
            Some("article 141")
        );
        assert_eq!(state.urls[1].depth, Some(1));
    }

    #[test]
    fn relative_artifact_paths_resolve_against_the_working_dir() {
        let now = Instant::now();
        let state = RunState::replay(
            [
                r#"{"type":"runStarted","protocol":1,"workingDir":"C:\\runs\\r1"}"#,
                r#"{"type":"artifact","kind":"html","label":"HTML report","path":"report.html"}"#,
                r#"{"type":"artifact","kind":"online","label":"Online HTML report","path":"https://crawler.siteone.io/r/x"}"#,
            ],
            now,
        );
        assert_eq!(
            state.artifacts[0].path,
            Some(Path::new("C:\\runs\\r1").join("report.html"))
        );
        assert_eq!(state.artifacts[1].path, None);
    }

    #[test]
    fn outcomes_follow_the_spec_table() {
        let now = Instant::now();
        let finished = |outcome: &str, exit_code: i64| {
            let mut state = RunState::new();
            state.apply(
                Event::RunFinished(RunFinished {
                    outcome: outcome.to_string(),
                    exit_code,
                    error: Some("boom".to_string()),
                    ..RunFinished::default()
                }),
                now,
            );
            state
        };
        assert_eq!(finished("success", 0).outcome(), RunOutcome::Finished);
        assert_eq!(finished("cancelled", 0).outcome(), RunOutcome::Stopped);
        assert_eq!(finished("qualityGate", 10).outcome(), RunOutcome::QualityGateFailed);
        assert_eq!(
            finished("failed", 1).outcome(),
            RunOutcome::Failed {
                error: Some("boom".to_string())
            }
        );
        assert_eq!(finished("failed", 3).outcome(), RunOutcome::NoPagesCrawled);

        let mut running = RunState::new();
        assert_eq!(running.outcome(), RunOutcome::Running);
        running.stop_requested = true;
        assert_eq!(running.outcome(), RunOutcome::Stopping);
        running.killed = true;
        running.process_exited(None);
        assert_eq!(running.outcome(), RunOutcome::Aborted);

        let mut config_error = RunState::new();
        config_error.push_console(StyledLine::plain(
            Stream::Stderr,
            "ERROR: Option --workers (x) must be positive integer",
        ));
        config_error.push_console(StyledLine::plain(Stream::Stderr, "Usage: ..."));
        config_error.process_exited(Some(101));
        assert_eq!(
            config_error.outcome(),
            RunOutcome::ConfigError {
                message: Some("Option --workers (x) must be positive integer".to_string())
            }
        );
        assert!(!config_error.outcome().is_recorded());

        let mut crashed = RunState::new();
        crashed.apply(url(1, 200, "HTML", 1, 1), now);
        crashed.process_exited(Some(-1073741819));
        assert_eq!(
            crashed.outcome(),
            RunOutcome::Crashed {
                exit_code: Some(-1073741819)
            }
        );
        let mut no_pages = RunState::new();
        no_pages.process_exited(Some(3));
        assert_eq!(no_pages.outcome(), RunOutcome::NoPagesCrawled);
    }

    #[test]
    fn the_ai_log_is_bounded() {
        let now = Instant::now();
        let mut state = RunState::new();
        for seq in 0..(AI_LOG_CAPACITY as u64 + 3) {
            state.apply(
                Event::AiRequest(Box::new(AiRequest {
                    seq,
                    ..AiRequest::default()
                })),
                now,
            );
        }
        assert_eq!(state.ai_requests.len(), AI_LOG_CAPACITY);
        assert_eq!(state.ai_requests_dropped, 3);
        assert_eq!(state.ai_requests.front().map(|request| request.seq), Some(3));
    }
}
