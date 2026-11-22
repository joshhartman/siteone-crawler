//! Link data from the crawler (GUI-SPEC §4.8): which page referenced a URL, how, with which link
//! text and where in that page.
//!
//! Two sources, both optional (a crawler before link data reports neither):
//!
//! - the `url` event of every crawled URL names its first referrer — `source`, `sourceAttr`, and
//!   with `--link-details` `linkText`, `sourceLine`, `sourceColumn` — as the URL arrives
//!   ([`CrawlerRef`] on the URL row);
//! - with `--link-details` the result JSON's `links` table lists every referring page of every URL
//!   the crawler found, crawled or not (up to 50 per URL, the queueing page first, with the total),
//!   and its `results` rows carry the depth and the in and out links of each crawled URL
//!   ([`LinkGraph`], built when the result is loaded, on a worker).
//!
//! Where the crawler gave the position of a reference, the app does not read that page again
//! ([`CrawlerRef::needs_fetch`]); its own reading of the page (`backend::link_text`) is the
//! fallback for runs without link details and for references without a position (pages rendered
//! with `--browser`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::link_text::{self, LONG_LINE_CHARS, LinkTexts, RefPosition, TextLookup};

/// The `sourceAttr` of a link (`<a href>`).
pub const ATTR_LINK: &str = "<a href>";

/// One reference to a URL, as the crawler reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrawlerRef {
    /// The referring page (or stylesheet, script, sitemap, redirecting URL).
    pub source: Arc<str>,
    /// How it referenced the URL: `<a href>`, `<img src>`, `css url()`, `redirect`, …
    pub attr: Arc<str>,
    /// The link text (`--link-details`; none for CSS, scripts, redirects, an empty `alt`).
    pub text: Option<Box<str>>,
    /// 1-based line and column (characters) of the reference in the source as received
    /// (`--link-details`; none for a `--browser`-rendered page, a sitemap or a script).
    pub line: Option<u32>,
    pub column: Option<u32>,
}

impl CrawlerRef {
    /// Where the source references the URL. The crawler does not report the length of the line: a
    /// reference more than [`LONG_LINE_CHARS`] characters into its line is on a long line (a
    /// minified file) and is shown with its column (`:1:20345`), any other with its line only.
    pub fn position(&self) -> Option<RefPosition> {
        let line = self.line?;
        let column = self.column.unwrap_or(1);
        Some(RefPosition {
            line,
            column,
            count: 1,
            long_line: column as usize > LONG_LINE_CHARS,
            crawled: true,
        })
    }

    /// The link text: the crawler's, "in a stylesheet" for a reference in a stylesheet, else how
    /// the page referenced the URL (`<script src>`, `redirect`, …).
    pub fn text_lookup(&self) -> TextLookup<'_> {
        match self.text.as_deref() {
            Some(text) => TextLookup::Text(text),
            None if link_text::is_stylesheet(&self.source) => TextLookup::Stylesheet,
            None => TextLookup::Text(&self.attr),
        }
    }

    /// Whether the app has to read the source itself to find where it references the URL: the
    /// crawler gave no position, and the reference is one the app's reading can find — an HTML
    /// attribute or a stylesheet's `url()`. References found in scripts (`js url`), sitemaps and
    /// redirects are not in what the app reads.
    pub fn needs_fetch(&self) -> bool {
        self.line.is_none() && (self.attr.starts_with('<') || &*self.attr == "css url()")
    }

    /// Whether the crawler's data answers for this reference (text and position need no fetch).
    pub fn is_complete(&self) -> bool {
        !self.needs_fetch()
    }
}

/// Every kept referrer of one URL, and how many pages reference it in all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetRefs {
    pub url: Arc<str>,
    /// In the crawler's order: the page that queued the URL first. At most 50.
    pub refs: Vec<CrawlerRef>,
    /// Distinct pages that reference the URL, also beyond the kept ones.
    pub in_links: u32,
    /// Not on the initial host (`www.` aside).
    pub external: bool,
}

