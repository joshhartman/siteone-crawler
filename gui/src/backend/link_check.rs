//! The external link check (GUI-SPEC §4.7): after a crawl the app — not the crawler — requests
//! each external link of the run once and records how it answered.
//!
//! # Rules
//!
//! - **One logical check per link:** one `GET` that follows redirects (at most
//!   [`MAX_REDIRECTS`]); the chain counts as that one check. Nothing else is requested: not the
//!   page's images, scripts, styles or links.
//! - **Headers:** `User-Agent` ([`user_agent`]: the run's `--user-agent` with its trailing-`!` rule,
//!   else a current desktop Chrome string followed by `SiteOne-Crawler/<version>`) and
//!   `Accept: text/html,*/*` (reqwest adds `Accept-Encoding`). Never the site's `--http-auth`,
//!   `--header` values or cookies (no cookie store).
//! - **Network settings:** the run's `--proxy`; the run's `--timeout`, at least [`MIN_TIMEOUT`],
//!   for the whole request including the body. TLS certificates are always verified: an invalid
//!   certificate is a broken link (TLS error); `--accept-invalid-certs` is for the crawled site.
//! - **Reading the answer:** the final status, the final URL, and only for an OK HTML answer at most
//!   [`MAX_BODY`] bytes of the body (less when `</title` arrives earlier), from which
//!   [`extract_title`] takes the `<title>`.
//! - **Classification:** [`classify_status`] for an answer; without one a [`Failure`]
//!   ([`classify_failure`]: DNS, connection, TLS, timeout, too many redirects, other).
//! - **Politeness:** [`WORKERS`] worker threads; at most one request at a time per host and at
//!   least [`HOST_GAP`] between the requests to one host ([`HostScheduler`]).
//! - **Lifecycle:** [`start`] returns at once; a coordinator thread builds the HTTP client and
//!   starts the workers, which send [`CheckMsg`]s and call the waker. [`CheckHandle::cancel`] (or
//!   dropping the handle) stops them: no new request starts, a running one is abandoned (its body
//!   is not read on) and its result is not sent. A panic in a check is caught (an internal-error
//!   result), so no server behaviour ends the app.

use std::collections::{HashMap, VecDeque};
use std::error::Error as _;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};

use super::catch_panic_unchecked;
use crate::model::config::{CrawlConfig, Value};
use crate::model::link_check::{ExternalLink, Failure, LinkResult, Outcome, classify_status, request_url};
use crate::model::schema::OptionSchema;

/// Worker threads of a check.
pub const WORKERS: usize = 4;
/// The least time between two requests to the same host.
pub const HOST_GAP: Duration = Duration::from_millis(500);
/// The most of a body that is read (for the title).
pub const MAX_BODY: usize = 256 * 1024;
/// Redirects followed per link.
pub const MAX_REDIRECTS: usize = 10;
/// The least timeout of a request, whatever the run's `--timeout` is.
pub const MIN_TIMEOUT: Duration = Duration::from_secs(10);
/// A title longer than this is cut (with `…`).
pub const MAX_TITLE_CHARS: usize = 200;

const ACCEPT_VALUE: &str = "text/html,*/*";
/// How long an idle worker sleeps before it looks for work again (also the cancel latency).
const POLL: Duration = Duration::from_millis(50);
/// Error texts are cut to this many characters.
const MAX_ERROR_CHARS: usize = 300;

/// How a check runs: built from the run's configuration by [`CheckSettings::from_config`]; tests
/// set short timeouts and gaps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckSettings {
    pub user_agent: String,
    /// `host:port` (or a URL), from `--proxy`.
    pub proxy: Option<String>,
    pub timeout: Duration,
    pub workers: usize,
    pub host_gap: Duration,
}

impl CheckSettings {
    /// The settings of a check of a run with `config`: its `--user-agent`, `--proxy` and
    /// `--timeout` (at least [`MIN_TIMEOUT`]). Nothing else of the configuration is used — in
    /// particular no credentials and no headers.
    pub fn from_config(config: &CrawlConfig, schema: &OptionSchema) -> Self {
        let text = |name: &str| match config.get(schema, name) {
            Some(Value::Text(text)) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
            _ => None,
        };
        let timeout = text("--timeout")
            .and_then(|seconds| seconds.parse::<u64>().ok())
            .map_or(MIN_TIMEOUT, |seconds| Duration::from_secs(seconds).max(MIN_TIMEOUT));
        Self {
            user_agent: user_agent(text("--user-agent").as_deref()),
            proxy: text("--proxy"),
            timeout,
            workers: WORKERS,
            host_gap: HOST_GAP,
        }
    }
}

