//! Link texts (GUI-SPEC §4.8): fetches the source pages of a run's broken and external links — only
//! pages of the crawled site — and reads, for each target URL, the text of the link that points to
//! it ([`link_texts`]).
//!
//! # Rules
//!
//! - **Only the crawled site:** a page whose host is not the start URL's host (`www.` aside) is never
//!   requested; redirects are followed (by hand, at most 10) only within that site.
//! - **Credentials as the crawler sends them:** `--http-auth` and the `--header` values go with a
//!   request — the first one and every redirect, each judged on its own URL — only when the crawler
//!   would send them there ([`crate::model::retry::may_send_credentials`]: not from `https` to
//!   `http`, and for an IP address or `localhost` only to the start port). A page of the site
//!   outside that boundary is requested without them.
//! - Like the crawler: the run's User-Agent rule ([`super::link_check::user_agent`]), `--proxy`,
//!   `--timeout` (at least 10 s), `--accept-invalid-certs`.
//! - One page at a time, at least [`super::link_check::HOST_GAP`] apart; at most [`MAX_PAGE_BYTES`]
//!   of a page are read; only an HTML answer below 400 is read.
//! - **The text:** for `<a>`/`<area>` the text content, whitespace collapsed (an `<a>` without text
//!   around an image: `[image: alt]` or `[image]`; an `<area>`: its `alt`); for `<img>` its `alt`;
//!   otherwise the tag and attribute (`<script src>`). Several different texts for one target: the
//!   first three joined by ` | `. At most 200 characters. URLs are compared in their normalized
//!   form ([`crate::model::link_text::normalized`]), resolved against the page URL and `<base href>`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, LOCATION};
use scraper::{ElementRef, Html, Selector};

use super::catch_panic_unchecked;
use super::link_check::{HOST_GAP, MIN_TIMEOUT, user_agent};
use crate::model::config::{CrawlConfig, Value};
use crate::model::link_text::{PageTexts, normalized, same_site, url_key};
use crate::model::schema::OptionSchema;

/// The most of a source page that is read.
pub const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
/// A link text longer than this is cut (with `…`).
pub const MAX_TEXT_CHARS: usize = 200;
/// At most this many different texts per target and page.
const MAX_TEXTS: usize = 3;
const MAX_REDIRECTS: usize = 10;
const ACCEPT_VALUE: &str = "text/html,application/xhtml+xml,*/*;q=0.8";

// ---------------------------------------------------------------------------------------------
// Reading link texts from a page
// ---------------------------------------------------------------------------------------------

/// The elements and attributes that reference a URL.
const REFERENCES: &[(&str, &str)] = &[
    ("a", "href"),
    ("area", "href"),
    ("img", "src"),
    ("img", "srcset"),
    ("source", "src"),
    ("source", "srcset"),
    ("script", "src"),
    ("link", "href"),
    ("iframe", "src"),
    ("video", "src"),
    ("video", "poster"),
    ("audio", "src"),
];

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The URLs of a `srcset` value (each candidate's first word).
fn srcset_urls(value: &str) -> Vec<&str> {
    value
        .split(',')
        .filter_map(|candidate| candidate.split_whitespace().next())
        .collect()
}

/// What an element that references a target says about it.
fn element_text(element: ElementRef<'_>, attribute: &str) -> String {
    let name = element.value().name();
    let alt = || element.value().attr("alt").map(collapse).filter(|alt| !alt.is_empty());
    match name {
        "a" => {
            let text = collapse(&element.text().collect::<String>());
            if !text.is_empty() {
                return text;
            }
            let image = Selector::parse("img")
                .ok()
                .and_then(|selector| element.select(&selector).next());
            match image {
                Some(image) => match image.value().attr("alt").map(collapse).filter(|alt| !alt.is_empty()) {
                    Some(alt) => format!("[image: {alt}]"),
                    None => "[image]".to_string(),
                },
                None => element
                    .value()
                    .attr("title")
                    .or(element.value().attr("aria-label"))
                    .map(collapse)
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| "<a href> without text".to_string()),
            }
        }
        "area" => alt().unwrap_or_else(|| "<area href>".to_string()),
        "img" => alt().unwrap_or_else(|| format!("<img {attribute}>")),
        _ => format!("<{name} {attribute}>"),
    }
}

