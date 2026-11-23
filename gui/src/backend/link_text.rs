//! Link texts (GUI-SPEC §4.8): fetches the source pages of a run's broken and external links — only
//! pages of the crawled site, HTML pages and stylesheets — and reads, for each target URL, the text
//! of the link that points to it ([`link_texts`]) and where the reference is in the file
//! ([`locate`]: line and column, the place to go and fix).
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
//!   of a page ([`MAX_STYLESHEET_BYTES`] of a stylesheet) are read; only an HTML or CSS answer below
//!   400 is read.
//! - **The text:** for `<a>`/`<area>` the text content, whitespace collapsed (an `<a>` without text
//!   around an image: `[image: alt]` or `[image]`; an `<area>`: its `alt`); for `<img>` its `alt`;
//!   otherwise the tag and attribute (`<script src>`). Several different texts for one target: the
//!   first three joined by ` | `. At most 200 characters. URLs are compared in their normalized
//!   form ([`crate::model::link_text::normalized`]), resolved against the page URL and `<base href>`.
//! - **The position** ([`locate`]): a tolerant scan of the raw text (`scraper` gives no source
//!   positions) for the attribute values that hold URLs (`href`, `src`, `srcset` items, `poster`,
//!   `<object data>`, …) and the `url()` of inline `style` attributes and `<style>` blocks —
//!   entities decoded before the value is resolved — and, in a stylesheet, for `url()` (quoted or
//!   not) and `@import "…"`, resolved against the stylesheet's own URL. Comments and the text of
//!   `<script>` are skipped: a link a script builds has no position. The first reference's line
//!   and column (1-based, in characters, where the URL text starts) and the number of references
//!   are kept.

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
use crate::model::link_text::{LONG_LINE_CHARS, PageTexts, RefPosition, is_stylesheet, normalized, same_site, url_key};
use crate::model::schema::OptionSchema;

/// The most of a source page that is read.
pub const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
/// The most of a stylesheet that is read: an unminified framework bundle can outgrow a page.
pub const MAX_STYLESHEET_BYTES: usize = 8 * 1024 * 1024;
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
                let Ok(resolved) = base.join(url.trim_ascii()) else {
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
// Locating the references in the source
// ---------------------------------------------------------------------------------------------

/// What a source file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Html,
    Css,
}

/// The first reference to a target in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub position: RefPosition,
    /// What references it first: `<img src>`, `<div style> url()`, `<style> url()`, `url()`,
    /// `@import`.
    pub what: String,
}

/// A URL as written in a source file: the byte offset of its text, the URL (entities decoded) and
/// what references it.
struct Reference {
    offset: usize,
    url: String,
    what: String,
}

/// Attributes whose value is one URL (`srcset`, `imagesrcset` and `style` are read apart).
fn is_url_attribute(tag: &str, name: &str) -> bool {
    match name {
        "href" | "src" | "poster" | "action" | "formaction" | "cite" | "background" | "longdesc" | "manifest"
        | "xlink:href" => true,
        "data" => tag == "object",
        "content" => tag == "meta",
        _ => false,
    }
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
}

fn skip_spaces(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).copied().is_some_and(is_space) {
        at += 1;
    }
    at
}

/// The first `needle` at or after `from`, ignoring ASCII case.
fn find_ignore_case(bytes: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= bytes.len() {
        return None;
    }
    bytes
        .get(from..)?
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
        .map(|at| at + from)
}

/// The first `byte` at or after `from`.
fn find_byte(bytes: &[u8], byte: u8, from: usize) -> Option<usize> {
    bytes
        .get(from..)?
        .iter()
        .position(|candidate| *candidate == byte)
        .map(|at| at + from)
}

