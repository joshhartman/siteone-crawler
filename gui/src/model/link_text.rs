//! The link text of a broken or external link: the text of the link, on the page it was found on,
//! that points to it — so the link can be found on that page (GUI-SPEC §4.8). The crawler records
//! no link text; the app fetches the source pages (on the crawled site only) after the crawl and
//! reads it there (`backend::link_text`). What it found is kept in `<run>/link-texts.json`.
//!
//! Texts are stored per source page: a page that was fetched has the texts of every target the run
//! knows (its internal 404s and its external links) that the page references; a target it does not
//! reference is "not found on the page". Next to each text, where the reference is in the file as
//! fetched ([`RefPosition`]: line, column, how many references): the place to go and fix. A
//! stylesheet that was fetched has positions only (a `url()` or `@import` has no link text).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::storage::{self, LoadError};

pub const TEXTS_FILE: &str = "link-texts.json";
pub const SCHEMA_VERSION: u32 = 1;

/// At most this many source pages of a run are fetched.
pub const MAX_SOURCE_PAGES: usize = 200;

/// A reference whose line is longer than this many characters (a minified file) is shown with its
/// column.
pub const LONG_LINE_CHARS: usize = 500;

/// Where a source page references a target, in the file as fetched after the crawl: the first
/// reference's line and column (1-based, in characters; the column is where the URL text starts),
/// and how many references to the target the page has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefPosition {
    pub line: u32,
    pub column: u32,
    /// References to the target on the page (at least 1).
    #[serde(default = "one")]
    pub count: u32,
    /// The first reference's line is longer than [`LONG_LINE_CHARS`] (a minified file): its
    /// column is shown too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub long_line: bool,
    /// Reported by the crawler (in the page as it received it), not read by the app after the
    /// crawl. Never stored: a crawler position comes from the result JSON each time.
    #[serde(skip)]
    pub crawled: bool,
}

fn one() -> u32 {
    1
}

impl RefPosition {
    /// The suffix after the page's URL: `:1929`, or `:1:20345` on a long line.
    pub fn suffix(&self) -> String {
        if self.long_line {
            format!(":{}:{}", self.line, self.column)
        } else {
            format!(":{}", self.line)
        }
    }

    /// In words: "line 1929" or "line 1, column 20345", with "(and 2 more)" for further references.
    pub fn describe(&self) -> String {
        let mut text = if self.long_line {
            format!("line {}, column {}", self.line, self.column)
        } else {
            format!("line {}", self.line)
        };
        if self.count > 1 {
            text.push_str(&format!(" (and {} more)", self.count - 1));
        }
        text
    }

    /// For a tooltip: "Referenced at line 1929 (and 2 more) in the file as fetched after the
    /// crawl." — or, from the crawler, "in the page as the crawler received it".
    pub fn tip(&self) -> String {
        let place = if self.crawled {
            "in the file as the crawler received it"
        } else {
            "in the file as fetched after the crawl"
        };
        format!("Referenced at {} {place}.", self.describe())
    }
}

/// What became of a source page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum PageTexts {
    /// An HTML page, fetched and read: target key ([`url_key`]) → link text, and → where the page
    /// references it (absent in the files of earlier versions).
    Fetched {
        texts: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        positions: BTreeMap<String, RefPosition>,
    },
    /// A stylesheet, fetched and read: target key → where a `url()` or `@import` references it.
    Css { positions: BTreeMap<String, RefPosition> },
    /// A stylesheet that was not fetched (the files of earlier versions): the link is a `url()`
    /// in it.
    Stylesheet,
    /// Not fetched, and why.
    NotFetched { reason: String },
}

impl PageTexts {
    /// An HTML page that was read: its texts, no positions.
    pub fn texts(texts: BTreeMap<String, String>) -> Self {
        PageTexts::Fetched {
            texts,
            positions: BTreeMap::new(),
        }
    }

