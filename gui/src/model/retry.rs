//! The retry queue of a run (GUI-SPEC §4.9): which URLs wait to be requested again, what each
//! retry answered, the file that keeps both (`<run>/retries.json`), the crawler child's argv for a
//! crawled URL, and how the newest answers are merged into what the Run page shows.
//!
//! # Answers
//!
//! A crawled URL is answered by the crawler (its `url` event, and the title from the `seo` table of
//! the child's result JSON); an external link by the app's single-link check (§4.7). The newest
//! answer replaces the old one in the views (the URL row of the run and its counters, or the link's
//! entry of the check). Per URL the first answer — the crawl's or the check's — is kept too: it says
//! what kind of URL it is and whether a retry changed what the crawler's own outputs show.
//!
//! # The child of a crawled URL
//!
//! [`request_options`] takes the run's options that shape a request ([`REQUEST_OPTIONS`]) from the
//! run's argv; [`crawl_argv`] adds what makes the child request exactly that one URL. Found by
//! experiment against a local server (crawler 2.7): `--max-visited-urls=1` with
//! `--disable-all-assets` requests the start URL only (no asset, no link, a redirect is reported as
//! the answer and not followed); `--ignore-robots-txt` skips the robots.txt request;
//! `--analyzer-filter-regex=/^SeoAndOpenGraphAnalyzer$/` keeps only the analyzer that gives the
//! `seo` table (the title) and drops the DNS and SSL/TLS analyzers, which open connections of
//! their own. `--http-cache-ttl=0s` makes the child ignore a stored answer older than one second
//! and store its own.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use siteone_crawler::engine::parsed_url::ParsedUrl;
use siteone_crawler::options::core_options;

use super::link_check::{LinkResult, Outcome};
use super::run_state::{RunState, StatusClass, UrlRow};
use super::storage;

pub const RETRIES_FILE: &str = "retries.json";
pub const SCHEMA_VERSION: u32 = 1;

/// The least time between two requests to one host in a pass.
pub const HOST_GAP_MS: u64 = 500;

/// The crawler's answer to a crawled URL.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CrawledAnswer {
    /// HTTP status; negative without an answer (the crawler's -1, -2, -4).
    pub status: i32,
    /// The crawler's content type (`HTML`, `Image`, …).
    pub content_type: String,
    pub time_ms: u64,
    pub size: u64,
    pub cached: bool,
    /// The `<title>` of an OK HTML answer.
    pub title: Option<String>,
}

impl CrawledAnswer {
    /// The answer of a URL row of the crawl.
    pub fn of_row(row: &UrlRow) -> Self {
        Self {
            status: row.status,
            content_type: row.kind.label().to_string(),
            time_ms: u64::from(row.time_ms),
            size: row.size,
            cached: row.cached,
            title: None,
        }
    }
}

/// One answer to a URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Answer {
    Crawled(CrawledAnswer),
    External(LinkResult),
}

impl Answer {
    /// The URL answered (2xx, 3xx; an external link classified OK).
    pub fn is_ok(&self) -> bool {
        match self {
            Answer::Crawled(answer) => matches!(
                StatusClass::of(i64::from(answer.status)),
                StatusClass::Success | StatusClass::Redirect
            ),
            Answer::External(link) => link.outcome == Outcome::Ok,
        }
    }

    /// The HTTP status, or a negative number without an answer.
    pub fn code(&self) -> i64 {
        match self {
            Answer::Crawled(answer) => i64::from(answer.status),
            Answer::External(link) => link.status.map_or(-1, i64::from),
        }
    }

    pub fn title(&self) -> Option<&str> {
        match self {
            Answer::Crawled(answer) => answer.title.as_deref(),
            Answer::External(link) => link.title.as_deref(),
        }
    }
}

/// What is known about one URL that was queued or retried. Fields of older files that are no longer
/// kept (`retriedAt`, `retries`) are ignored on load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryEntry {
    pub url: String,
    /// The crawl's or the check's answer.
    pub first: Answer,
    /// The answer of the last retry that got one.
    #[serde(default)]
    pub newest: Option<Answer>,
    /// Why the last attempt got no answer (cleared by an answer).
    #[serde(default)]
    pub error: Option<String>,
}