/// The Chrome major version current on `today` (one release every four weeks since 131 on
/// 12 November 2024), so the default User-Agent does not age.
fn chrome_major(today: chrono::NaiveDate) -> i64 {
    let base = chrono::NaiveDate::from_ymd_opt(2024, 11, 12).unwrap_or(today);
    131 + (today - base).num_days().max(0) / 28
}

/// The User-Agent of the check: `custom` (the run's `--user-agent`) with the CLI's rule — a
/// trailing `!` is removed and means "no signature", otherwise ` SiteOne-Crawler/<version>` is
/// added — or a current desktop Chrome string with the signature.
pub fn user_agent(custom: Option<&str>) -> String {
    let signature = format!("SiteOne-Crawler/{}", siteone_crawler::version::CODE);
    match custom.map(str::trim).filter(|custom| !custom.is_empty()) {
        Some(custom) if custom.ends_with('!') => custom.trim_end_matches('!').trim_end().to_string(),
        Some(custom) => format!("{custom} {signature}"),
        None => format!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{}.0.0.0 Safari/537.36 {signature}",
            chrome_major(chrono::Local::now().date_naive())
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// Per-host scheduling
// ---------------------------------------------------------------------------------------------

/// The host a link is throttled under (lower-case; empty for an unparsable URL).
pub fn host_key(url: &str) -> String {
    url::Url::parse(url.trim())
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}

/// Requests in a row without an answer after which a host that has never answered is given up.
/// A failed DNS lookup gives it up at once: the next lookup would fail the same way.
pub const HOST_FAILURES: u32 = 2;

#[derive(Debug, Default)]
struct Health {
    answered: bool,
    failures: u32,
    last: Option<Failure>,
}

/// Remembers which hosts do not answer, so that a dead host is asked [`HOST_FAILURES`] times and
/// not once per link (each of which would wait for the timeout).
///
/// A host is given up when it has never sent an HTTP answer (of any status) and its requests
/// failed for a reason that belongs to the host and not to one URL: DNS, connection, TLS,
/// timeout. A host that has answered once is never given up: a later timeout is then a slow page.
#[derive(Debug, Default)]
pub struct HostHealth {
    hosts: HashMap<String, Health>,
}

impl HostHealth {
    /// A request to `host` got an HTTP answer.
    pub fn answered(&mut self, host: &str) {
        let health = self.hosts.entry(host.to_string()).or_default();
        health.answered = true;
        health.failures = 0;
    }

    /// A request to `host` got no answer.
    pub fn failed(&mut self, host: &str, failure: Failure) {
        if host.is_empty()
            || !matches!(
                failure,
                Failure::Dns | Failure::Connect | Failure::Tls | Failure::Timeout
            )
        {
            return;
        }
        let health = self.hosts.entry(host.to_string()).or_default();
        health.failures += 1;
        health.last = Some(failure);
    }

    /// Records the check of a link to `host`.
    pub fn record(&mut self, host: &str, result: &LinkResult) {
        match (result.status, result.failure) {
            (Some(_), _) => self.answered(host),
            (None, Some(failure)) => self.failed(host, failure),
            (None, None) => {}
        }
    }

    /// Why `host` is not asked any more, if it is given up.
    pub fn given_up(&self, host: &str) -> Option<Failure> {
        let health = self.hosts.get(host)?;
        let failure = health.last?;
        let enough = health.failures >= HOST_FAILURES || failure == Failure::Dns;
        (!health.answered && enough).then_some(failure)
    }
}

/// The error text of a link that was not requested because its host is given up.
pub fn not_requested_text(host: &str, failure: Failure) -> String {
    format!(
        "Not requested: {host} did not answer the earlier requests ({}).",
        failure.label().to_lowercase()
    )
}

/// What the health of a server is kept under: scheme, host and port (lower-case; empty for an
/// unparsable URL). A dead `https://` port says nothing about `http://` on the same host.
pub fn origin_key(url: &str) -> String {
    url::Url::parse(url.trim())
        .ok()
        .filter(|parsed| parsed.host_str().is_some())
        .map(|parsed| parsed.origin().ascii_serialization())
        .unwrap_or_default()
}

#[derive(Debug, Default)]
struct HostState {
    busy: bool,
    ready_at: Option<Instant>,
}

/// What a worker does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    /// Check the link with this index.
    Job(usize),
    /// Do not request the link with this index: its host is given up for this reason.
    Skip(usize, Failure),
    /// Nothing can start now; look again after this long.
    Wait(Duration),
    /// Every link has been handed out.
    Done,
}

/// Hands out links so that a host has at most one request at a time and its requests are at
/// least `gap` apart (from the end of one to the start of the next). Pure: the caller passes the
/// time.
#[derive(Debug)]
pub struct HostScheduler {
    pending: VecDeque<usize>,
    hosts: Vec<String>,
    state: HashMap<String, HostState>,
    /// What the health of the server of each link is kept under (index = link index); the host
    /// where none was given.
    origins: Vec<String>,
    health: HostHealth,
    gap: Duration,
}

impl HostScheduler {
    /// A scheduler for links whose hosts are `hosts` (index = link index), handed out in order.
    pub fn new(hosts: Vec<String>, gap: Duration) -> Self {
        Self {
            pending: (0..hosts.len()).collect(),
            hosts,
            state: HashMap::new(),
            origins: Vec::new(),
            health: HostHealth::default(),
            gap,
        }
    }

    /// Keeps the health of servers by `origins` (index = link index) instead of by host.
    pub fn with_origins(mut self, origins: Vec<String>) -> Self {
        self.origins = origins;
        self
    }

    fn health_key(&self, job: usize) -> &str {
        self.origins
            .get(job)
            .or_else(|| self.hosts.get(job))
            .map_or("", String::as_str)
    }

    pub fn take(&mut self, now: Instant) -> Take {
        if self.pending.is_empty() {
            return Take::Done;
        }
        let given_up = self.pending.iter().enumerate().find_map(|(position, job)| {
            self.health
                .given_up(self.health_key(*job))
                .map(|failure| (position, *job, failure))
        });
        if let Some((position, job, failure)) = given_up {
            self.pending.remove(position);
            return Take::Skip(job, failure);
        }
        let mut wait: Option<Duration> = None;
        let mut chosen = None;
        for (position, job) in self.pending.iter().enumerate() {
            let host = self.hosts.get(*job).map_or("", String::as_str);
            match self.state.get(host) {
                Some(state) if state.busy => {}
                Some(HostState {
                    ready_at: Some(ready), ..
                }) if *ready > now => {
                    let left = *ready - now;
                    wait = Some(wait.map_or(left, |wait: Duration| wait.min(left)));
                }
                _ => {
                    chosen = Some((position, *job, host.to_string()));
                    break;
                }
            }
        }
        match chosen {
            Some((position, job, host)) => {
                self.pending.remove(position);
                self.state.entry(host).or_default().busy = true;
                Take::Job(job)
            }
            None => Take::Wait(wait.unwrap_or(POLL).min(POLL)),
        }
    }

    /// How the check of `job` ended, for the health of its host.
    pub fn record(&mut self, job: usize, result: &LinkResult) {
        let key = self.health_key(job).to_string();
        self.health.record(&key, result);
    }

    /// The check of `job` ended at `now`: its host is free again after the gap.
    pub fn release(&mut self, job: usize, now: Instant) {
        let host = self.hosts.get(job).cloned().unwrap_or_default();
        let state = self.state.entry(host).or_default();
        state.busy = false;
        state.ready_at = Some(now + self.gap);
    }
}

// ---------------------------------------------------------------------------------------------
// Running a check
// ---------------------------------------------------------------------------------------------

/// A message of a running check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckMsg {
    /// One link was checked.
    Checked(LinkResult),
    /// The check cannot run at all (e.g. the proxy setting is invalid). `Finished` follows.
    Failed(String),
    /// Every worker has ended: always the last message (not sent after a cancel).
    Finished,
}