fn starts_with_ignore_case(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

/// The end of a CSS string opened before `from` with `quote`: its closing quote, a line end, or the
/// end of the text.
fn string_end(bytes: &[u8], from: usize, quote: u8) -> usize {
    let mut at = from;
    while let Some(&byte) = bytes.get(at) {
        if byte == quote || byte == b'\n' {
            return at;
        }
        at += if byte == b'\\' { 2 } else { 1 };
    }
    bytes.len()
}

/// `raw` with its character references decoded, and for each byte of the result the offset in
/// `raw` it came from (one more entry for the end).
fn decode_mapped(raw: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(raw.len());
    let mut map = Vec::with_capacity(raw.len() + 1);
    let mut at = 0;
    while let Some(rest) = raw.get(at..).filter(|rest| !rest.is_empty()) {
        if rest.starts_with('&')
            && let Some(semicolon) = rest.get(1..rest.len().min(12)).and_then(|window| window.find(';'))
            && let Some(character) = rest.get(1..=semicolon).and_then(super::link_check::entity)
        {
            out.push(character);
            map.extend(std::iter::repeat_n(at, character.len_utf8()));
            at += semicolon + 2;
            continue;
        }
        let Some(character) = rest.chars().next() else {
            break;
        };
        out.push(character);
        map.extend((0..character.len_utf8()).map(|byte| at + byte));
        at += character.len_utf8();
    }
    map.push(raw.len());
    (out, map)
}

/// The `url()` and `@import "…"` references of CSS text: (byte offset of the URL text, the URL,
/// what). Comments are skipped, and so are other strings.
fn css_references(css: &str) -> Vec<(usize, String, &'static str)> {
    let bytes = css.as_bytes();
    let text = |start: usize, end: usize| css.get(start..end.min(css.len())).unwrap_or("").to_string();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(rest) = bytes.get(at..).filter(|rest| !rest.is_empty()) {
        if rest.starts_with(b"/*") {
            at = find_ignore_case(bytes, b"*/", at + 2).map_or(bytes.len(), |end| end + 2);
        } else if starts_with_ignore_case(rest, b"@import") {
            at = skip_spaces(bytes, at + 7);
            if let Some(&quote) = bytes.get(at).filter(|byte| matches!(byte, b'"' | b'\'')) {
                let end = string_end(bytes, at + 1, quote);
                found.push((at + 1, text(at + 1, end), "@import"));
                at = end + 1;
            }
        } else if starts_with_ignore_case(rest, b"url(")
            && !at
                .checked_sub(1)
                .and_then(|before| bytes.get(before))
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            let start = skip_spaces(bytes, at + 4);
            match bytes.get(start) {
                Some(&quote) if quote == b'"' || quote == b'\'' => {
                    let end = string_end(bytes, start + 1, quote);
                    found.push((start + 1, text(start + 1, end), "url()"));
                    at = end + 1;
                }
                _ => {
                    let end = find_byte(bytes, b')', start).unwrap_or(bytes.len());
                    let url = text(start, end);
                    let url = url.trim_end();
                    if !url.is_empty() {
                        found.push((start, url.to_string(), "url()"));
                    }
                    at = end + 1;
                }
            }
        } else if matches!(rest[0], b'"' | b'\'') {
            at = string_end(bytes, at + 1, rest[0]) + 1;
        } else {
            at += 1;
        }
    }
    found
}

/// The references of one attribute whose value `raw` starts at byte `start` of the page.
fn attribute_references(tag: &str, name: &str, raw: &str, start: usize, found: &mut Vec<Reference>) {
    if name != "srcset" && name != "imagesrcset" && name != "style" && !is_url_attribute(tag, name) {
        return;
    }
    let (value, map) = decode_mapped(raw);
    let at = |offset: usize| start + map.get(offset).copied().unwrap_or(0);
    match name {
        "srcset" | "imagesrcset" => {
            let mut offset = 0;
            for candidate in value.split(',') {
                let lead = candidate.len() - candidate.trim_start().len();
                if let Some(url) = candidate.split_whitespace().next() {
                    found.push(Reference {
                        offset: at(offset + lead),
                        url: url.to_string(),
                        what: format!("<{tag} {name}>"),
                    });
                }
                offset += candidate.len() + 1;
            }
        }
        "style" => {
            for (offset, url, _) in css_references(&value) {
                found.push(Reference {
                    offset: at(offset),
                    url,
                    what: format!("<{tag} style> url()"),
                });
            }
        }
        _ => {
            // Only ASCII whitespace is not part of the URL (a non-breaking space is).
            let lead = value.len() - value.trim_ascii_start().len();
            let url = value.trim_ascii();
            if !url.is_empty() {
                found.push(Reference {
                    offset: at(lead),
                    url: url.to_string(),
                    what: format!("<{tag} {name}>"),
                });
            }
        }
    }
}