impl TargetRefs {
    /// Referring pages beyond the kept ones ("and 312 more").
    pub fn more(&self) -> usize {
        (self.in_links as usize).saturating_sub(self.refs.len())
    }

    /// The reference of `page` (a trailing `/` aside).
    pub fn on(&self, page: &str) -> Option<&CrawlerRef> {
        self.refs.iter().find(|r| &*r.source == page).or_else(|| {
            let key = page.trim().trim_end_matches('/');
            self.refs.iter().find(|r| r.source.trim().trim_end_matches('/') == key)
        })
    }

    /// Whether any page links to it with `<a href>`.
    pub fn is_link(&self) -> bool {
        self.refs.iter().any(|r| &*r.attr == ATTR_LINK)
    }
}

/// The link facts of a crawled URL (`results` rows with `--link-details`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UrlFacts {
    /// Shortest link distance from a start URL.
    pub depth: Option<u32>,
    pub in_links: Option<u32>,
    pub out_links: Option<u32>,
}

/// The crawler's `links` table and link facts of a run (`--link-details`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkGraph {
    targets: Vec<TargetRefs>,
    /// Target URL → position in `targets`.
    index: HashMap<Arc<str>, u32>,
    /// Crawled URL → its facts.
    facts: HashMap<Box<str>, UrlFacts>,
    /// Rows of the `links` table.
    rows: usize,
}

/// One row of the `links` table, as read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkRow {
    pub url: String,
    pub source_url: String,
    pub source_attr: String,
    pub link_text: String,
    pub source_line: Option<u32>,
    pub source_column: Option<u32>,
    pub in_links: Option<u32>,
    pub external: bool,
}

/// Shares equal strings (the referring pages and attributes repeat on many rows).
#[derive(Debug, Clone, Default)]
pub struct Interner(HashSet<Arc<str>>);

impl Interner {
    pub fn get(&mut self, text: &str) -> Arc<str> {
        if let Some(known) = self.0.get(text) {
            return Arc::clone(known);
        }
        let shared: Arc<str> = Arc::from(text);
        self.0.insert(Arc::clone(&shared));
        shared
    }
}

/// Builds a [`LinkGraph`] row by row, so that the `links` table is never held as read (a result
/// JSON can have hundreds of thousands of rows).
#[derive(Debug, Default)]
pub struct LinkGraphBuilder {
    graph: LinkGraph,
    strings: Interner,
}

impl LinkGraphBuilder {
    /// Adds a row of the `links` table (rows come in table order).
    pub fn add(&mut self, row: LinkRow) {
        let graph = &mut self.graph;
        let url = row.url.trim();
        let source = row.source_url.trim();
        if url.is_empty() || source.is_empty() {
            return;
        }
        let at = match graph.index.get(url) {
            Some(at) => *at as usize,
            None => {
                let Ok(at) = u32::try_from(graph.targets.len()) else {
                    return;
                };
                let url = self.strings.get(url);
                graph.index.insert(Arc::clone(&url), at);
                graph.targets.push(TargetRefs {
                    url,
                    external: row.external,
                    ..TargetRefs::default()
                });
                at as usize
            }
        };
        let Some(target) = graph.targets.get_mut(at) else {
            return;
        };
        target.refs.push(CrawlerRef {
            source: self.strings.get(source),
            attr: self.strings.get(row.source_attr.trim()),
            text: Some(row.link_text.trim())
                .filter(|text| !text.is_empty())
                .map(Box::from),
            line: row.source_line.filter(|line| *line > 0),
            column: row.source_column.filter(|column| *column > 0),
        });
        let kept = u32::try_from(target.refs.len()).unwrap_or(u32::MAX);
        target.in_links = target.in_links.max(row.in_links.unwrap_or(0)).max(kept);
        target.external |= row.external;
        graph.rows += 1;
    }