/// A running check. Dropping it cancels the check.
pub struct CheckHandle {
    receiver: Receiver<CheckMsg>,
    cancel: Arc<AtomicBool>,
}

impl CheckHandle {
    /// The next message, without blocking.
    pub fn try_next(&self) -> Option<CheckMsg> {
        self.receiver.try_recv().ok()
    }

    pub fn receiver(&self) -> &Receiver<CheckMsg> {
        &self.receiver
    }

    /// Stops the check: no request starts any more; running ones are abandoned.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for CheckHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct Shared {
    links: Vec<ExternalLink>,
    client: Client,
    scheduler: Mutex<HostScheduler>,
    cancel: Arc<AtomicBool>,
    sender: Sender<CheckMsg>,
    waker: Box<dyn Fn() + Send + Sync>,
    live: AtomicUsize,
}

impl Shared {
    fn scheduler(&self) -> MutexGuard<'_, HostScheduler> {
        self.scheduler.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn send(&self, message: CheckMsg) -> bool {
        let sent = self.sender.send(message).is_ok();
        (self.waker)();
        sent
    }
}

/// Installs the `ring` crypto provider of `rustls` once per process (the crawler's `main.rs` does
/// the same): with more than one provider compiled in, `rustls` does not pick one itself.
pub(crate) fn install_crypto_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The HTTP client of a check (also used by a retry pass for external links).
pub(crate) fn build_client(settings: &CheckSettings) -> Result<Client, String> {
    install_crypto_provider();
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static(ACCEPT_VALUE));
    // A User-Agent that is no valid header value would fail the whole check: use the default.
    let user_agent = if HeaderValue::from_str(&settings.user_agent).is_ok() {
        settings.user_agent.clone()
    } else {
        user_agent(None)
    };
    let mut builder = Client::builder()
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .timeout(settings.timeout)
        .connect_timeout(settings.timeout.min(MIN_TIMEOUT))
        .user_agent(user_agent)
        .default_headers(headers)
        .pool_max_idle_per_host(0);
    if let Some(proxy) = &settings.proxy {
        let url = if proxy.contains("://") {
            proxy.clone()
        } else {
            format!("http://{proxy}")
        };
        let proxy = reqwest::Proxy::all(&url).map_err(|error| format!("The proxy {proxy} cannot be used: {error}"))?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|error| format!("The HTTP client could not be created: {error}"))
}