impl RetryEntry {
    pub fn is_crawled(&self) -> bool {
        matches!(self.first, Answer::Crawled(_))
    }

    /// The answer the views show: the newest, else the first.
    pub fn current(&self) -> &Answer {
        self.newest.as_ref().unwrap_or(&self.first)
    }
}

/// How the last pass ended (not stored).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PassSummary {
    /// URLs the pass took.
    pub total: usize,
    /// Answered without an error.
    pub ok: usize,
    /// Answered with an error.
    pub failing: usize,
    /// No answer (they stay queued): the distinct reasons, at most five.
    pub not_retried: usize,
    pub reasons: Vec<String>,
    /// Stopped before every URL was requested.
    pub stopped: bool,
}

impl PassSummary {
    pub fn add_reason(&mut self, reason: &str) {
        if self.reasons.len() < 5 && !self.reasons.iter().any(|known| known == reason) {
            self.reasons.push(reason.to_string());
        }
    }
}

/// The queue and the retry results of a run (`retries.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RetryState {
    /// URLs waiting for a pass, in order; each has an entry.
    pub queue: Vec<String>,
    pub entries: Vec<RetryEntry>,
    /// URL → position in `entries`.
    #[serde(skip)]
    index: HashMap<String, usize>,
    /// URL → position in `queue`.
    #[serde(skip)]
    queued: HashMap<String, usize>,
    /// Counts every change (the views rebuild on it).
    #[serde(skip)]
    pub revision: u64,
    #[serde(skip)]
    pub last_pass: Option<PassSummary>,
}

impl RetryState {
    pub fn path(run_dir: &Path) -> PathBuf {
        run_dir.join(RETRIES_FILE)
    }

    /// The stored state of a run; a damaged file is set aside (`*.corrupt-<timestamp>`) and an empty
    /// state used, with a notice for a toast (GUI-SPEC §5.6).
    pub fn load(run_dir: &Path) -> (Self, Option<String>) {
        let loaded = storage::load_or_recover::<RetryState>(&Self::path(run_dir), SCHEMA_VERSION);
        let mut state = loaded.value;
        state.reindex();
        (state, loaded.notice)
    }

    /// Writes the state atomically into the run folder, never re-creating a deleted one.
    pub fn write(&self, run_dir: &Path) -> io::Result<()> {
        storage::write_json_in_existing_dir(&Self::path(run_dir), SCHEMA_VERSION, self)
    }

    /// Rebuilds the lookups; drops duplicate entries and queued URLs without an entry.
    fn reindex(&mut self) {
        self.index.clear();
        let mut kept = Vec::with_capacity(self.entries.len());
        for entry in std::mem::take(&mut self.entries) {
            if !self.index.contains_key(&entry.url) {
                self.index.insert(entry.url.clone(), kept.len());
                kept.push(entry);
            }
        }
        self.entries = kept;
        let index = &self.index;
        let mut seen = std::collections::HashSet::new();
        self.queue
            .retain(|url| index.contains_key(url) && seen.insert(url.clone()));
        self.reindex_queue();
    }

    fn reindex_queue(&mut self) {
        self.queued = self
            .queue
            .iter()
            .enumerate()
            .map(|(position, url)| (url.clone(), position))
            .collect();
        self.revision += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty() && self.entries.is_empty()
    }

    pub fn entry(&self, url: &str) -> Option<&RetryEntry> {
        self.index.get(url).and_then(|at| self.entries.get(*at))
    }

    fn entry_mut(&mut self, url: &str) -> Option<&mut RetryEntry> {
        self.index.get(url).and_then(|at| self.entries.get_mut(*at))
    }

    /// The position of `url` in the queue.
    pub fn queue_position(&self, url: &str) -> Option<usize> {
        self.queued.get(url).copied()
    }

    pub fn is_queued(&self, url: &str) -> bool {
        self.queued.contains_key(url)
    }