/// For each of `targets` (absolute URLs), the link text on the page `html` at `page_url`, or
/// `None` when the page does not reference it. Keys of the result: [`url_key`] of the targets.
pub fn link_texts(html: &str, page_url: &str, targets: &[String]) -> BTreeMap<String, String> {
    let wanted: HashSet<String> = targets.iter().map(|target| url_key(target)).collect();
    let mut found: HashMap<String, Vec<String>> = HashMap::new();
    let Ok(page) = url::Url::parse(page_url.trim()) else {
        return BTreeMap::new();
    };
    let document = Html::parse_document(html);
    let base = Selector::parse("base[href]")
        .ok()
        .and_then(|selector| document.select(&selector).next())
        .and_then(|base| base.value().attr("href"))
        .and_then(|href| page.join(href.trim()).ok())
        .unwrap_or(page);
    for (tag, attribute) in REFERENCES {
        let Ok(selector) = Selector::parse(&format!("{tag}[{attribute}]")) else {
            continue;
        };
        for element in document.select(&selector) {
            let Some(value) = element.value().attr(attribute) else {
                continue;
            };
            let urls = if attribute.ends_with("srcset") {
                srcset_urls(value)
            } else {
                vec![value]
            };
            for url in urls {
                let Ok(resolved) = base.join(url.trim()) else {
                    continue;
                };
                let key = normalized(&resolved);
                if !wanted.contains(&key) {
                    continue;
                }
                let text = element_text(element, attribute);
                let texts = found.entry(key).or_default();
                if !texts.contains(&text) && texts.len() < MAX_TEXTS {
                    texts.push(text);
                }
            }
        }
    }
    found
        .into_iter()
        .map(|(key, texts)| (key, cut(&texts.join(" | "), MAX_TEXT_CHARS)))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Fetching the source pages
// ---------------------------------------------------------------------------------------------

/// How the source pages are requested.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceSettings {
    /// The crawled site's host (the start URL's).
    pub site_host: String,
    /// The start URL (without userinfo): the credential boundary is drawn around it.
    pub site_url: String,
    pub user_agent: String,
    pub proxy: Option<String>,
    pub timeout: Duration,
    pub accept_invalid_certs: bool,
    /// `user:password` of `--http-auth`.
    pub http_auth: Option<String>,
    /// The `--header` values (`Name: value`).
    pub headers: Vec<String>,
    pub gap: Duration,
}

impl std::fmt::Debug for SourceSettings {
    /// Without the credentials and header values.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceSettings")
            .field("site_host", &self.site_host)
            .field("user_agent", &self.user_agent)
            .field("timeout", &self.timeout)
            .field("http_auth", &self.http_auth.as_ref().map(|_| "••••"))
            .field("headers", &self.headers.len())
            .finish_non_exhaustive()
    }
}

impl SourceSettings {
    /// The settings of a run with `config`, start URL `site_url` and its `--http-auth` value.
    pub fn from_config(config: &CrawlConfig, schema: &OptionSchema, site_url: &str, http_auth: Option<String>) -> Self {
        let text = |name: &str| match config.get(schema, name) {
            Some(Value::Text(text)) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
            _ => None,
        };
        let timeout = text("--timeout")
            .and_then(|seconds| seconds.parse::<u64>().ok())
            .map_or(MIN_TIMEOUT, |seconds| Duration::from_secs(seconds).max(MIN_TIMEOUT));
        let headers = match config.get(schema, "--header") {
            Some(Value::List(items)) => items,
            _ => Vec::new(),
        };
        let parsed = url::Url::parse(site_url.trim()).ok();
        Self {
            site_host: parsed
                .as_ref()
                .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
                .unwrap_or_default(),
            site_url: parsed
                .map(|mut url| {
                    let _ = url.set_username("");
                    let _ = url.set_password(None);
                    url.to_string()
                })
                .unwrap_or_default(),
            user_agent: user_agent(text("--user-agent").as_deref()),
            proxy: text("--proxy"),
            timeout,
            accept_invalid_certs: matches!(config.get(schema, "--accept-invalid-certs"), Some(Value::Bool(true))),
            http_auth: http_auth.filter(|auth| !auth.is_empty()),
            headers,
            gap: HOST_GAP,
        }
    }

    /// Whether `url` is a page of the crawled site.
    pub fn is_site_page(&self, url: &str) -> bool {
        url::Url::parse(url.trim()).ok().is_some_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some_and(|host| same_site(host, &self.site_host))
        })
    }

    /// Whether the crawler would send its credentials to `url` (the start URL's scheme, host and
    /// port decide).
    pub fn may_send_credentials(&self, url: &str) -> bool {
        crate::model::retry::may_send_credentials(&self.site_url, url)
    }
}

/// A message of a running fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextMsg {
    /// One source page was handled.
    Page(String, PageTexts),
    /// Every page was handled (not sent after a cancel).
    Finished,
}

