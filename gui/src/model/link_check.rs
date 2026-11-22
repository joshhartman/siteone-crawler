//! The external link check of a run (GUI-SPEC §4.7): which links to check — taken from the
//! run's result JSON — what a check found, and the file it is kept in (`<run>/external-links.json`).
//!
//! # Where the links come from
//!
//! The crawler does not request URLs on other hosts; it lists them in two tables of the result
//! JSON, both built from its skipped-URL list (one entry per URL: the first page and the first
//! attribute it was found in):
//!
//! - `skipped` — every skipped URL with `reason` (`Not allowed host` for another host), `url` (the
//!   full URL for another host), `sourceAttr` (`<a href>`, `<img src>`, `<script src>`, `<link
//!   href>`, `css url()`, …) and `sourceUqId`, the page it was found on **without the start URL's
//!   scheme and host** (`/about/`);
//! - `external-urls` — the same URLs with `count` (number of pages) and `foundOn`, the full URLs of
//!   up to 5 pages joined by `", "`, with ` (+N)` for more.
//!
//! [`link_list`] joins them by URL: `skipped` says whether a URL is a link (`<a href>`) or a file
//! (anything else); `external-urls` gives the count and full source URLs. A URL that only one table
//! has is used too; without a `skipped` row its kind follows from its extension. Only `http`/`https`
//! URLs, fragments dropped, de-duplicated, sources merged (at most [`MAX_SOURCES`]), links capped at
//! [`MAX_LINKS`].

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::coverage::CrawlScope;
use super::link_text::LinkTexts;
use super::results::{ResultData, ResultTable};
use super::storage::{self, LoadError};

/// The file of a run that keeps its check.
pub const LINKS_FILE: &str = "external-links.json";
pub const SCHEMA_VERSION: u32 = 1;

/// At most this many links of a run are checked; the rest is reported as left out.
pub const MAX_LINKS: usize = 2_000;

/// At most this many pages are kept per link ("found on").
pub const MAX_SOURCES: usize = 20;

/// The reason of a skipped URL on another host (`SkippedUrlsAnalyzer`).
pub(crate) const REASON_OTHER_HOST: &str = "Not allowed host";

/// The `sourceAttr` of a link (`SkippedUrlsAnalyzer::get_source_short_name`).
const SOURCE_LINK: &str = "<a href>";

/// An external link to check: the URL as found, the pages it was found on, and on how many pages.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExternalLink {
    pub url: String,
    pub sources: Vec<String>,
    pub count: u64,
}

/// The external links of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkList {
    /// The links to check (at most [`MAX_LINKS`]).
    pub links: Vec<ExternalLink>,
    /// External files (images, scripts, styles, fonts, …) — not checked.
    pub files: usize,
    /// Links left out by the cap.
    pub truncated: usize,
}

impl LinkList {
    /// Every external link found, the ones left out by the cap included.
    pub fn found(&self) -> usize {
        self.links.len() + self.truncated
    }
}

#[derive(Debug, Default)]
struct Entry {
    url: String,
    sources: Vec<String>,
    count: u64,
    /// Seen as `<a href>`.
    link: bool,
    /// Seen with another source attribute.
    file: bool,
}

impl Entry {
    fn add_source(&mut self, source: String) {
        if !source.is_empty() && !self.sources.contains(&source) && self.sources.len() < MAX_SOURCES {
            self.sources.push(source);
        }
    }
}