    /// Whether a retry gave a crawled URL another status than the crawl did (the crawler's own
    /// outputs then no longer match the app's views).
    pub fn crawled_changed(&self) -> bool {
        self.entries.iter().any(|entry| {
            entry.is_crawled()
                && entry
                    .newest
                    .as_ref()
                    .is_some_and(|newest| newest.code() != entry.first.code())
        })
    }

    /// Makes sure `url` has an entry; `first` is its answer now (used only for a new entry).
    fn ensure_entry(&mut self, url: &str, first: Answer) {
        if !self.index.contains_key(url) {
            self.index.insert(url.to_string(), self.entries.len());
            self.entries.push(RetryEntry {
                url: url.to_string(),
                first,
                newest: None,
                error: None,
            });
        }
    }

    /// Adds `url` at the end of the queue (once); `first` is its answer now. Returns whether it
    /// was added.
    pub fn enqueue(&mut self, url: &str, first: Answer) -> bool {
        self.ensure_entry(url, first);
        if self.is_queued(url) {
            return false;
        }
        self.queue.push(url.to_string());
        self.reindex_queue();
        true
    }

    /// Removes the URLs from the queue (their results stay).
    pub fn dequeue<'a>(&mut self, urls: impl IntoIterator<Item = &'a str>) -> usize {
        let remove: std::collections::HashSet<&str> = urls.into_iter().collect();
        let before = self.queue.len();
        self.queue.retain(|url| !remove.contains(url.as_str()));
        let removed = before - self.queue.len();
        if removed > 0 {
            self.reindex_queue();
        }
        removed
    }

    pub fn clear_queue(&mut self) {
        self.queue.clear();
        self.reindex_queue();
    }

    /// Prepares a URL for a pass: it gets an entry (with `first`, its answer now) — it is not
    /// queued by this.
    pub fn prepare(&mut self, url: &str, first: Answer) {
        self.ensure_entry(url, first);
        self.revision += 1;
    }

    /// A retry answered: the newest answer, out of the queue.
    pub fn record_answer(&mut self, url: &str, answer: Answer) {
        if let Some(entry) = self.entry_mut(url) {
            entry.newest = Some(answer);
            entry.error = None;
        }
        self.dequeue([url]);
        self.revision += 1;
    }

    /// A retry got no answer: the old answer stays, the URL is queued (at the end when it was not)
    /// and shows the reason.
    pub fn record_failure(&mut self, url: &str, reason: &str) {
        let Some(entry) = self.entry_mut(url) else {
            return;
        };
        entry.error = Some(reason.to_string());
        if !self.is_queued(url) {
            self.queue.push(url.to_string());
        }
        self.reindex_queue();
    }

    /// Puts the newest answer of every retried crawled URL into the run's URL rows (and their
    /// counters). Idempotent. Returns how many rows changed.
    pub fn apply_to_state(&self, state: &mut RunState) -> usize {
        let wanted: HashMap<&str, &CrawledAnswer> = self
            .entries
            .iter()
            .filter_map(|entry| match &entry.newest {
                Some(Answer::Crawled(answer)) => Some((entry.url.as_str(), answer)),
                _ => None,
            })
            .collect();
        if wanted.is_empty() {
            return 0;
        }
        let rows: Vec<(usize, &CrawledAnswer)> = state
            .urls
            .iter()
            .enumerate()
            .filter_map(|(index, row)| wanted.get(&*row.url).map(|answer| (index, *answer)))
            .collect();
        let mut changed = 0;
        for (index, answer) in rows {
            if state.replace_answer(
                index,
                i64::from(answer.status),
                &answer.content_type,
                answer.time_ms,
                answer.size,
                answer.cached,
            ) {
                changed += 1;
            }
        }
        changed
    }
}

// ---------------------------------------------------------------------------------------------
// The child's argv
// ---------------------------------------------------------------------------------------------