    /// The graph, with the facts of the `results` rows.
    pub fn finish(self, facts: impl IntoIterator<Item = (String, UrlFacts)>) -> LinkGraph {
        let mut graph = self.graph;
        graph.facts = facts
            .into_iter()
            .filter(|(url, facts)| !url.is_empty() && *facts != UrlFacts::default())
            .map(|(url, facts)| (Box::from(url.as_str()), facts))
            .collect();
        graph
    }
}

impl LinkGraph {
    /// The graph of the `links` rows (in table order) and the facts of the `results` rows.
    pub fn build(rows: impl IntoIterator<Item = LinkRow>, facts: impl IntoIterator<Item = (String, UrlFacts)>) -> Self {
        let mut builder = LinkGraphBuilder::default();
        for row in rows {
            builder.add(row);
        }
        builder.finish(facts)
    }

    /// Whether the run reported nothing (no `links` table, no facts).
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty() && self.facts.is_empty()
    }

    /// Rows of the `links` table.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The referrers of `url` (as the crawler names it).
    pub fn target(&self, url: &str) -> Option<&TargetRefs> {
        let at = self.index.get(url.trim()).copied()?;
        self.targets.get(at as usize)
    }

    /// Every URL with its referrers, in the order the crawler found them.
    pub fn targets(&self) -> &[TargetRefs] {
        &self.targets
    }

    /// The link facts of a crawled URL.
    pub fn facts(&self, url: &str) -> Option<UrlFacts> {
        self.facts.get(url).copied()
    }

    /// Whether the `results` rows carry link facts.
    pub fn has_facts(&self) -> bool {
        !self.facts.is_empty()
    }
}

/// A page that references a URL, with what the crawler reported about that reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Referrer<'a> {
    pub page: &'a str,
    pub crawler: Option<&'a CrawlerRef>,
}

impl<'a> Referrer<'a> {
    /// A page known without the crawler's details (the `404` table, the external link check).
    pub fn page(page: &'a str) -> Self {
        Self { page, crawler: None }
    }

    pub fn of(reference: &'a CrawlerRef) -> Self {
        Self {
            page: &reference.source,
            crawler: Some(reference),
        }
    }

    /// The crawler's reference when its data answers for it (no fetch needed).
    fn complete(&self) -> Option<&'a CrawlerRef> {
        self.crawler
            .filter(|reference| reference.is_complete() || reference.text.is_some())
    }

    /// Where the page references `target`: the crawler's position, else the one the app read.
    pub fn position(&self, texts: &LinkTexts, target: &str) -> Option<RefPosition> {
        self.crawler
            .and_then(CrawlerRef::position)
            .or_else(|| texts.position(target, self.page).copied())
    }
}