/// A running fetch of source pages. Dropping it cancels the fetch.
pub struct TextHandle {
    receiver: Receiver<TextMsg>,
    cancel: Arc<AtomicBool>,
}

impl TextHandle {
    pub fn try_next(&self) -> Option<TextMsg> {
        self.receiver.try_recv().ok()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for TextHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Basic authentication for `user:password`.
fn basic_auth(credentials: &str) -> Option<HeaderValue> {
    use std::fmt::Write as _;
    // Base64 without a crate: the alphabet and padding of RFC 4648.
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = credentials.as_bytes();
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (index, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                let _ = encoded.write_char(char::from(ALPHABET[((n >> shift) & 63) as usize]));
            } else {
                encoded.push('=');
            }
        }
    }
    let mut value = HeaderValue::from_str(&format!("Basic {encoded}")).ok()?;
    value.set_sensitive(true);
    Some(value)
}

/// The headers of a request to `url`: `Accept`, and `--http-auth` and the `--header` values only
/// where the crawler would send them.
fn site_headers(settings: &SourceSettings, url: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static(ACCEPT_VALUE));
    if !settings.may_send_credentials(url) {
        return headers;
    }
    for header in &settings.headers {
        if let Some((name, value)) = header.split_once(':')
            && let (Ok(name), Ok(mut value)) = (
                HeaderName::from_bytes(name.trim().as_bytes()),
                HeaderValue::from_str(value.trim()),
            )
        {
            value.set_sensitive(true);
            headers.append(name, value);
        }
    }
    if let Some(value) = settings.http_auth.as_deref().and_then(basic_auth) {
        headers.insert(AUTHORIZATION, value);
    }
    headers
}

fn build_client(settings: &SourceSettings) -> Result<Client, String> {
    super::link_check::install_crypto_provider();
    let user_agent = if HeaderValue::from_str(&settings.user_agent).is_ok() {
        settings.user_agent.clone()
    } else {
        user_agent(None)
    };
    let mut builder = Client::builder()
        // Redirects are followed by `fetch_page`, which decides the headers of every request.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(settings.timeout)
        .connect_timeout(settings.timeout.min(MIN_TIMEOUT))
        .user_agent(user_agent)
        .danger_accept_invalid_certs(settings.accept_invalid_certs)
        .pool_max_idle_per_host(0);
    if let Some(proxy) = &settings.proxy {
        let url = if proxy.contains("://") {
            proxy.clone()
        } else {
            format!("http://{proxy}")
        };
        builder = builder
            .proxy(reqwest::Proxy::all(&url).map_err(|error| format!("The proxy {proxy} cannot be used: {error}"))?);
    }
    builder
        .build()
        .map_err(|error| format!("The HTTP client could not be created: {error}"))
}

/// Fetches and reads one source page of the site.
fn fetch_page(
    client: &Client,
    settings: &SourceSettings,
    page: &str,
    targets: &[String],
    cancel: &AtomicBool,
) -> PageTexts {
    let not_fetched = |reason: String| PageTexts::NotFetched { reason };
    let mut url = match url::Url::parse(page.trim()) {
        Ok(url) if settings.is_site_page(url.as_str()) => url,
        _ => return not_fetched("It is not a page of the crawled site.".to_string()),
    };
    let mut redirects = 0;
    let response = loop {
        let response = match client
            .get(url.clone())
            .headers(site_headers(settings, url.as_str()))
            .send()
        {
            Ok(response) => response,
            Err(error) => return not_fetched(format!("The page could not be requested: {}", error_text(&error))),
        };
        let location = matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308)
            .then(|| response.headers().get(LOCATION))
            .flatten()
            .and_then(|value| value.to_str().ok())
            .and_then(|location| url.join(location.trim()).ok());
        let Some(next) = location else {
            break response;
        };
        if !settings.is_site_page(next.as_str()) {
            return not_fetched(format!(
                "The page answered {} with a redirect outside the crawled site.",
                response.status().as_u16()
            ));
        }
        redirects += 1;
        if redirects > MAX_REDIRECTS || cancel.load(Ordering::SeqCst) {
            return not_fetched(format!("The page answered {}.", response.status().as_u16()));
        }
        url = next;
    };
    let status = response.status().as_u16();
    if status >= 300 {
        return not_fetched(format!("The page answered {status}."));
    }
    let html = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("html")
        });
    if !html {
        return not_fetched("The page is not HTML.".to_string());
    }
    let final_url = response.url().to_string();
    let mut body = Vec::new();
    let mut reader = response.take(MAX_PAGE_BYTES as u64);
    let mut buffer = [0u8; 32 * 1024];
    while !cancel.load(Ordering::SeqCst) {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => body.extend_from_slice(buffer.get(..read).unwrap_or_default()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&body);
    PageTexts::Fetched {
        texts: link_texts(&text, &final_url, targets),
    }
}