    /// Whether the page was fetched and read (an HTML page or a stylesheet).
    pub fn is_read(&self) -> bool {
        matches!(self, PageTexts::Fetched { .. } | PageTexts::Css { .. })
    }
}

/// The link texts of a run, by source page URL.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LinkTexts {
    pub pages: BTreeMap<String, PageTexts>,
    /// Source pages left out by [`MAX_SOURCE_PAGES`].
    pub cut: usize,
}

/// The link text of one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextLookup<'a> {
    Text(&'a str),
    /// The page was read but does not reference the URL (it changed, or a script builds the link).
    NotFound,
    /// Referenced by a stylesheet (a `url()` or `@import`: no link text).
    Stylesheet,
    /// The stylesheet was read but does not reference the URL.
    NotInStylesheet,
    /// The page was not fetched (the reason).
    NotFetched(&'a str),
    /// No source page, or not looked at yet.
    Unknown,
}

impl TextLookup<'_> {
    /// The text for sorting, filtering and Copy list.
    pub fn text(&self) -> &str {
        match self {
            TextLookup::Text(text) => text,
            TextLookup::NotFound => "not found on the page",
            TextLookup::Stylesheet => "in a stylesheet",
            TextLookup::NotInStylesheet => "not found in the stylesheet",
            TextLookup::NotFetched(_) => "source not fetched",
            TextLookup::Unknown => "",
        }
    }
}

impl LinkTexts {
    pub fn path(run_dir: &Path) -> PathBuf {
        run_dir.join(TEXTS_FILE)
    }

    /// Writes the texts into the run folder (never re-creating a deleted one).
    pub fn write(&self, run_dir: &Path) -> io::Result<()> {
        storage::write_json_in_existing_dir(&Self::path(run_dir), SCHEMA_VERSION, self)
    }

    pub fn read(run_dir: &Path) -> Result<Option<LinkTexts>, LoadError> {
        storage::read_json(&Self::path(run_dir), SCHEMA_VERSION)
    }

    /// The text of `target` on the first of `sources` that has one (else the first source's state).
    pub fn lookup<'a>(&'a self, target: &str, sources: &[&str]) -> TextLookup<'a> {
        let key = url_key(target);
        let mut first = None;
        for source in sources {
            let found = match self.pages.get(*source) {
                Some(PageTexts::Fetched { texts, .. }) => match texts.get(&key) {
                    Some(text) => return TextLookup::Text(text),
                    None => TextLookup::NotFound,
                },
                Some(PageTexts::Css { positions }) if positions.contains_key(&key) => return TextLookup::Stylesheet,
                Some(PageTexts::Css { .. }) => TextLookup::NotInStylesheet,
                Some(PageTexts::Stylesheet) => TextLookup::Stylesheet,
                Some(PageTexts::NotFetched { reason }) => TextLookup::NotFetched(reason),
                None => TextLookup::Unknown,
            };
            first.get_or_insert(found);
        }
        first.unwrap_or(TextLookup::Unknown)
    }

    /// Where the page `source` references `target` (`None` when the page was not read or has no
    /// reference to it).
    pub fn position(&self, target: &str, source: &str) -> Option<&RefPosition> {
        match self.pages.get(source)? {
            PageTexts::Fetched { positions, .. } | PageTexts::Css { positions } => positions.get(&url_key(target)),
            PageTexts::Stylesheet | PageTexts::NotFetched { .. } => None,
        }
    }

    /// `source` with the position of its reference to `target` (`…/style.css:1929`), else
    /// `source` alone.
    pub fn found_on_with_position(&self, target: &str, source: &str) -> String {
        match self.position(target, source) {
            Some(position) => format!("{source}{}", position.suffix()),
            None => source.to_string(),
        }
    }
}

/// Whether a source "page" is a stylesheet (its path ends with `.css`): read as CSS when its
/// answer does not say it is HTML.
pub fn is_stylesheet(url: &str) -> bool {
    url::Url::parse(url.trim())
        .map(|parsed| parsed.path().to_ascii_lowercase().ends_with(".css"))
        .unwrap_or(false)
}