/// Options of the run that shape how one URL is requested; a retry passes them on as the run had
/// them. The credentials among them ([`CREDENTIALS`]) only to a URL the crawl sent them to.
pub const REQUEST_OPTIONS: &[&str] = &[
    "--device",
    "--user-agent",
    "--timeout",
    "--proxy",
    "--http-auth",
    "--header",
    "--accept-invalid-certs",
    "--accept-encoding",
    "--resolve",
    "--transform-url",
    "--add-random-query-params",
    "--max-url-length",
    "--http-cache-dir",
    "--http-cache-compression",
    "--no-cache",
];

/// Sent by the crawler only within the crawled site (`ParsedUrl::may_send_credentials`).
pub const CREDENTIALS: &[&str] = &["--http-auth", "--header"];

/// Options the retry sets itself ([`crawl_argv`]).
pub const SET_BY_RETRY: &[&str] = &[
    "--url",
    "--max-visited-urls",
    "--disable-all-assets",
    "--ignore-robots-txt",
    "--analyzer-filter-regex",
    "--http-cache-ttl",
    "--events-file",
    "--output",
    "--hide-progress-bar",
    "--output-json-file",
    "--output-html-report",
    "--output-text-file",
];

/// Options of the groups that mix request options with others, which do not shape the request of
/// one URL (what is crawled, how many workers, outputs, diagnostics). A retry leaves them out.
pub const NOT_REQUEST_OPTIONS: &[&str] = &[
    // Basic settings
    "--url-list",
    "--single-page",
    "--max-depth",
    "--help",
    "--version",
    // Resource filtering
    "--disable-javascript",
    "--disable-styles",
    "--disable-fonts",
    "--disable-images",
    "--disable-files",
    "--remove-all-anchor-listeners",
    // Advanced crawler settings
    "--workers",
    "--max-reqs-per-sec",
    "--memory-limit",
    "--allowed-domain-for-external-files",
    "--allowed-domain-for-crawling",
    "--single-foreign-page",
    "--include-regex",
    "--ignore-regex",
    "--regex-filtering-only-for-pages",
    "--remove-query-params",
    "--keep-query-param",
    "--force-relative-urls",
    "--ignore-html-comments",
    // What the crawler records about the links it finds; a retry reads only the answer.
    "--link-details",
    "--max-queue-length",
    "--max-skipped-urls",
    "--max-non200-responses-per-basename",
    // Expert settings
    "--debug",
    "--debug-log-file",
    "--debug-url-regex",
    "--result-storage",
    "--result-storage-dir",
    "--result-storage-compression",
    "--websocket-server",
    "--console-width",
    "--control-stdin",
];

/// Groups none of whose options shapes a request: outputs, exports, analyzers, CI, utility modes,
/// AI and browser rendering. A retry leaves them out whole.
pub const NOT_REQUEST_GROUPS: &[&str] = &[
    core_options::GROUP_OUTPUT_SETTINGS,
    core_options::GROUP_FILE_EXPORT_SETTINGS,
    core_options::GROUP_MAILER_SETTINGS,
    core_options::GROUP_MARKDOWN_EXPORT_SETTINGS,
    core_options::GROUP_OFFLINE_EXPORT_SETTINGS,
    core_options::GROUP_SITEMAP_SETTINGS,
    core_options::GROUP_UPLOAD_SETTINGS,
    core_options::GROUP_FASTEST_ANALYZER,
    core_options::GROUP_SEO_AND_OPENGRAPH_ANALYZER,
    core_options::GROUP_SLOWEST_ANALYZER,
    core_options::GROUP_CI_CD_SETTINGS,
    core_options::GROUP_SERVER_SETTINGS,
    core_options::GROUP_AI_SETTINGS,
    core_options::GROUP_BROWSER,
];

/// The analyzer the child keeps: it gives the `seo` table (the page title) and opens no
/// connection.
pub const ANALYZER_FILTER: &str = "/^SeoAndOpenGraphAnalyzer$/";

/// The option name of an argv element (`--header:=X` → `--header`).
fn option_name(arg: &str) -> &str {
    let name = arg.split_once('=').map_or(arg, |(name, _)| name);
    name.strip_suffix(':').unwrap_or(name)
}