fn error_text(error: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut causes = Vec::new();
    let mut source = error.source();
    while let Some(cause) = source {
        causes.push(cause.to_string());
        source = cause.source();
    }
    let text = if causes.is_empty() {
        "request failed".to_string()
    } else {
        causes.join(": ")
    };
    cut(&text, 200)
}

/// Fetches `pages` one at a time (the gap of `settings` between requests) and reads in each the
/// texts of `targets`. Returns at once; `waker` is called after every message.
pub fn start(
    pages: Vec<String>,
    targets: Vec<String>,
    settings: SourceSettings,
    waker: impl Fn() + Send + Sync + 'static,
) -> io::Result<TextHandle> {
    let (sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let handle = TextHandle {
        receiver,
        cancel: Arc::clone(&cancel),
    };
    thread::Builder::new()
        .name("siteone-gui-linktext".to_string())
        .spawn(move || run(pages, targets, settings, cancel, sender, waker))?;
    Ok(handle)
}

fn run(
    pages: Vec<String>,
    targets: Vec<String>,
    settings: SourceSettings,
    cancel: Arc<AtomicBool>,
    sender: Sender<TextMsg>,
    waker: impl Fn(),
) {
    let send = |message: TextMsg| {
        let sent = sender.send(message).is_ok();
        waker();
        sent
    };
    let client = match catch_panic_unchecked(|| build_client(&settings)) {
        Ok(Ok(client)) => Some(client),
        Ok(Err(error)) => {
            for page in &pages {
                if !send(TextMsg::Page(
                    page.clone(),
                    PageTexts::NotFetched { reason: error.clone() },
                )) {
                    return;
                }
            }
            None
        }
        Err(_) => None,
    };
    if let Some(client) = client {
        let mut last: Option<Instant> = None;
        for page in pages {
            if let Some(last) = last {
                let wait = settings.gap.saturating_sub(last.elapsed());
                let until = Instant::now() + wait;
                while Instant::now() < until {
                    if cancel.load(Ordering::SeqCst) {
                        return;
                    }
                    thread::sleep(
                        until
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(50)),
                    );
                }
            }
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            let requests = settings.is_site_page(&page);
            let texts = catch_panic_unchecked(|| fetch_page(&client, &settings, &page, &targets, &cancel))
                .unwrap_or_else(|_| PageTexts::NotFetched {
                    reason: "The page could not be read (internal error).".to_string(),
                });
            if requests {
                last = Some(Instant::now());
            }
            if cancel.load(Ordering::SeqCst) || !send(TextMsg::Page(page, texts)) {
                return;
            }
        }
    }
    if !cancel.load(Ordering::SeqCst) {
        send(TextMsg::Finished);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(html: &str, page: &str, targets: &[&str]) -> Vec<Option<String>> {
        let targets: Vec<String> = targets.iter().map(|t| t.to_string()).collect();
        let found = link_texts(html, page, &targets);
        targets
            .iter()
            .map(|target| found.get(&url_key(target)).cloned())
            .collect()
    }

    #[test]
    fn link_texts_are_read_from_the_elements_that_reference_the_target() {
        let html = r#"<html><head><link rel="stylesheet" href="/css/site.css"></head><body>
            <p>Read <a href="../docs/guide.html#part">the   <b>full</b>
               guide</a> &amp; more.</p>
            <a href="https://Dead.Example:443/page/"><img src="/i/logo.png" alt="Dead &quot;logo&quot;"></a>
            <a href="https://dead.example/page">Dead &amp; gone</a>
            <a href="https://dead.example/page">Dead &amp; gone</a>
            <a href="https://dead.example/page">Third</a>
            <a href="https://dead.example/page">Fourth</a>
            <a href="/empty"><img src="/i/x.png"></a>
            <img srcset="/i/small.png 1x, /i/big%20one.png 2x" alt="">
            <script src="/js/app.js"></script>
            </body></html>"#;
        let found = texts(
            html,
            "https://site.example/blog/post/",
            &[
                "https://site.example/blog/docs/guide.html",
                "https://dead.example/page",
                "https://site.example/empty",
                "https://site.example/i/big one.png",
                "https://site.example/js/app.js",
                "https://site.example/css/site.css",
                "https://site.example/missing",
            ],
        );
        assert_eq!(
            found,
            [
                Some("the full guide".to_string()),
                Some("[image: Dead \"logo\"] | Dead & gone | Third".to_string()),
                Some("[image]".to_string()),
                Some("<img srcset>".to_string()),
                Some("<script src>".to_string()),
                Some("<link href>".to_string()),
                None,
            ]
        );
    }

    #[test]
    fn the_base_element_resolves_relative_links_and_texts_are_capped() {
        let long = "word ".repeat(80);
        let html = format!(
            r#"<head><base href="https://cdn.site.example/root/"></head><a href="page">{long}</a><area href="map" alt="Region">"#
        );
        let found = texts(
            &html,
            "https://site.example/other/",
            &[
                "https://cdn.site.example/root/page",
                "https://cdn.site.example/root/map",
            ],
        );
        let first = found[0].clone().expect("found through <base>");
        assert_eq!(first.chars().count(), MAX_TEXT_CHARS);
        assert!(first.ends_with('…'));
        assert_eq!(found[1].as_deref(), Some("Region"));
        assert_eq!(texts("<p>no links</p>", "not a url", &["https://x.example/"]), [None]);
    }

    #[test]
    fn credentials_go_only_into_the_sites_headers_and_never_into_debug_output() {
        assert_eq!(
            basic_auth("Aladdin:open sesame")
                .expect("value")
                .to_str()
                .expect("ascii"),
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
        assert_eq!(basic_auth("ab").expect("value").to_str().expect("ascii"), "Basic YWI=");
        let schema = OptionSchema::load();
        let mut config = CrawlConfig::new("https://www.site.example/");
        config
            .set(
                &schema,
                "--header",
                Value::List(vec!["X-Token: secret-token".to_string()]),
            )
            .expect("set");
        config
            .set(&schema, "--accept-invalid-certs", Value::Bool(true))
            .expect("set");
        let settings = SourceSettings::from_config(
            &config,
            &schema,
            "https://www.site.example/",
            Some("user:pass-secret".to_string()),
        );
        assert!(settings.accept_invalid_certs);
        assert!(settings.is_site_page("https://site.example/a"));
        assert!(!settings.is_site_page("https://cdn.site.example/a"));
        assert!(!settings.is_site_page("ftp://www.site.example/a"));
        let headers = site_headers(&settings, "https://site.example/a");
        assert_eq!(
            headers.get("x-token").and_then(|v| v.to_str().ok()),
            Some("secret-token")
        );
        assert!(headers.get(AUTHORIZATION).is_some());
        // A page of the site the crawler sends no credentials to (plain http after an https
        // start): neither the password nor a header value.
        assert!(settings.is_site_page("http://www.site.example/a"));
        let downgraded = site_headers(&settings, "http://www.site.example/a");
        assert!(downgraded.get("x-token").is_none() && downgraded.get(AUTHORIZATION).is_none());
        assert!(downgraded.get(ACCEPT).is_some());
        let debug = format!("{settings:?}");
        assert!(!debug.contains("secret"), "{debug}");
    }

    #[test]
    fn the_credential_boundary_is_the_crawlers() {
        let schema = OptionSchema::load();
        let settings = |site: &str| SourceSettings::from_config(&CrawlConfig::new(site), &schema, site, None);
        // The start URL's userinfo is not kept.
        let with_userinfo = settings("https://user:pw@site.example:8443/");
        assert_eq!(with_userinfo.site_url, "https://site.example:8443/");
        assert!(with_userinfo.may_send_credentials("https://site.example:8443/a"));
        // A named host: any port, never plain http after https.
        let named = settings("https://www.site.example/");
        assert!(named.may_send_credentials("https://site.example:8443/a"));
        assert!(!named.may_send_credentials("http://www.site.example/a"));
        // An IP address or localhost: the start port only.
        let ip = settings("http://127.0.0.1:8080/");
        assert!(ip.may_send_credentials("http://127.0.0.1:8080/a"));
        assert!(!ip.may_send_credentials("http://127.0.0.1:8081/a"));
        assert!(ip.is_site_page("http://127.0.0.1:8081/a"), "still a page of the site");
        let local = settings("http://localhost:8080/");
        assert!(!local.may_send_credentials("http://localhost:9090/a"));
        // Another host: not a page of the site at all.
        assert!(!ip.is_site_page("http://localhost:8080/a"));
        assert!(!ip.may_send_credentials("http://localhost:8080/a"));
    }
}