/// The URL to request and to de-duplicate on: an absolute `http`/`https` URL with a host, without
/// its fragment. `None` for anything else.
pub fn request_url(url: &str) -> Option<String> {
    let mut parsed = url::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none_or(str::is_empty) {
        return None;
    }
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

/// The cells of a table by column key.
pub(crate) fn column(table: &ResultTable, key: &str) -> Option<usize> {
    table.columns.iter().position(|column| column.key == key)
}

pub(crate) fn cell(row: &[String], index: Option<usize>) -> &str {
    index.and_then(|index| row.get(index)).map_or("", String::as_str)
}

/// `foundOn` of `external-urls`: full URLs joined by `", "` (a piece that does not start a URL
/// belongs to the previous one), then ` (+N)` when there are more.
fn split_found_on(text: &str) -> Vec<String> {
    let text = text.trim();
    let text = match text.rfind(" (+") {
        Some(at) if text.ends_with(')') && text[at + 3..text.len() - 1].chars().all(|c| c.is_ascii_digit()) => {
            &text[..at]
        }
        _ => text,
    };
    let mut sources: Vec<String> = Vec::new();
    for piece in text.split(", ") {
        let starts_url = piece.starts_with("http://") || piece.starts_with("https://");
        match sources.last_mut() {
            Some(last) if !starts_url => {
                last.push_str(", ");
                last.push_str(piece);
            }
            _ if !piece.trim().is_empty() => sources.push(piece.trim().to_string()),
            _ => {}
        }
    }
    sources
}

/// A page of `skipped.sourceUqId` as a full URL: the analyzer strips the start URL's scheme and
/// host from it.
fn resolve_source(source: &str, base: Option<&url::Url>) -> String {
    let source = source.trim();
    if source.is_empty() || source.starts_with("http://") || source.starts_with("https://") {
        return source.to_string();
    }
    base.and_then(|base| base.join(source).ok())
        .map(|url| url.to_string())
        .unwrap_or_else(|| source.to_string())
}

/// Whether a URL whose source attribute is unknown points to a file (an image, script, style,
/// font or media) by its extension.
fn looks_like_file(url: &str) -> bool {
    let path = url::Url::parse(url)
        .map(|parsed| parsed.path().to_string())
        .unwrap_or_default();
    let last = path.rsplit('/').next().unwrap_or("");
    let Some((stem, extension)) = last.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && matches!(
            extension.to_ascii_lowercase().as_str(),
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "svg"
                | "avif"
                | "ico"
                | "bmp"
                | "css"
                | "js"
                | "mjs"
                | "woff"
                | "woff2"
                | "ttf"
                | "otf"
                | "eot"
                | "mp4"
                | "webm"
                | "mp3"
                | "ogg"
                | "wav"
                | "json"
                | "xml"
        )
}

/// The external links of a run from its result JSON (see the module documentation). `site_url` is
/// the run's start URL, against which `skipped` resolves its source pages.
pub fn link_list(data: &ResultData, site_url: &str) -> LinkList {
    let base = url::Url::parse(site_url.trim()).ok();
    let mut entries: Vec<Entry> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut entry_for = |entries: &mut Vec<Entry>, key: String, url: &str| -> usize {
        *index.entry(key).or_insert_with(|| {
            entries.push(Entry {
                url: url.trim().to_string(),
                ..Entry::default()
            });
            entries.len() - 1
        })
    };

    // `external-urls` first: its order (most pages first) is the order of the check.
    let mut known_kind = vec![];
    if let Some(table) = data.table("external-urls") {
        let (url, count, found_on) = (column(table, "url"), column(table, "count"), column(table, "foundOn"));
        for row in &table.rows {
            let raw = cell(row, url);
            let Some(key) = request_url(raw) else { continue };
            let at = entry_for(&mut entries, key, raw);
            if let Some(entry) = entries.get_mut(at) {
                for source in split_found_on(cell(row, found_on)) {
                    entry.add_source(source);
                }
                let pages = cell(row, count).trim().parse::<u64>().unwrap_or(0);
                entry.count = entry.count.max(pages);
            }
        }
    }
    if let Some(table) = data.table("skipped") {
        let (reason, url, attr, source) = (
            column(table, "reason"),
            column(table, "url"),
            column(table, "sourceAttr"),
            column(table, "sourceUqId"),
        );
        for row in &table.rows {
            if cell(row, reason) != REASON_OTHER_HOST {
                continue;
            }
            let raw = cell(row, url);
            let Some(key) = request_url(raw) else { continue };
            let at = entry_for(&mut entries, key, raw);
            if let Some(entry) = entries.get_mut(at) {
                if cell(row, attr) == SOURCE_LINK {
                    entry.link = true;
                } else {
                    entry.file = true;
                }
                entry.add_source(resolve_source(cell(row, source), base.as_ref()));
                if known_kind.len() <= at {
                    known_kind.resize(at + 1, false);
                }
                if let Some(known) = known_kind.get_mut(at) {
                    *known = true;
                }
            }
        }
    }

    let mut list = LinkList::default();
    for (at, mut entry) in entries.into_iter().enumerate() {
        let known = known_kind.get(at).copied().unwrap_or(false);
        let is_link = if known {
            entry.link
        } else {
            !looks_like_file(&entry.url)
        };
        if !is_link {
            list.files += 1;
            continue;
        }
        if list.links.len() >= MAX_LINKS {
            list.truncated += 1;
            continue;
        }
        entry.count = entry.count.max(entry.sources.len() as u64).max(1);
        list.links.push(ExternalLink {
            url: entry.url,
            sources: entry.sources,
            count: entry.count,
        });
    }
    list
}

// ---------------------------------------------------------------------------------------------
// What a check found
// ---------------------------------------------------------------------------------------------

/// How a link answered. The order is the sort order of the Link checker tab (broken first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// 4xx/5xx other than [`Outcome::Refused`], or no answer at all.
    Broken,
    /// 401, 403, 429: the site turned an automated request away. Flagged, but not called dead.
    Refused,
    /// An answer below 400 after the redirects.
    #[default]
    Ok,
}