/// Starts checking `links` on worker threads; returns at once. `waker` is called after every
/// message.
pub fn start(
    links: Vec<ExternalLink>,
    settings: CheckSettings,
    waker: impl Fn() + Send + Sync + 'static,
) -> io::Result<CheckHandle> {
    let (sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let handle = CheckHandle {
        receiver,
        cancel: Arc::clone(&cancel),
    };
    thread::Builder::new()
        .name("siteone-gui-linkcheck".to_string())
        .spawn(move || coordinate(links, settings, cancel, sender, Box::new(waker)))?;
    Ok(handle)
}

/// Builds the client (off the UI thread: it starts a runtime and loads the trust store) and starts
/// the workers.
fn coordinate(
    links: Vec<ExternalLink>,
    settings: CheckSettings,
    cancel: Arc<AtomicBool>,
    sender: Sender<CheckMsg>,
    waker: Box<dyn Fn() + Send + Sync>,
) {
    let client = match catch_panic_unchecked(|| build_client(&settings)) {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => {
            let _ = sender.send(CheckMsg::Failed(error));
            let _ = sender.send(CheckMsg::Finished);
            waker();
            return;
        }
        Err(_) => {
            let _ = sender.send(CheckMsg::Failed("The HTTP client could not be created.".to_string()));
            let _ = sender.send(CheckMsg::Finished);
            waker();
            return;
        }
    };
    let workers = settings.workers.clamp(1, 16).min(links.len().max(1));
    let hosts = links.iter().map(|link| host_key(&link.url)).collect();
    let origins = links.iter().map(|link| origin_key(&link.url)).collect();
    let shared = Arc::new(Shared {
        scheduler: Mutex::new(HostScheduler::new(hosts, settings.host_gap).with_origins(origins)),
        links,
        client,
        cancel,
        sender,
        waker,
        live: AtomicUsize::new(workers),
    });
    for number in 0..workers {
        let worker_shared = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name(format!("siteone-gui-linkcheck-{number}"))
            .spawn(move || work(&worker_shared));
        if spawned.is_err() {
            // One worker fewer; the last one to end reports the end.
            end_worker(&shared);
        }
    }
}

/// A worker has ended; the last one sends [`CheckMsg::Finished`] (unless cancelled).
fn end_worker(shared: &Shared) {
    if shared.live.fetch_sub(1, Ordering::SeqCst) == 1 && !shared.cancelled() {
        shared.send(CheckMsg::Finished);
    }
}

fn work(shared: &Shared) {
    struct Exit<'a>(&'a Shared);
    impl Drop for Exit<'_> {
        fn drop(&mut self) {
            end_worker(self.0);
        }
    }
    let _exit = Exit(shared);
    loop {
        if shared.cancelled() {
            return;
        }
        let take = shared.scheduler().take(Instant::now());
        match take {
            Take::Done => return,
            Take::Wait(wait) => thread::sleep(wait.min(POLL)),
            Take::Skip(index, failure) => {
                let Some(link) = shared.links.get(index) else {
                    continue;
                };
                if !shared.send(CheckMsg::Checked(not_requested(link, failure))) {
                    return;
                }
            }
            Take::Job(index) => {
                let Some(link) = shared.links.get(index) else {
                    shared.scheduler().release(index, Instant::now());
                    continue;
                };
                let result = catch_panic_unchecked(|| check_link(&shared.client, link, &shared.cancel))
                    .unwrap_or_else(|_| internal_error(link));
                {
                    let mut scheduler = shared.scheduler();
                    scheduler.record(index, &result);
                    scheduler.release(index, Instant::now());
                }
                if shared.cancelled() || !shared.send(CheckMsg::Checked(result)) {
                    return;
                }
            }
        }
    }
}