/// Whether the crawl of a site that starts at `site_url` sent its credentials to `url` (the
/// crawler's own rule).
pub fn may_send_credentials(site_url: &str, url: &str) -> bool {
    let Ok(target) = url::Url::parse(url.trim()) else {
        return false;
    };
    let (Some(host), Some(port)) = (target.host_str(), target.port_or_known_default()) else {
        return false;
    };
    ParsedUrl::may_send_credentials(&ParsedUrl::parse(site_url.trim(), None), target.scheme(), host, port)
}

/// The run's request options for a retry of `url`: the elements of `run_argv` (the run's argv,
/// with real values) whose option is in [`REQUEST_OPTIONS`]; credentials only when the crawl of
/// `site_url` would send them to `url`.
pub fn request_options(run_argv: &[String], site_url: &str, url: &str) -> Vec<String> {
    let credentials = may_send_credentials(site_url, url);
    run_argv
        .iter()
        .filter(|arg| {
            let name = option_name(arg);
            REQUEST_OPTIONS.contains(&name) && (credentials || !CREDENTIALS.contains(&name))
        })
        .cloned()
        .collect()
}

/// Where a child writes its events and its result JSON, and the config file it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildFiles {
    pub events: PathBuf,
    pub result: PathBuf,
    /// The app's empty config file ([`crate::model::paths::AppPaths::empty_config_file`]).
    pub config: PathBuf,
}

/// Whether the request options switch the HTTP cache off.
pub fn cache_off(options: &[String]) -> bool {
    options
        .iter()
        .any(|arg| option_name(arg) == "--no-cache" && !arg.ends_with("=false"))
}

/// The child's argv (without the program name) that requests exactly `url` with `options` (from
/// [`request_options`]). With `use_cache` and the run's cache on, the child ignores a stored answer
/// and stores its own (`--http-cache-ttl=0s`); without, the cache is off (`--no-cache`).
pub fn crawl_argv(options: &[String], url: &str, files: &ChildFiles, use_cache: bool) -> Vec<String> {
    let mut argv = Vec::with_capacity(options.len() + 14);
    argv.push(format!("--url={url}"));
    argv.extend(options.iter().cloned());
    if cache_off(options) {
        // As the run had it.
    } else if use_cache {
        argv.push("--http-cache-ttl=0s".to_string());
    } else {
        argv.push("--no-cache=true".to_string());
    }
    argv.extend([
        "--max-visited-urls=1".to_string(),
        "--disable-all-assets=true".to_string(),
        "--ignore-robots-txt=true".to_string(),
        format!("--analyzer-filter-regex={ANALYZER_FILTER}"),
        crate::model::config::config_file_arg(&files.config),
        format!("--events-file={}", files.events.display()),
        "--output=text".to_string(),
        "--hide-progress-bar=true".to_string(),
        format!("--output-json-file={}", files.result.display()),
        "--output-html-report=".to_string(),
        "--output-text-file=".to_string(),
    ]);
    argv
}

/// The least time between two requests to one host: [`HOST_GAP_MS`], more when the run's
/// `--max-reqs-per-sec` (in `run_argv`) asks for it.
pub fn host_gap(run_argv: &[String]) -> std::time::Duration {
    let rate = run_argv
        .iter()
        .find(|arg| option_name(arg) == "--max-reqs-per-sec")
        .and_then(|arg| arg.split_once('=').map(|(_, value)| value.trim()))
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|rate| rate.is_finite() && *rate > 0.0);
    let gap = rate.map_or(0.0, |rate| 1000.0 / rate);
    std::time::Duration::from_millis(HOST_GAP_MS.max(gap.ceil() as u64))
}