/// Why a link that did not answer failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Failure {
    Dns,
    Connect,
    Tls,
    Timeout,
    TooManyRedirects,
    Other,
}

impl Failure {
    pub fn label(self) -> &'static str {
        match self {
            Failure::Dns => "DNS lookup failed",
            Failure::Connect => "Connection failed",
            Failure::Tls => "TLS error",
            Failure::Timeout => "Timeout",
            Failure::TooManyRedirects => "Too many redirects",
            Failure::Other => "Request failed",
        }
    }
}

/// The classification of a final HTTP status: below 400 OK (a 3xx only when its redirect cannot be
/// followed), 401/403/429 refused, every other 4xx/5xx broken.
pub fn classify_status(status: u16) -> Outcome {
    match status {
        401 | 403 | 429 => Outcome::Refused,
        400.. => Outcome::Broken,
        _ => Outcome::Ok,
    }
}

/// The check of one link.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LinkResult {
    pub url: String,
    pub sources: Vec<String>,
    pub count: u64,
    pub outcome: Outcome,
    /// The final HTTP status (after the redirects); `None` without an answer.
    pub status: Option<u16>,
    /// Why there was no answer.
    pub failure: Option<Failure>,
    /// The error in words (no answer), for the tooltip.
    pub error: Option<String>,
    /// Where the redirects ended, when that differs from the URL.
    pub final_url: Option<String>,
    /// The `<title>` of an HTML answer.
    pub title: Option<String>,
    pub ms: u64,
}

impl LinkResult {
    /// The status chip: the code, "Refused 403", or the failure in words.
    pub fn status_label(&self) -> String {
        match (self.outcome, self.status, self.failure) {
            (Outcome::Refused, Some(status), _) => format!("Refused {status}"),
            (_, Some(status), _) => status.to_string(),
            (_, None, Some(failure)) => failure.label().to_string(),
            (_, None, None) => Failure::Other.label().to_string(),
        }
    }

    /// Broken or refused: listed among the broken links.
    pub fn is_flagged(&self) -> bool {
        self.outcome != Outcome::Ok
    }
}

/// Counts by outcome.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutcomeCounts {
    pub ok: usize,
    pub refused: usize,
    pub broken: usize,
}

impl OutcomeCounts {
    pub fn of(results: &[LinkResult]) -> Self {
        results.iter().fold(Self::default(), |mut counts, result| {
            match result.outcome {
                Outcome::Ok => counts.ok += 1,
                Outcome::Refused => counts.refused += 1,
                Outcome::Broken => counts.broken += 1,
            }
            counts
        })
    }