fn base_result(link: &ExternalLink) -> LinkResult {
    LinkResult {
        url: link.url.clone(),
        sources: link.sources.clone(),
        count: link.count,
        ..LinkResult::default()
    }
}

/// The result of a link whose host is given up: broken for the reason of the host, without a
/// request.
pub fn not_requested(link: &ExternalLink, failure: Failure) -> LinkResult {
    LinkResult {
        outcome: Outcome::Broken,
        failure: Some(failure),
        error: Some(not_requested_text(&host_key(&link.url), failure)),
        ..base_result(link)
    }
}

fn internal_error(link: &ExternalLink) -> LinkResult {
    LinkResult {
        outcome: Outcome::Broken,
        failure: Some(Failure::Other),
        error: Some("The check failed with an internal error; see crash.log in the data folder.".to_string()),
        ..base_result(link)
    }
}

/// Checks one link with `client` (one `GET`, redirects followed) — see the module rules.
pub fn check_link(client: &Client, link: &ExternalLink, cancel: &AtomicBool) -> LinkResult {
    let started = Instant::now();
    let mut result = base_result(link);
    let Some(url) = request_url(&link.url) else {
        result.outcome = Outcome::Broken;
        result.failure = Some(Failure::Other);
        result.error = Some("Not an http or https URL.".to_string());
        return result;
    };
    match client.get(&url).send() {
        Ok(mut response) => {
            let status = response.status().as_u16();
            result.status = Some(status);
            result.outcome = classify_status(status);
            if response.url().as_str() != url {
                result.final_url = Some(response.url().to_string());
            }
            if result.outcome == Outcome::Ok && is_html(response.headers()) {
                let body = read_head(&mut response, MAX_BODY, cancel);
                result.title = extract_title(&body);
            }
        }
        Err(error) => {
            let (failure, text) = describe_error(&error);
            result.outcome = Outcome::Broken;
            result.failure = Some(failure);
            result.error = Some(text);
        }
    }
    result.ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    result
}

fn is_html(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("text/html") || value.contains("application/xhtml")
        })
}