/// The URL references of an HTML page in document order, and the first `<base href>` (decoded).
fn html_references(html: &str) -> (Vec<Reference>, Option<String>) {
    let bytes = html.as_bytes();
    let length = bytes.len();
    let mut found = Vec::new();
    let mut base = None;
    let mut at = 0;
    while let Some(open) = find_byte(bytes, b'<', at) {
        at = open + 1;
        if bytes.get(at..).is_some_and(|tail| tail.starts_with(b"!--")) {
            at = find_ignore_case(bytes, b"-->", at + 3).map_or(length, |end| end + 3);
            continue;
        }
        // End tags, `<!DOCTYPE>`, a `<` in text.
        if !bytes.get(at).is_some_and(u8::is_ascii_alphabetic) {
            continue;
        }
        let name_end = (at..length)
            .find(|&i| !(bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'-' | b':')))
            .unwrap_or(length);
        let tag = html.get(at..name_end).unwrap_or("").to_ascii_lowercase();
        at = name_end;
        // The attributes, up to `>`.
        loop {
            while bytes.get(at).is_some_and(|byte| is_space(*byte) || *byte == b'/') {
                at += 1;
            }
            match bytes.get(at) {
                None => break,
                Some(b'>') => {
                    at += 1;
                    break;
                }
                _ => {}
            }
            let name_start = at;
            while bytes
                .get(at)
                .is_some_and(|byte| !is_space(*byte) && !matches!(byte, b'=' | b'>' | b'/'))
            {
                at += 1;
            }
            if at == name_start {
                at += 1;
                continue;
            }
            let name = html.get(name_start..at).unwrap_or("").to_ascii_lowercase();
            let look = skip_spaces(bytes, at);
            if bytes.get(look) != Some(&b'=') {
                at = look;
                continue;
            }
            let look = skip_spaces(bytes, look + 1);
            let (start, end, next) = match bytes.get(look) {
                Some(&quote) if quote == b'"' || quote == b'\'' => {
                    let end = find_byte(bytes, quote, look + 1).unwrap_or(length);
                    (look + 1, end, end + 1)
                }
                _ => {
                    let end = (look..length)
                        .find(|&i| is_space(bytes[i]) || bytes[i] == b'>')
                        .unwrap_or(length);
                    (look, end, end)
                }
            };
            let raw = html.get(start..end).unwrap_or("");
            if tag == "base" && name == "href" {
                if base.is_none() {
                    base = Some(decode_mapped(raw).0.trim().to_string());
                }
            } else {
                attribute_references(&tag, &name, raw, start, &mut found);
            }
            at = next;
        }
        at = at.min(length);
        // Raw text, never markup: a `<style>` block is CSS, a script's text is skipped.
        if matches!(tag.as_str(), "script" | "style" | "textarea" | "title" | "xmp") {
            let end = find_ignore_case(bytes, format!("</{tag}").as_bytes(), at).unwrap_or(length);
            if tag == "style" {
                for (offset, url, _) in css_references(html.get(at..end).unwrap_or("")) {
                    found.push(Reference {
                        offset: at + offset,
                        url,
                        what: "<style> url()".to_string(),
                    });
                }
            }
            at = end;
        }
    }
    (found, base)
}