/// Whether two hosts are one site (`www.` aside), as the crawler sees it.
pub fn same_site(a: &str, b: &str) -> bool {
    let a = a.to_ascii_lowercase();
    let b = b.to_ascii_lowercase();
    a.strip_prefix("www.").unwrap_or(&a) == b.strip_prefix("www.").unwrap_or(&b)
}

/// Decodes `%XX` sequences (invalid ones stay), lossy for bytes that are no UTF-8.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == b'%'
            && let Some(hex) = text.get(at + 1..at + 3)
            && let Ok(value) = u8::from_str_radix(hex, 16)
        {
            out.push(value);
            at += 3;
            continue;
        }
        out.push(byte);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The comparable form of an absolute URL: scheme and host lower-case, default port dropped
/// (`url` does both), no fragment, path and query percent-decoded, no trailing `/` in the path.
pub fn normalized(url: &url::Url) -> String {
    let mut path = percent_decode(url.path());
    while path.ends_with('/') {
        path.pop();
    }
    let mut key = format!(
        "{}://{}",
        url.scheme(),
        url.host_str().unwrap_or("").to_ascii_lowercase()
    );
    if let Some(port) = url.port() {
        key.push_str(&format!(":{port}"));
    }
    key.push_str(&path);
    if let Some(query) = url.query().filter(|query| !query.is_empty()) {
        key.push('?');
        key.push_str(&percent_decode(query));
    }
    key
}