/// Reads at most `max` bytes, stopping early once `</title` has arrived, on an error (a slow body
/// runs into the request timeout) or when the check is cancelled.
pub fn read_head(reader: &mut impl Read, max: usize, cancel: &AtomicBool) -> Vec<u8> {
    let mut body = Vec::with_capacity(max.min(32 * 1024));
    let mut buffer = [0u8; 16 * 1024];
    while body.len() < max && !cancel.load(Ordering::SeqCst) {
        let want = (max - body.len()).min(buffer.len());
        let Some(chunk) = buffer.get_mut(..want) else { break };
        match reader.read(chunk) {
            Ok(0) => break,
            Ok(read) => {
                // Look for the end of the title from a little before the new bytes.
                let from = body.len().saturating_sub(8);
                body.extend_from_slice(chunk.get(..read).unwrap_or_default());
                if find_ignore_case(body.get(from..).unwrap_or_default(), b"</title").is_some() {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    body
}

// ---------------------------------------------------------------------------------------------
// Classifying a failed request
// ---------------------------------------------------------------------------------------------

/// The failure and its text (the chain of causes, without the URL) of a request without an answer.
fn describe_error(error: &reqwest::Error) -> (Failure, String) {
    let mut causes: Vec<String> = Vec::new();
    let mut tls = false;
    let mut source = error.source();
    while let Some(cause) = source {
        if cause.downcast_ref::<rustls::Error>().is_some()
            || cause
                .downcast_ref::<io::Error>()
                .and_then(io::Error::get_ref)
                .is_some_and(|inner| inner.downcast_ref::<rustls::Error>().is_some())
        {
            tls = true;
        }
        let text = cause.to_string();
        if !causes.iter().any(|known| known.contains(&text)) {
            causes.push(text);
        }
        source = cause.source();
    }
    let chain = causes.join(": ");
    let failure = classify_failure(
        error.is_redirect(),
        error.is_timeout(),
        error.is_connect(),
        tls,
        &chain.to_lowercase(),
    );
    let text = if chain.is_empty() {
        failure.label().to_string()
    } else {
        chain
    };
    (failure, cut(&text, MAX_ERROR_CHARS))
}

/// The failure of a request without an answer, from reqwest's error kind and the lower-case text
/// of its causes (which never contains the URL).
pub fn classify_failure(redirect: bool, timeout: bool, connect: bool, tls: bool, causes: &str) -> Failure {
    let any = |needles: &[&str]| needles.iter().any(|needle| causes.contains(needle));
    if redirect || any(&["too many redirects", "redirect loop"]) {
        Failure::TooManyRedirects
    } else if timeout || any(&["timed out", "timeout"]) {
        Failure::Timeout
    } else if tls
        || any(&[
            "certificate",
            "tls",
            "ssl",
            "handshake",
            "corrupt message",
            "invalid peer",
            "unknownissuer",
        ])
    {
        Failure::Tls
    } else if any(&[
        "dns error",
        "failed to lookup",
        "no such host",
        "name or service not known",
        "nodename nor servname",
        "getaddrinfo",
    ]) {
        Failure::Dns
    } else if connect
        || any(&[
            "connection refused",
            "connection reset",
            "connection closed",
            "connection aborted",
            "incomplete message",
            "unexpected eof",
            "broken pipe",
            "forcibly closed",
        ])
    {
        Failure::Connect
    } else {
        Failure::Other
    }
}

// ---------------------------------------------------------------------------------------------
// The title
// ---------------------------------------------------------------------------------------------

/// The position of `needle` (ASCII, lower-case) in `haystack`, ignoring ASCII case.
fn find_ignore_case(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// The `<title>` of an HTML document head: lenient decoding (UTF-8, otherwise Latin-1), the common
/// entities decoded, whitespace collapsed, at most [`MAX_TITLE_CHARS`] characters. `None` without
/// a complete, non-empty title element in `body`.
pub fn extract_title(body: &[u8]) -> Option<String> {
    let mut from = 0;
    let content_start = loop {
        let at = from + find_ignore_case(body.get(from..)?, b"<title")?;
        let after = at + b"<title".len();
        match body.get(after) {
            Some(b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/') => {
                let close = body.get(after..)?.iter().position(|byte| *byte == b'>')?;
                break after + close + 1;
            }
            Some(_) => from = after,
            None => return None,
        }
    };
    let rest = body.get(content_start..)?;
    let end = find_ignore_case(rest, b"</title")?;
    let raw = rest.get(..end)?;
    let text = match std::str::from_utf8(raw) {
        Ok(text) => text.to_string(),
        Err(_) => raw.iter().map(|byte| char::from(*byte)).collect(),
    };
    let decoded = decode_entities(&text);
    let collapsed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then(|| cut(&collapsed, MAX_TITLE_CHARS))
}

/// At most `max` characters, the last one `…` when cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// Decodes numeric character references and the common named entities; anything else stays.
pub fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let decoded = tail
            .get(1..tail.len().min(12))
            .and_then(|window| window.find(';'))
            .and_then(|semicolon| {
                let name = &tail[1..=semicolon];
                entity(name).map(|character| (character, semicolon + 2))
            });
        match decoded {
            Some((character, length)) => {
                out.push(character);
                rest = &tail[length..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The character of an entity name (between `&` and `;`).
pub(crate) fn entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse::<u32>().ok()?,
        };
        return Some(char::from_u32(code).filter(|c| *c != '\0').unwrap_or('\u{FFFD}'));
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "middot" => '·',
        "bull" => '•',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "euro" => '€',
        "pound" => '£',
        "times" => '×',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_decoded_collapsed_and_capped() {
        let page = b"<!doctype html><HTML><head><meta charset=utf-8>\n<TITLE lang=en>\n  Caf\xc3\xa9 &amp; Bar &#8211; &#x1F600; &nbsp;&ldquo;Ok&rdquo; &bogus; &\n</Title></head>";
        assert_eq!(extract_title(page).as_deref(), Some("Café & Bar – 😀 “Ok” &bogus; &"));
        // `<titles>` is no title; the real one follows.
        assert_eq!(
            extract_title(b"<titles>x</titles><title>Real</title>").as_deref(),
            Some("Real")
        );
        assert_eq!(extract_title(b"<html><head></head><body>No title</body></html>"), None);
        assert_eq!(extract_title(b"<title>   </title>"), None);
        // Cut by the read limit: no closing tag, no title.
        assert_eq!(extract_title(b"<title>Half a ti"), None);
        // Not UTF-8: read as Latin-1, never a panic.
        assert_eq!(extract_title(b"<title>Caf\xe9 \xff</title>").as_deref(), Some("Café ÿ"));
        let long = format!("<title>{}</title>", "word ".repeat(100));
        let title = extract_title(long.as_bytes()).expect("a title");
        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
        assert!(title.ends_with('…'));
        assert_eq!(decode_entities("&#0;&#xZZ;&amp"), "\u{FFFD}&#xZZ;&amp");
    }

    #[test]
    fn a_title_beyond_the_read_limit_is_not_read() {
        let mut page = b"<html><head>".to_vec();
        page.extend(std::iter::repeat_n(b' ', MAX_BODY));
        page.extend_from_slice(b"<title>Too late</title>");
        let cancel = AtomicBool::new(false);
        let head = read_head(&mut page.as_slice(), MAX_BODY, &cancel);
        assert_eq!(head.len(), MAX_BODY);
        assert_eq!(extract_title(&head), None);
        // Reading stops right after the title.
        let mut page = b"<title>Early</title>".to_vec();
        page.extend(std::iter::repeat_n(b'x', 1_000_000));
        let head = read_head(&mut page.as_slice(), MAX_BODY, &cancel);
        assert!(head.len() <= 16 * 1024, "{}", head.len());
        assert_eq!(extract_title(&head).as_deref(), Some("Early"));
    }

    #[test]
    fn failures_are_classified_from_the_error_kind_and_causes() {
        assert_eq!(
            classify_failure(true, false, false, false, ""),
            Failure::TooManyRedirects
        );
        assert_eq!(
            classify_failure(false, true, true, false, "operation timed out"),
            Failure::Timeout
        );
        assert_eq!(
            classify_failure(false, false, true, true, "received corrupt message"),
            Failure::Tls
        );
        assert_eq!(
            classify_failure(false, false, true, false, "invalid peer certificate: unknownissuer"),
            Failure::Tls
        );
        assert_eq!(
            classify_failure(
                false,
                false,
                true,
                false,
                "dns error: no such host is known. (os error 11001)"
            ),
            Failure::Dns
        );
        assert_eq!(
            classify_failure(false, false, true, false, "tcp connect error: connection refused"),
            Failure::Connect
        );
        assert_eq!(
            classify_failure(false, false, false, false, "connection closed before message completed"),
            Failure::Connect
        );
        assert_eq!(
            classify_failure(false, false, false, false, "something else"),
            Failure::Other
        );
    }

    #[test]
    fn a_host_that_never_answers_is_given_up() {
        let mut health = HostHealth::default();
        // One failed connection is not enough; two are.
        health.failed("http://dead.example", Failure::Connect);
        assert_eq!(health.given_up("http://dead.example"), None);
        health.failed("http://dead.example", Failure::Timeout);
        assert_eq!(health.given_up("http://dead.example"), Some(Failure::Timeout));
        // A failed DNS lookup is enough at once.
        health.failed("http://nowhere.example", Failure::Dns);
        assert_eq!(health.given_up("http://nowhere.example"), Some(Failure::Dns));
        // A host that has answered is never given up: later timeouts are slow pages.
        health.answered("http://slow.example");
        for _ in 0..5 {
            health.failed("http://slow.example", Failure::Timeout);
        }
        assert_eq!(health.given_up("http://slow.example"), None);
        // A failure of the URL, not of the host, does not count; nor does an unparsable URL.
        health.failed("http://loops.example", Failure::TooManyRedirects);
        health.failed("http://loops.example", Failure::Other);
        assert_eq!(health.given_up("http://loops.example"), None);
        health.failed("", Failure::Dns);
        assert_eq!(health.given_up(""), None);
        // The port and the scheme belong to the key.
        assert_eq!(origin_key("HTTPS://Example.org/a?b"), "https://example.org");
        assert_eq!(origin_key("http://example.org:8080/"), "http://example.org:8080");
        assert_eq!(origin_key("nonsense"), "");
    }

    #[test]
    fn the_scheduler_skips_the_links_of_a_host_that_is_given_up() {
        let hosts = vec!["dead.example".to_string(); 4];
        let mut scheduler = HostScheduler::new(hosts, Duration::ZERO);
        let now = Instant::now();
        let failed = LinkResult {
            outcome: Outcome::Broken,
            failure: Some(Failure::Connect),
            ..LinkResult::default()
        };
        for job in 0..2 {
            assert_eq!(scheduler.take(now), Take::Job(job));
            scheduler.record(job, &failed);
            scheduler.release(job, now);
        }
        assert_eq!(scheduler.take(now), Take::Skip(2, Failure::Connect));
        assert_eq!(scheduler.take(now), Take::Skip(3, Failure::Connect));
        assert_eq!(scheduler.take(now), Take::Done);
    }

    #[test]
    fn the_scheduler_keeps_one_request_per_host_and_the_gap() {
        let gap = Duration::from_millis(500);
        let hosts = ["a.example", "a.example", "b.example", "a.example"]
            .map(str::to_string)
            .to_vec();
        let mut scheduler = HostScheduler::new(hosts, gap);
        let t0 = Instant::now();
        assert_eq!(scheduler.take(t0), Take::Job(0));
        // a.example is busy: the next free host is b.example.
        assert_eq!(scheduler.take(t0), Take::Job(2));
        assert!(matches!(scheduler.take(t0), Take::Wait(_)));
        scheduler.release(0, t0 + Duration::from_millis(100));
        // Free, but the gap has not passed: wait for the rest of it (capped at the poll interval).
        match scheduler.take(t0 + Duration::from_millis(560)) {
            Take::Wait(wait) => assert_eq!(wait, Duration::from_millis(40)),
            other => panic!("{other:?}"),
        }
        assert_eq!(scheduler.take(t0 + Duration::from_millis(600)), Take::Job(1));
        scheduler.release(1, t0 + Duration::from_millis(700));
        scheduler.release(2, t0 + Duration::from_millis(700));
        assert!(matches!(
            scheduler.take(t0 + Duration::from_millis(1_000)),
            Take::Wait(_)
        ));
        assert_eq!(scheduler.take(t0 + Duration::from_millis(1_200)), Take::Job(3));
        assert_eq!(scheduler.take(t0 + Duration::from_millis(1_200)), Take::Done);
    }

    #[test]
    fn the_user_agent_follows_the_cli_rules() {
        let signature = format!("SiteOne-Crawler/{}", siteone_crawler::version::CODE);
        assert_eq!(user_agent(Some("MyBot/1.0")), format!("MyBot/1.0 {signature}"));
        assert_eq!(user_agent(Some("MyBot/1.0 !")), "MyBot/1.0");
        let default = user_agent(None);
        assert!(
            default.starts_with("Mozilla/5.0 (Windows NT 10.0; Win64; x64)"),
            "{default}"
        );
        assert!(default.ends_with(&signature), "{default}");
        assert_eq!(user_agent(Some("  ")), default);
        let day = |y, m, d| chrono::NaiveDate::from_ymd_opt(y, m, d).expect("date");
        assert_eq!(chrome_major(day(2024, 11, 12)), 131);
        assert_eq!(chrome_major(day(2026, 11, 18)), 157);
    }

    #[test]
    fn the_settings_take_only_user_agent_proxy_and_timeout_from_the_run() {
        let schema = OptionSchema::load();
        let mut config = CrawlConfig::new("https://site.example/");
        // The app's default `--timeout` (30 s); a shorter one is raised to the minimum.
        assert_eq!(
            CheckSettings::from_config(&config, &schema).timeout,
            Duration::from_secs(30)
        );
        config
            .set(&schema, "--timeout", Value::Text("5".to_string()))
            .expect("set");
        assert_eq!(CheckSettings::from_config(&config, &schema).timeout, MIN_TIMEOUT);
        for (name, value) in [
            ("--timeout", Value::Text("30".to_string())),
            ("--proxy", Value::Text("proxy.example:3128".to_string())),
            ("--user-agent", Value::Text("Bot!".to_string())),
            ("--header", Value::List(vec!["Cookie: session=secret".to_string()])),
        ] {
            config.set(&schema, name, value).expect("set");
        }
        let settings = CheckSettings::from_config(&config, &schema);
        assert_eq!(settings.timeout, Duration::from_secs(30));
        assert_eq!(settings.proxy.as_deref(), Some("proxy.example:3128"));
        assert_eq!(settings.user_agent, "Bot");
        assert!(!format!("{settings:?}").contains("secret"));
        assert_eq!((settings.workers, settings.host_gap), (WORKERS, HOST_GAP));
    }
}