/// The run's `--timeout` in seconds (from `run_argv`), else the CLI default.
pub fn request_timeout(run_argv: &[String]) -> u64 {
    run_argv
        .iter()
        .find(|arg| option_name(arg) == "--timeout")
        .and_then(|arg| arg.split_once('=').map(|(_, value)| value.trim()))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::link_check::Failure;
    use crate::model::run_state::ContentKind;
    use crate::model::schema::OptionSchema;

    fn crawled(status: i32) -> Answer {
        Answer::Crawled(CrawledAnswer {
            status,
            content_type: "HTML".to_string(),
            time_ms: 12,
            size: 300,
            ..CrawledAnswer::default()
        })
    }

    #[test]
    fn the_queue_is_ordered_without_duplicates_and_keeps_the_first_answer() {
        let mut state = RetryState::default();
        assert!(state.enqueue("https://a.example/1", crawled(404)));
        assert!(state.enqueue("https://a.example/2", crawled(-2)));
        assert!(!state.enqueue("https://a.example/1", crawled(200)), "once");
        assert_eq!(state.queue, ["https://a.example/1", "https://a.example/2"]);
        assert_eq!(state.queue_position("https://a.example/2"), Some(1));

        state.record_answer("https://a.example/1", crawled(200));
        assert_eq!(state.queue, ["https://a.example/2"], "an answer leaves the queue");
        let entry = state.entry("https://a.example/1").expect("entry");
        assert_eq!(entry.first, crawled(404));
        assert_eq!(entry.newest, Some(crawled(200)));
        assert!(state.crawled_changed());
        // Queued again and answered again: the first answer stays the crawl's.
        state.enqueue("https://a.example/1", crawled(200));
        state.record_answer("https://a.example/1", crawled(404));
        let entry = state.entry("https://a.example/1").expect("entry");
        assert_eq!((entry.first.code(), entry.current().code()), (404, 404));
        assert!(!state.crawled_changed(), "the crawl's answer again");

        // No answer: the old answer stays, the URL stays queued and names the reason.
        state.record_failure("https://a.example/2", "No crawler binary");
        assert_eq!(state.queue, ["https://a.example/2"]);
        let entry = state.entry("https://a.example/2").expect("entry");
        assert_eq!(
            (entry.newest.clone(), entry.error.as_deref()),
            (None, Some("No crawler binary"))
        );
        assert_eq!(state.dequeue(["https://a.example/2"]), 1);
        assert!(state.queue.is_empty() && state.entries.len() == 2);
        state.enqueue("https://a.example/3", crawled(404));
        state.clear_queue();
        assert!(state.queue.is_empty());
    }

    #[test]
    fn the_state_round_trips_and_a_damaged_file_is_set_aside() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let mut state = RetryState::default();
        state.enqueue("https://site.example/gone", crawled(404));
        let link = LinkResult {
            url: "https://other.example/".to_string(),
            sources: vec!["https://site.example/".to_string()],
            count: 1,
            outcome: Outcome::Broken,
            failure: Some(Failure::Timeout),
            ..LinkResult::default()
        };
        state.enqueue(&link.url, Answer::External(link.clone()));
        let ok = LinkResult {
            outcome: Outcome::Ok,
            status: Some(200),
            failure: None,
            title: Some("Other".to_string()),
            ..link.clone()
        };
        state.record_answer(&link.url, Answer::External(ok));
        state.write(tmp.path()).expect("written");
        let text = std::fs::read_to_string(tmp.path().join(RETRIES_FILE)).expect("file");
        assert!(text.contains("\"schemaVersion\": 1"), "{text}");
        let (read, notice) = RetryState::load(tmp.path());
        assert_eq!(notice, None);
        assert_eq!(
            (read.queue.clone(), read.entries.clone()),
            (state.queue.clone(), state.entries.clone())
        );
        assert!(read.is_queued("https://site.example/gone"));
        assert!(read.entry(&link.url).is_some_and(|entry| entry.current().is_ok()));

        // Never into a deleted run folder.
        let gone = tmp.path().join("deleted-run");
        assert_eq!(
            state.write(&gone).expect_err("no folder").kind(),
            io::ErrorKind::NotFound
        );
        assert!(!gone.exists());

        // A damaged file: set aside, an empty state, a notice.
        std::fs::write(tmp.path().join(RETRIES_FILE), "{\"schemaVersion\":1,\"queue\":[").expect("damage");
        let (read, notice) = RetryState::load(tmp.path());
        assert!(read.is_empty());
        assert!(notice.is_some_and(|notice| notice.contains("could not be loaded")));
        let kept = std::fs::read_dir(tmp.path())
            .expect("dir")
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with("retries.json.corrupt-"));
        assert!(kept);
        // A queued URL without an entry, and duplicates, are dropped on load.
        std::fs::write(
            tmp.path().join(RETRIES_FILE),
            r#"{"schemaVersion":1,"queue":["https://x.example/","https://x.example/","https://y.example/"],"entries":[{"url":"https://x.example/","first":{"kind":"crawled","status":404}}]}"#,
        )
        .expect("file");
        let (read, _) = RetryState::load(tmp.path());
        assert_eq!(read.queue, ["https://x.example/"]);
        // A file of an earlier version, with the fields no longer kept, still loads.
        std::fs::write(
            tmp.path().join(RETRIES_FILE),
            r#"{"schemaVersion":1,"queue":[],"entries":[{"url":"https://x.example/","first":{"kind":"crawled","status":404},"newest":{"kind":"crawled","status":200},"retriedAt":"2026-11-19T14:32:00+01:00","retries":2}]}"#,
        )
        .expect("file");
        let (read, notice) = RetryState::load(tmp.path());
        assert_eq!(notice, None);
        let entry = read.entry("https://x.example/").expect("entry");
        assert_eq!((entry.first.code(), entry.current().code()), (404, 200));
    }

    #[test]
    fn newest_answers_replace_the_url_rows_and_their_counters() {
        let mut state = RunState::new();
        let now = std::time::Instant::now();
        for (url, status, kind) in [
            ("https://site.example/", 200, "HTML"),
            ("https://site.example/gone", 404, "HTML"),
            ("https://site.example/slow", -2, "Other"),
        ] {
            state.apply(
                crate::model::events::Event::Url(crate::model::events::UrlEvent {
                    url: url.to_string(),
                    status,
                    content_type: kind.to_string(),
                    time_ms: 100,
                    size: 1_000,
                    cached: false,
                    done: 1,
                    total: 3,
                    ..crate::model::events::UrlEvent::default()
                }),
                now,
            );
        }
        assert_eq!(state.status_counts.errors(), 2);
        let mut retries = RetryState::default();
        retries.enqueue("https://site.example/gone", crawled(404));
        retries.record_answer(
            "https://site.example/gone",
            Answer::Crawled(CrawledAnswer {
                status: 200,
                content_type: "HTML".to_string(),
                time_ms: 40,
                size: 2_000,
                cached: false,
                title: Some("Back".to_string()),
            }),
        );
        let revision = state.revision;
        assert_eq!(retries.apply_to_state(&mut state), 1);
        assert_eq!(retries.apply_to_state(&mut state), 0, "idempotent");
        assert!(state.revision > revision);
        assert_eq!(state.urls[1].status, 200);
        assert_eq!(state.urls[1].kind, ContentKind::Html);
        assert_eq!((state.status_counts.success, state.status_counts.errors()), (2, 1));
        assert_eq!(state.total_bytes, 4_000);
        assert_eq!(state.avg_time_ms(), Some(80.0));
    }

    #[test]
    fn the_child_requests_one_url_with_the_runs_request_options_only() {
        let run_argv: Vec<String> = [
            "--url=https://site.example/",
            "--workers=9",
            "--user-agent=Bot/1 !",
            "--timeout=12",
            "--http-auth=user:secret",
            "--header:=Cookie: session=abc",
            "--offline-export-dir=offline",
            "--ai-actions=seo",
            "--browser=true",
            "--max-reqs-per-sec=1",
            "--http-cache-dir=C:\\data\\cache\\http",
            "--events-file=C:\\run\\events.ndjson",
        ]
        .map(str::to_string)
        .to_vec();
        let options = request_options(&run_argv, "https://site.example/", "https://www.site.example/gone");
        assert_eq!(
            options,
            [
                "--user-agent=Bot/1 !",
                "--timeout=12",
                "--http-auth=user:secret",
                "--header:=Cookie: session=abc",
                "--http-cache-dir=C:\\data\\cache\\http",
            ]
        );
        // Another host: the crawl sent no credentials there, nor does the retry.
        let other = request_options(&run_argv, "https://site.example/", "https://cdn.example/x.png");
        assert!(
            other
                .iter()
                .all(|arg| !arg.contains("secret") && !arg.contains("session"))
        );
        // Plain http after an https start: no credentials either.
        assert!(!may_send_credentials("https://site.example/", "http://site.example/"));

        let tmp = tempfile::tempdir().expect("temp dir");
        let files = ChildFiles {
            events: tmp.path().join("events.ndjson"),
            result: tmp.path().join("result.json"),
            config: crate::model::test_support::empty_config_file().to_path_buf(),
        };
        let argv = crawl_argv(&options, "https://www.site.example/gone", &files, true);
        assert_eq!(argv[0], "--url=https://www.site.example/gone");
        for expected in [
            "--http-cache-ttl=0s",
            "--max-visited-urls=1",
            "--disable-all-assets=true",
            "--ignore-robots-txt=true",
            "--analyzer-filter-regex=/^SeoAndOpenGraphAnalyzer$/",
            "--output-html-report=",
            "--output-text-file=",
        ] {
            assert!(argv.iter().any(|arg| arg == expected), "{expected} in {argv:?}");
        }
        let repeat = crawl_argv(&options, "https://www.site.example/gone", &files, false);
        assert!(repeat.contains(&"--no-cache=true".to_string()));
        assert!(!repeat.iter().any(|arg| arg.starts_with("--http-cache-ttl")));
        // The CLI's parser accepts both.
        let mut checked = argv.clone();
        checked.retain(|arg| !arg.starts_with("--http-cache-dir"));
        checked.push(format!("--http-cache-dir={}", tmp.path().join("cache").display()));
        crate::model::config::check_argv(&checked).expect("valid argv");
        // A run without the cache keeps it off.
        let off = crawl_argv(&["--no-cache=true".to_string()], "https://site.example/", &files, true);
        assert!(!off.iter().any(|arg| arg.starts_with("--http-cache-ttl")));

        assert_eq!(host_gap(&run_argv), std::time::Duration::from_millis(1_000));
        assert_eq!(host_gap(&[]), std::time::Duration::from_millis(HOST_GAP_MS));
        assert_eq!(request_timeout(&run_argv), 12);
    }

    /// Fails when the CLI gains an option this module has not classified: is it a request option
    /// a retry must pass on, or not?
    #[test]
    fn every_option_of_the_cli_is_classified_for_a_retry() {
        let schema = OptionSchema::load();
        let lists = [REQUEST_OPTIONS, SET_BY_RETRY, NOT_REQUEST_OPTIONS, CREDENTIALS];
        for list in lists {
            for name in list {
                assert!(schema.get(name).is_some(), "{name} is no option of the CLI");
            }
        }
        for name in REQUEST_OPTIONS.iter().chain(NOT_REQUEST_OPTIONS) {
            assert!(!SET_BY_RETRY.contains(name), "{name} twice");
        }
        let unclassified: Vec<&str> = schema
            .options()
            .iter()
            .filter(|spec| !NOT_REQUEST_GROUPS.contains(&spec.group.as_str()))
            .map(|spec| spec.name.as_str())
            .filter(|name| {
                !REQUEST_OPTIONS.contains(name) && !SET_BY_RETRY.contains(name) && !NOT_REQUEST_OPTIONS.contains(name)
            })
            .collect();
        assert!(
            unclassified.is_empty(),
            "options a retry has not classified: {unclassified:?}"
        );
        for spec in schema.options() {
            if REQUEST_OPTIONS.contains(&spec.name.as_str()) {
                assert!(
                    !NOT_REQUEST_GROUPS.contains(&spec.group.as_str()),
                    "{} is in a group a retry leaves out",
                    spec.name
                );
            }
        }
    }
}