/// The position of byte `offset` of `source`, whose line ends are at `newlines`.
fn position_at(source: &str, newlines: &[usize], offset: usize, count: u32) -> RefPosition {
    let line = newlines.partition_point(|&newline| newline < offset);
    let start = line
        .checked_sub(1)
        .and_then(|before| newlines.get(before))
        .map_or(0, |newline| newline + 1);
    let end = newlines.get(line).copied().unwrap_or(source.len());
    let column = source.get(start..offset).map_or(0, |text| text.chars().count()) + 1;
    let length = source
        .get(start..end)
        .map_or(0, |text| text.trim_end_matches('\r').chars().count());
    RefPosition {
        line: u32::try_from(line + 1).unwrap_or(u32::MAX),
        column: u32::try_from(column).unwrap_or(u32::MAX),
        count,
        long_line: length > LONG_LINE_CHARS,
        crawled: false,
    }
}

/// For each of `targets` (absolute URLs) that the file `source` at `url` references: where the
/// first reference is (line and column of its URL text) and how many there are. URLs are resolved
/// against `<base href>` and the page (HTML) or the stylesheet's own URL (CSS) and compared
/// [`normalized`]. Keys of the result: [`url_key`] of the targets.
pub fn locate(source: &str, url: &str, kind: SourceKind, targets: &[String]) -> BTreeMap<String, Located> {
    let Ok(page) = url::Url::parse(url.trim()) else {
        return BTreeMap::new();
    };
    let (mut references, base) = match kind {
        SourceKind::Html => html_references(source),
        SourceKind::Css => (
            css_references(source)
                .into_iter()
                .map(|(offset, url, what)| Reference {
                    offset,
                    url,
                    what: what.to_string(),
                })
                .collect(),
            None,
        ),
    };
    references.sort_by_key(|reference| reference.offset);
    let base = base.and_then(|href| page.join(&href).ok()).unwrap_or(page);
    let wanted: HashSet<String> = targets.iter().map(|target| url_key(target)).collect();
    let mut found: BTreeMap<String, (usize, String, u32)> = BTreeMap::new();
    for reference in references {
        let written = reference.url.trim_ascii();
        if written.is_empty() {
            continue;
        }
        let Ok(resolved) = base.join(written) else {
            continue;
        };
        let key = normalized(&resolved);
        if wanted.contains(&key) {
            found.entry(key).or_insert((reference.offset, reference.what, 0)).2 += 1;
        }
    }
    if found.is_empty() {
        return BTreeMap::new();
    }
    let newlines: Vec<usize> = source
        .bytes()
        .enumerate()
        .filter(|(_, byte)| *byte == b'\n')
        .map(|(at, _)| at)
        .collect();
    found
        .into_iter()
        .map(|(key, (offset, what, count))| {
            let position = position_at(source, &newlines, offset, count);
            (key, Located { position, what })
        })
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

/// Fetches and reads one source page of the site: an HTML page or a stylesheet.
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
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let final_url = response.url().to_string();
    let kind = if content_type.contains("css") {
        SourceKind::Css
    } else if content_type.contains("html") {
        SourceKind::Html
    } else if is_stylesheet(&final_url) {
        SourceKind::Css
    } else {
        return not_fetched("The page is neither HTML nor a stylesheet.".to_string());
    };
    let limit = match kind {
        SourceKind::Html => MAX_PAGE_BYTES,
        SourceKind::Css => MAX_STYLESHEET_BYTES,
    };
    let mut body = Vec::new();
    let mut reader = response.take(limit as u64);
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
    // An editor counts no column for a byte order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let located = locate(text, &final_url, kind, targets);
    match kind {
        SourceKind::Html => {
            let mut texts = link_texts(text, &final_url, targets);
            // A reference the link texts do not cover (an inline style, a `<style>` block,
            // `<object data>`, …) is named by what it is.
            for (key, found) in &located {
                texts.entry(key.clone()).or_insert_with(|| found.what.clone());
            }
            PageTexts::Fetched {
                texts,
                positions: located.into_iter().map(|(key, found)| (key, found.position)).collect(),
            }
        }
        SourceKind::Css => PageTexts::Css {
            positions: located.into_iter().map(|(key, found)| (key, found.position)).collect(),
        },
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

    /// `(line, column, count, long line)` of each target in `source`, `None` where it has none.
    fn positions(source: &str, url: &str, kind: SourceKind, targets: &[&str]) -> Vec<Option<(u32, u32, u32, bool)>> {
        let targets: Vec<String> = targets.iter().map(|t| t.to_string()).collect();
        let found = locate(source, url, kind, &targets);
        targets
            .iter()
            .map(|target| {
                found.get(&url_key(target)).map(|found| {
                    (
                        found.position.line,
                        found.position.column,
                        found.position.count,
                        found.position.long_line,
                    )
                })
            })
            .collect()
    }

    #[test]
    fn references_are_located_in_the_attributes_of_an_html_page() {
        let html = "<!DOCTYPE html>\n<html><head>\n<link rel=\"stylesheet\" href=\"../css/site.css\">\n<!-- <a href=\"/gone\">commented out</a> -->\n<script>var u = \"/gone\"; document.write('<a href=\"/gone\">');</script>\n</head><body>\n<p><a class=x HREF='/gone?a=1&amp;b=2'>Gone</a>\n<img alt=\"\" src = /img/missing.png>\n<video poster=\"/v/poster.jpg\" src=\"/v/clip.mp4\"></video><object data=\"/doc.pdf\"></object>\n<img srcset=\"/i/small.png 1x,\n  /i/big%20one.png 2x\">\n<p>Ünïcödé text <a href=\"/caf&#233;\">café</a></p>\n</body></html>";
        assert_eq!(
            positions(
                html,
                "https://site.example/blog/post/",
                SourceKind::Html,
                &[
                    "https://site.example/blog/css/site.css",
                    "https://site.example/gone?a=1&b=2",
                    "https://site.example/img/missing.png",
                    "https://site.example/v/poster.jpg",
                    "https://site.example/v/clip.mp4",
                    "https://site.example/doc.pdf",
                    "https://site.example/i/big one.png",
                    "https://site.example/café",
                    "https://site.example/gone",
                    "https://site.example/nowhere",
                ],
            ),
            [
                Some((3, 30, 1, false)),
                // Entities decoded before the URL is resolved; the column is the raw text's.
                Some((7, 21, 1, false)),
                // An unquoted value after spaces around `=`.
                Some((8, 19, 1, false)),
                Some((9, 16, 1, false)),
                Some((9, 36, 1, false)),
                Some((9, 71, 1, false)),
                // The second item of a srcset, on the next line.
                Some((11, 3, 1, false)),
                // Columns count characters, not bytes.
                Some((12, 26, 1, false)),
                // Only in a comment and in a script: no position.
                None,
                None,
            ]
        );
    }

    #[test]
    fn a_url_is_matched_as_the_crawler_reports_it() {
        // Only ASCII whitespace around a URL is not part of it: a non-breaking space is (the
        // crawler requests `/%C2%A0/x.html`). A mistyped scheme is compared as written.
        let html = "<p><a href=\"\u{a0}/x.html\">Nbsp</a>\n<a href=\" /y.html \">Spaces</a>\n<a href=\"htp://site.example/typo\">Typo</a></p>";
        let targets = [
            "https://site.example/%C2%A0/x.html",
            "https://site.example/y.html",
            "htp://site.example/typo",
            "https://site.example/x.html",
        ];
        assert_eq!(
            texts(html, "https://site.example/", &targets),
            [
                Some("Nbsp".to_string()),
                Some("Spaces".to_string()),
                Some("Typo".to_string()),
                None
            ]
        );
        assert_eq!(
            positions(html, "https://site.example/", SourceKind::Html, &targets),
            [
                Some((1, 13, 1, false)),
                Some((2, 11, 1, false)),
                Some((3, 10, 1, false)),
                None
            ]
        );
    }

    #[test]
    fn the_base_element_inline_styles_and_style_blocks_are_honoured() {
        let html = "<head><base href=\"https://cdn.site.example/root/\">\n<style>\n/* url(gone.png) */\n.hero { background: url( \"img/hero.webp\" ) }\n</style></head>\n<div style=\"background-image: url(&quot;img/tile.png&quot;)\">x</div>\n<a href=\"page\">Page</a> <a href=\"page\">Again</a>\n<a href=\"page#top\">Third</a>";
        let targets = [
            "https://cdn.site.example/root/img/hero.webp",
            "https://cdn.site.example/root/img/tile.png",
            "https://cdn.site.example/root/page",
            "https://cdn.site.example/root/gone.png",
            "https://site.example/page",
        ];
        assert_eq!(
            positions(html, "https://site.example/other/", SourceKind::Html, &targets),
            [
                Some((4, 27, 1, false)),
                // `&quot;` around the URL in a style attribute.
                Some((6, 41, 1, false)),
                // Several references: the first, and how many.
                Some((7, 10, 3, false)),
                None,
                None,
            ]
        );
        let targets: Vec<String> = targets.iter().map(|t| t.to_string()).collect();
        let found = locate(html, "https://site.example/other/", SourceKind::Html, &targets);
        assert_eq!(found[&url_key(&targets[0])].what, "<style> url()");
        assert_eq!(found[&url_key(&targets[1])].what, "<div style> url()");
        assert_eq!(found[&url_key(&targets[2])].what, "<a href>");
    }

    #[test]
    fn references_are_located_in_a_stylesheet() {
        let css = "@import \"base.css\";\n@import url(print.css) print;\n/* .old { background: url(../img/gone.png) } */\n.a { background: url(../img/bg.png) no-repeat; }\n.b { background: url( '../img/bg.png' ); }\n@font-face { src: url(\"../fonts/My Font.woff2\") format(\"woff2\"), url(  ../fonts/x.woff  ); }\n.c::after { content: \"url(../img/fake.png)\"; }\n.d { mask: myurl(../img/fake.png); }";
        assert_eq!(
            positions(
                css,
                "https://site.example/assets/css/page-styles.css",
                SourceKind::Css,
                &[
                    "https://site.example/assets/css/base.css",
                    "https://site.example/assets/css/print.css",
                    "https://site.example/assets/img/bg.png",
                    "https://site.example/assets/fonts/My%20Font.woff2",
                    "https://site.example/assets/fonts/x.woff",
                    "https://site.example/assets/img/gone.png",
                    "https://site.example/assets/img/fake.png",
                    "https://site.example/assets/img/missing.png",
                ],
            ),
            [
                Some((1, 10, 1, false)),
                Some((2, 13, 1, false)),
                // Unquoted and quoted, with spaces inside the parentheses: two references.
                Some((4, 22, 2, false)),
                Some((6, 24, 1, false)),
                Some((6, 72, 1, false)),
                // In a comment, in a string, in another function: none.
                None,
                None,
                None,
            ]
        );
    }

    #[test]
    fn a_minified_file_gives_the_column_on_its_one_long_line() {
        let mut css = String::new();
        for n in 0..400 {
            css.push_str(&format!(".r{n}{{color:#{n:03}}}"));
        }
        let column = css.chars().count() + 1 + ".x{background:url(".len();
        css.push_str(".x{background:url(../img/missing.png)}");
        assert!(css.len() > LONG_LINE_CHARS);
        let found = positions(
            &css,
            "https://site.example/css/style.min.css",
            SourceKind::Css,
            &["https://site.example/img/missing.png"],
        );
        assert_eq!(found, [Some((1, column as u32, 1, true))]);
        let position = RefPosition {
            line: 1,
            column: column as u32,
            count: 1,
            long_line: true,
            crawled: false,
        };
        assert_eq!(position.suffix(), format!(":1:{column}"));
        // A short line: the line alone.
        assert_eq!(
            positions(
                ".a{}\r\n.b{background:url(x.png)}",
                "https://site.example/s.css",
                SourceKind::Css,
                &["https://site.example/x.png"]
            ),
            [Some((2, 19, 1, false))]
        );
        assert!(locate("", "not a url", SourceKind::Css, &["https://x.example/".to_string()]).is_empty());
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