    pub fn total(&self) -> usize {
        self.ok + self.refused + self.broken
    }

    /// "21 external links checked: 18 OK, 2 refused, 1 broken".
    pub fn summary(&self) -> String {
        let total = self.total();
        let noun = if total == 1 { "external link" } else { "external links" };
        let mut parts = vec![format!("{} OK", self.ok)];
        if self.refused > 0 {
            parts.push(format!("{} refused", self.refused));
        }
        parts.push(format!("{} broken", self.broken));
        format!("{total} {noun} checked: {}", parts.join(", "))
    }
}

/// The stored check of a run (`external-links.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CheckReport {
    /// `false`: stopped before every link was checked; `results` has the ones that were.
    pub complete: bool,
    /// When the check started (RFC 3339).
    pub checked_at: String,
    /// The links the check was asked to check.
    pub total: usize,
    /// External files of the run, not checked.
    pub files_not_checked: usize,
    /// Links left out by [`MAX_LINKS`].
    pub truncated: usize,
    pub results: Vec<LinkResult>,
}

impl CheckReport {
    pub fn path(run_dir: &Path) -> PathBuf {
        run_dir.join(LINKS_FILE)
    }

    /// Writes the report atomically into the run folder — never re-creating a folder that was
    /// deleted in the meantime (that is an error of kind `NotFound`).
    pub fn write(&self, run_dir: &Path) -> io::Result<()> {
        storage::write_json_in_existing_dir(&Self::path(run_dir), SCHEMA_VERSION, self)
    }

    /// The report of a run; `Ok(None)` when it has none.
    pub fn read(run_dir: &Path) -> Result<Option<CheckReport>, LoadError> {
        storage::read_json(&Self::path(run_dir), SCHEMA_VERSION)
    }

    pub fn counts(&self) -> OutcomeCounts {
        OutcomeCounts::of(&self.results)
    }
}

/// What a run knows about its external links: the list from its result JSON, its stored check, or
/// why there is neither — and the link texts of its broken and external links (GUI-SPEC §4.8).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunLinks {
    /// `None` until the result JSON is loaded (or when the run has none).
    pub list: Option<LinkList>,
    pub report: Option<CheckReport>,
    /// Why the check could not run, or why the stored one cannot be read.
    pub error: Option<String>,
    /// `(404 URL, page it was found on)` of the result JSON's `404` table.
    pub internal: Vec<(String, String)>,
    /// The link texts found so far (`link-texts.json`).
    pub texts: LinkTexts,
    /// The source pages of the external links are wanted too (the external link check ran).
    pub texts_external: bool,
    /// No source page is wanted that has not been handled (nothing to fetch now).
    pub texts_settled: bool,
    /// The crawl's scope and limits (`None` until the result JSON is loaded).
    pub scope: Option<CrawlScope>,
}

impl RunLinks {
    /// The list, the stored check and the stored link texts of a run whose result is `data` (call
    /// it on a worker).
    pub fn load(run_dir: &Path, data: Option<&ResultData>, site_url: &str) -> Self {
        let list = data.map(|data| link_list(data, site_url));
        let mut errors = Vec::new();
        let report = CheckReport::read(run_dir).unwrap_or_else(|error| {
            errors.push(format!("The stored external link check cannot be read: {error}."));
            None
        });
        let texts = LinkTexts::read(run_dir).ok().flatten().unwrap_or_default();
        let internal = data.map(internal_pairs).unwrap_or_default();
        Self {
            texts_external: false,
            list,
            report,
            error: errors.pop(),
            internal,
            texts,
            texts_settled: false,
            scope: data.map(CrawlScope::from_result),
        }
    }

    /// Links found that the stored check does not cover (no check, or a stopped one).
    pub fn unchecked(&self) -> usize {
        let found = self.list.as_ref().map_or(0, |list| list.links.len());
        match &self.report {
            Some(report) if report.complete => 0,
            Some(report) => found.saturating_sub(report.results.len()),
            None => found,
        }
    }