/// The key of a target URL in [`PageTexts::Fetched`] (its [`normalized`] form; the trimmed text
/// when it is no absolute URL).
pub fn url_key(url: &str) -> String {
    url::Url::parse(url.trim())
        .map(|parsed| normalized(&parsed))
        .unwrap_or_else(|_| url.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_compare_without_the_differences_that_do_not_matter() {
        let same = [
            "HTTPS://Example.COM:443/a%20b/?q=%C3%A9#top",
            "https://example.com/a b?q=é",
            "https://example.com/a%20b",
        ];
        assert_eq!(url_key(same[0]), url_key(same[1]));
        assert_ne!(url_key(same[0]), url_key(same[2]), "the query counts");
        assert_eq!(url_key("http://example.com:80/x/"), url_key("http://example.com/x"));
        assert_ne!(url_key("http://example.com:8080/x"), url_key("http://example.com/x"));
        assert_eq!(url_key("not a url "), "not a url");
        assert!(same_site("www.Example.com", "example.com"));
        assert!(!same_site("cdn.example.com", "example.com"));
        assert!(is_stylesheet("https://example.com/css/site.css?v=2"));
    }

    #[test]
    fn lookup_reads_the_first_source_with_a_text_and_the_file_round_trips() {
        let mut texts = LinkTexts::default();
        texts.pages.insert(
            "https://site.example/a".to_string(),
            PageTexts::Fetched {
                texts: BTreeMap::from([(url_key("https://dead.example/"), "Dead link".to_string())]),
                positions: BTreeMap::from([(
                    url_key("https://dead.example/"),
                    RefPosition {
                        line: 12,
                        column: 9,
                        count: 3,
                        long_line: false,
                        crawled: false,
                    },
                )]),
            },
        );
        texts
            .pages
            .insert("https://site.example/b".to_string(), PageTexts::texts(BTreeMap::new()));
        texts.pages.insert(
            "https://site.example/new.css".to_string(),
            PageTexts::Css {
                positions: BTreeMap::from([(
                    url_key("https://site.example/font.woff2"),
                    RefPosition {
                        line: 1,
                        column: 20345,
                        count: 1,
                        long_line: true,
                        crawled: false,
                    },
                )]),
            },
        );
        texts.pages.insert(
            "https://site.example/c".to_string(),
            PageTexts::NotFetched {
                reason: "404".to_string(),
            },
        );
        texts
            .pages
            .insert("https://site.example/s.css".to_string(), PageTexts::Stylesheet);
        let lookup = |target: &str, sources: &[&str]| texts.lookup(target, sources);
        assert_eq!(
            lookup(
                "https://dead.example",
                &["https://site.example/b", "https://site.example/a"]
            ),
            TextLookup::Text("Dead link")
        );
        assert_eq!(
            lookup("https://other.example/", &["https://site.example/b"]),
            TextLookup::NotFound
        );
        assert_eq!(
            lookup("https://x.example/", &["https://site.example/c"]),
            TextLookup::NotFetched("404")
        );
        assert_eq!(
            lookup("https://x.example/", &["https://site.example/s.css"]),
            TextLookup::Stylesheet
        );
        assert_eq!(
            lookup("https://x.example/", &["https://site.example/new"]),
            TextLookup::Unknown
        );
        assert_eq!(lookup("https://x.example/", &[]), TextLookup::Unknown);
        // A stylesheet that was read: its references have no text, only a position.
        assert_eq!(
            lookup("https://site.example/font.woff2", &["https://site.example/new.css"]),
            TextLookup::Stylesheet
        );
        assert_eq!(
            lookup("https://site.example/other.png", &["https://site.example/new.css"]),
            TextLookup::NotInStylesheet
        );
        // Positions: per page, shown after the page's URL.
        assert_eq!(
            texts.found_on_with_position("https://dead.example", "https://site.example/a"),
            "https://site.example/a:12"
        );
        assert_eq!(
            texts.found_on_with_position("https://site.example/font.woff2", "https://site.example/new.css"),
            "https://site.example/new.css:1:20345"
        );
        assert_eq!(
            texts.found_on_with_position("https://dead.example", "https://site.example/b"),
            "https://site.example/b"
        );
        let first = texts
            .position("https://dead.example", "https://site.example/a")
            .expect("position");
        assert_eq!(first.describe(), "line 12 (and 2 more)");
        assert_eq!(
            texts
                .position("https://site.example/font.woff2", "https://site.example/new.css")
                .map(RefPosition::describe)
                .as_deref(),
            Some("line 1, column 20345")
        );

        let tmp = tempfile::tempdir().expect("temp dir");
        texts.cut = 3;
        texts.write(tmp.path()).expect("written");
        assert_eq!(LinkTexts::read(tmp.path()).expect("read"), Some(texts));
    }

    #[test]
    fn a_file_of_an_earlier_version_still_loads() {
        // As written before positions were kept: no `positions`, stylesheets not fetched.
        let tmp = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            LinkTexts::path(tmp.path()),
            r#"{
  "pages": {
    "https://site.example/a": { "state": "fetched", "texts": { "https://dead.example": "Dead link" } },
    "https://site.example/s.css": { "state": "stylesheet" },
    "https://site.example/c": { "state": "notFetched", "reason": "The page answered 404." }
  },
  "cut": 0,
  "schemaVersion": 1
}
"#,
        )
        .expect("written");
        let texts = LinkTexts::read(tmp.path()).expect("read").expect("a file");
        assert_eq!(
            texts.lookup("https://dead.example/", &["https://site.example/a"]),
            TextLookup::Text("Dead link")
        );
        assert_eq!(texts.position("https://dead.example/", "https://site.example/a"), None);
        assert_eq!(
            texts.lookup("https://site.example/x.png", &["https://site.example/s.css"]),
            TextLookup::Stylesheet
        );
        // A page without positions is written as before (no empty `positions`).
        texts.write(tmp.path()).expect("written");
        let written = std::fs::read_to_string(LinkTexts::path(tmp.path())).expect("read");
        assert!(!written.contains("positions"), "{written}");
        // A position without `count` and `longLine` reads as one reference on a short line.
        let position: RefPosition = serde_json::from_str(r#"{"line":4,"column":2}"#).expect("parsed");
        assert_eq!((position.count, position.long_line), (1, false));
    }
}