/// The link text of `target` on the first of `referrers` that has one (else what the first one
/// says): the crawler's where it answers, else what the app read on the page.
pub fn text_on<'a>(texts: &'a LinkTexts, target: &str, referrers: &[Referrer<'a>]) -> TextLookup<'a> {
    let mut first = None;
    for referrer in referrers {
        let found = match referrer.complete() {
            Some(reference) => reference.text_lookup(),
            None => texts.lookup(target, &[referrer.page]),
        };
        if matches!(found, TextLookup::Text(_)) {
            return found;
        }
        first.get_or_insert(found);
    }
    first.unwrap_or(TextLookup::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(url: &str, source: &str, attr: &str, text: &str, line: Option<u32>, in_links: u32) -> LinkRow {
        LinkRow {
            url: url.to_string(),
            source_url: source.to_string(),
            source_attr: attr.to_string(),
            link_text: text.to_string(),
            source_line: line,
            source_column: line.map(|_| 9),
            in_links: Some(in_links),
            external: url.contains("other.example"),
        }
    }

    #[test]
    fn rows_group_by_target_with_shared_strings_and_the_total() {
        let graph = LinkGraph::build(
            [
                row(
                    "https://site.example/gone",
                    "https://site.example/",
                    "<a href>",
                    "Gone",
                    Some(12),
                    3,
                ),
                row(
                    "https://site.example/gone",
                    "https://site.example/a",
                    "<a href>",
                    "",
                    None,
                    3,
                ),
                row(
                    "https://other.example/",
                    "https://site.example/a",
                    "<a href>",
                    "Partner",
                    Some(4),
                    1,
                ),
                row("", "https://site.example/", "<a href>", "", None, 1),
            ],
            [(
                "https://site.example/gone".to_string(),
                UrlFacts {
                    depth: Some(1),
                    in_links: Some(3),
                    out_links: Some(0),
                },
            )],
        );
        assert_eq!(graph.rows(), 3);
        let gone = graph.target("https://site.example/gone").expect("target");
        assert_eq!((gone.refs.len(), gone.in_links, gone.more()), (2, 3, 1));
        assert!(Arc::ptr_eq(
            &gone.refs[1].source,
            &graph.target("https://other.example/").expect("external").refs[0].source
        ));
        assert!(gone.on("https://site.example/a/").is_some(), "a trailing slash aside");
        assert_eq!(gone.refs[1].text, None);
        assert!(
            graph
                .target("https://other.example/")
                .is_some_and(|t| t.external && t.is_link())
        );
        assert_eq!(graph.facts("https://site.example/gone").and_then(|f| f.depth), Some(1));
        assert!(graph.facts("https://site.example/").is_none());
    }

    #[test]
    fn texts_and_positions_come_from_the_crawler_where_it_answers() {
        let reference =
            |attr: &str, text: Option<&str>, line: Option<u32>, column: Option<u32>, source: &str| CrawlerRef {
                source: Arc::from(source),
                attr: Arc::from(attr),
                text: text.map(Box::from),
                line,
                column,
            };
        let link = reference("<a href>", Some("Gone"), Some(45), Some(257), "https://site.example/");
        let position = link.position().expect("position");
        assert_eq!((position.suffix(), position.crawled), (":45".to_string(), true));
        let minified = reference(
            "css url()",
            None,
            Some(1),
            Some(20345),
            "https://site.example/s.min.css",
        );
        assert_eq!(minified.position().map(|p| p.suffix()).as_deref(), Some(":1:20345"));
        assert_eq!(minified.text_lookup(), TextLookup::Stylesheet);
        let script = reference("<script src>", None, Some(3), Some(14), "https://site.example/");
        assert_eq!(script.text_lookup(), TextLookup::Text("<script src>"));
        assert!(!link.needs_fetch() && !minified.needs_fetch());
        // Rendered with --browser: no position, the app reads the page; never a script's reference.
        let rendered = reference("<a href>", Some("Gone"), None, None, "https://site.example/b");
        assert!(rendered.needs_fetch());
        assert!(!reference("js url", None, None, None, "https://site.example/app.js").needs_fetch());
        assert!(!reference("redirect", None, None, None, "https://site.example/old").needs_fetch());

        // The crawler's text wins; without crawler data the page the app read answers.
        let mut texts = LinkTexts::default();
        texts.pages.insert(
            "https://site.example/c".to_string(),
            link_text::PageTexts::texts(std::collections::BTreeMap::from([(
                link_text::url_key("https://site.example/gone"),
                "Read by the app".to_string(),
            )])),
        );
        let target = "https://site.example/gone";
        assert_eq!(
            text_on(
                &texts,
                target,
                &[Referrer::page("https://site.example/c"), Referrer::of(&link)]
            ),
            TextLookup::Text("Read by the app")
        );
        assert_eq!(
            text_on(&texts, target, &[Referrer::of(&link)]),
            TextLookup::Text("Gone")
        );
        // Without link details (no text, no position): what the app read, or nothing yet.
        let bare = reference("<a href>", None, None, None, "https://site.example/d");
        assert_eq!(text_on(&texts, target, &[Referrer::of(&bare)]), TextLookup::Unknown);
        assert_eq!(Referrer::of(&bare).position(&texts, target), None);
        assert_eq!(text_on(&texts, target, &[]), TextLookup::Unknown);
    }
}