    /// Every URL whose link text is read on a fetched page: the 404s and the external links.
    pub fn text_targets(&self) -> Vec<String> {
        let mut targets: Vec<String> = self.internal.iter().map(|(target, _)| target.clone()).collect();
        if let Some(list) = &self.list {
            targets.extend(list.links.iter().map(|link| link.url.clone()));
        }
        targets.sort();
        targets.dedup();
        targets
    }

    /// The source pages not handled yet: those of the 404s (with `internal`) and, once the external
    /// link check has run, those of the external links — each once, in order.
    pub fn wanted_pages(&self, internal: bool) -> Vec<String> {
        let mut pages: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let internal_sources = self
            .internal
            .iter()
            .filter(|_| internal)
            .map(|(_, source)| source.as_str());
        let external_sources = self
            .list
            .iter()
            .filter(|_| self.texts_external)
            .flat_map(|list| list.links.iter())
            .flat_map(|link| link.sources.iter().map(String::as_str));
        for page in internal_sources.chain(external_sources) {
            if !page.is_empty() && !self.texts.pages.contains_key(page) && seen.insert(page) {
                pages.push(page.to_string());
            }
        }
        pages
    }
}

/// `(404 URL, page it was found on)` of the `404` table (`url`, `sourceUqId`: full URLs).
fn internal_pairs(data: &ResultData) -> Vec<(String, String)> {
    let Some(table) = data.table("404") else {
        return Vec::new();
    };
    let (url, source) = (column(table, "url"), column(table, "sourceUqId"));
    table
        .rows
        .iter()
        .map(|row| (cell(row, url).trim().to_string(), cell(row, source).trim().to_string()))
        .filter(|(url, source)| !url.is_empty() && !source.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::results::parse_result;

    const FIXTURE: &str = include_str!("../../../docs/OUTPUT-crawler.siteone.io.json");

    fn table(key: &str, columns: &[&str], rows: &[&[&str]]) -> String {
        let columns: Vec<String> = columns.iter().map(|c| format!(r#""{c}":{{"name":"{c}"}}"#)).collect();
        let rows: Vec<String> = rows
            .iter()
            .map(|row| {
                let cells: Vec<String> = columns_keys(row)
                    .map(|(key, value)| format!("{}:{}", serde_json::json!(key), serde_json::json!(value)))
                    .collect();
                format!("{{{}}}", cells.join(","))
            })
            .collect();
        format!(
            r#""{key}":{{"columns":{{{}}},"rows":[{}]}}"#,
            columns.join(","),
            rows.join(",")
        )
    }

    /// Rows are given as `key=value` cells.
    fn columns_keys<'a>(row: &'a [&'a str]) -> impl Iterator<Item = (&'a str, &'a str)> {
        row.iter().filter_map(|cell| cell.split_once('='))
    }

    fn data(tables: &[String]) -> ResultData {
        parse_result(&format!(r#"{{"tables":{{{}}}}}"#, tables.join(","))).expect("parses")
    }

    #[test]
    fn the_fixture_gives_its_links_with_full_source_urls_and_counts_its_script() {
        let data = parse_result(FIXTURE).expect("fixture");
        let list = link_list(&data, "https://crawler.siteone.io/");
        assert_eq!((list.links.len(), list.files, list.truncated), (88, 1, 0));
        let thanks = list
            .links
            .iter()
            .find(|link| link.url == "https://adamwathan.me/")
            .expect("a link of the fixture");
        assert_eq!(thanks.sources, ["https://crawler.siteone.io/introduction/thanks/"]);
        assert_eq!(thanks.count, 1);
        assert!(list.links.iter().all(|link| link.url.starts_with("http")));
    }

    #[test]
    fn links_and_files_are_told_apart_and_sources_are_joined() {
        let skipped = table(
            "skipped",
            &["reason", "url", "sourceAttr", "sourceUqId"],
            &[
                &[
                    "reason=Not allowed host",
                    "url=https://ok.example/page",
                    "sourceAttr=<a href>",
                    "sourceUqId=/about/",
                ],
                &[
                    "reason=Not allowed host",
                    "url=https://cdn.example/app.js",
                    "sourceAttr=<script src>",
                    "sourceUqId=/",
                ],
                &[
                    "reason=Not allowed host",
                    "url=https://cdn.example/logo",
                    "sourceAttr=<img src>",
                    "sourceUqId=/",
                ],
                &[
                    "reason=Robots.txt",
                    "url=/private/",
                    "sourceAttr=<a href>",
                    "sourceUqId=/",
                ],
                &[
                    "reason=Not allowed host",
                    "url=mailto:a@b.example",
                    "sourceAttr=<a href>",
                    "sourceUqId=/",
                ],
                &[
                    "reason=Not allowed host",
                    "url=https://ok.example/page#part",
                    "sourceAttr=<a href>",
                    "sourceUqId=https://www.site.example/x",
                ],
            ],
        );
        let external = table(
            "external-urls",
            &["url", "count", "foundOn"],
            &[
                &[
                    "url=https://ok.example/page",
                    "count=7",
                    "foundOn=https://site.example/a, b, https://site.example/c (+2)",
                ],
                &[
                    "url=https://only-here.example/",
                    "count=1",
                    "foundOn=https://site.example/",
                ],
                &[
                    "url=https://only-here.example/font.woff2",
                    "count=1",
                    "foundOn=https://site.example/",
                ],
                &["url=ftp://files.example/x", "count=1", "foundOn=https://site.example/"],
            ],
        );
        let list = link_list(&data(&[external, skipped]), "https://site.example/");
        let urls: Vec<&str> = list.links.iter().map(|link| link.url.as_str()).collect();
        assert_eq!(urls, ["https://ok.example/page", "https://only-here.example/"]);
        // The script, the image (by its attribute, extension or not) and the font (by extension).
        assert_eq!(list.files, 3);
        let page = &list.links[0];
        assert_eq!(
            page.sources,
            [
                "https://site.example/a, b",
                "https://site.example/c",
                "https://site.example/about/",
                "https://www.site.example/x",
            ]
        );
        assert_eq!(page.count, 7);
    }

    #[test]
    fn the_list_is_capped_and_sources_are_bounded() {
        let rows: Vec<Vec<String>> = (0..MAX_LINKS + 5)
            .map(|n| {
                vec![
                    format!("url=https://e{n}.example/"),
                    "count=1".to_string(),
                    "foundOn=https://site.example/".to_string(),
                ]
            })
            .collect();
        let rows: Vec<Vec<&str>> = rows
            .iter()
            .map(|row| row.iter().map(String::as_str).collect())
            .collect();
        let rows: Vec<&[&str]> = rows.iter().map(Vec::as_slice).collect();
        let list = link_list(
            &data(&[table("external-urls", &["url", "count", "foundOn"], &rows)]),
            "",
        );
        assert_eq!(
            (list.links.len(), list.truncated, list.found()),
            (MAX_LINKS, 5, MAX_LINKS + 5)
        );

        let many: Vec<String> = (0..MAX_SOURCES + 10)
            .map(|n| format!("https://site.example/p{n}"))
            .collect();
        let found_on = format!("foundOn={}", many.join(", "));
        let list = link_list(
            &data(&[table(
                "external-urls",
                &["url", "count", "foundOn"],
                &[&["url=https://x.example/", "count=30", &found_on]],
            )]),
            "",
        );
        assert_eq!(list.links[0].sources.len(), MAX_SOURCES);
        assert_eq!(list.links[0].count, 30);
        assert_eq!(link_list(&ResultData::default(), ""), LinkList::default());
    }

    #[test]
    fn statuses_are_classified_and_labelled() {
        assert_eq!(classify_status(200), Outcome::Ok);
        assert_eq!(classify_status(204), Outcome::Ok);
        assert_eq!(classify_status(304), Outcome::Ok);
        for refused in [401, 403, 429] {
            assert_eq!(classify_status(refused), Outcome::Refused);
        }
        for broken in [400, 404, 410, 500, 503] {
            assert_eq!(classify_status(broken), Outcome::Broken);
        }
        let mut result = LinkResult {
            outcome: Outcome::Refused,
            status: Some(403),
            ..LinkResult::default()
        };
        assert_eq!(result.status_label(), "Refused 403");
        result.outcome = Outcome::Broken;
        result.status = Some(404);
        assert_eq!(result.status_label(), "404");
        result.status = None;
        result.failure = Some(Failure::Tls);
        assert_eq!(result.status_label(), "TLS error");
        result.failure = Some(Failure::TooManyRedirects);
        assert_eq!(result.status_label(), "Too many redirects");
        let counts = OutcomeCounts {
            ok: 18,
            refused: 2,
            broken: 1,
        };
        assert_eq!(
            counts.summary(),
            "21 external links checked: 18 OK, 2 refused, 1 broken"
        );
    }

    #[test]
    fn the_report_round_trips_and_is_never_written_into_a_deleted_run() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let report = CheckReport {
            complete: false,
            checked_at: "2026-11-18T10:00:00+01:00".to_string(),
            total: 3,
            files_not_checked: 2,
            truncated: 0,
            results: vec![LinkResult {
                url: "https://ok.example/".to_string(),
                sources: vec!["https://site.example/".to_string()],
                count: 1,
                outcome: Outcome::Ok,
                status: Some(200),
                title: Some("Ok — Example".to_string()),
                final_url: Some("https://ok.example/home".to_string()),
                ms: 120,
                ..LinkResult::default()
            }],
        };
        assert_eq!(CheckReport::read(tmp.path()).expect("readable"), None);
        report.write(tmp.path()).expect("written");
        let text = std::fs::read_to_string(tmp.path().join(LINKS_FILE)).expect("file");
        assert!(text.contains("\"schemaVersion\": 1"), "{text}");
        assert_eq!(CheckReport::read(tmp.path()).expect("readable"), Some(report.clone()));

        let gone = tmp.path().join("deleted-run");
        let error = report.write(&gone).expect_err("no folder");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!gone.exists());

        let links = RunLinks {
            list: Some(LinkList {
                links: vec![ExternalLink::default(); 3],
                ..LinkList::default()
            }),
            report: Some(report),
            ..RunLinks::default()
        };
        assert_eq!(links.unchecked(), 2, "a stopped check leaves links unchecked");
    }

    #[test]
    fn the_wanted_source_pages_are_each_page_once_and_not_the_handled_ones() {
        let data = parse_result(FIXTURE).expect("fixture");
        let tmp = tempfile::tempdir().expect("temp dir");
        let mut links = RunLinks::load(tmp.path(), Some(&data), "https://crawler.siteone.io/");
        assert_eq!(
            links.internal,
            [(
                "https://crawler.siteone.io/docs/features/content-type-analysis".to_string(),
                "https://crawler.siteone.io/features/performance-metrics/".to_string()
            )]
        );
        assert!(!links.texts_external);
        assert_eq!(links.wanted_pages(false), Vec::<String>::new());
        assert_eq!(
            links.wanted_pages(true),
            ["https://crawler.siteone.io/features/performance-metrics/"]
        );
        links.texts_external = true;
        let wanted = links.wanted_pages(true);
        let mut unique = wanted.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), wanted.len(), "each page once");
        assert!(wanted.len() > 5 && wanted.len() < 88, "{}", wanted.len());
        links.texts.pages.insert(
            "https://crawler.siteone.io/features/performance-metrics/".to_string(),
            crate::model::link_text::PageTexts::Stylesheet,
        );
        assert!(
            !links
                .wanted_pages(true)
                .contains(&"https://crawler.siteone.io/features/performance-metrics/".to_string())
        );
        assert_eq!(links.text_targets().len(), 89);
    }
}
