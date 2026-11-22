// SiteOne Crawler - LinkGraph
// (c) Jan Reges <jan.reges@siteone.cz>
//
//! Every link the crawler finds, for `--link-details`.
//!
//! The crawl queue keeps only the first page a URL was found on; later discoveries of a known
//! URL are dropped. Here every (page → URL) link is recorded once per page: up to
//! `MAX_REFERRERS` referring pages per URL with their link text and position, the total number
//! of referring pages, and each page's distinct out-links. URLs are interned once and edges are
//! `u32` ids, so a site with millions of links costs a few bytes per link. A worker collects the
//! links of its page in `PageLinks` and commits them with one short lock of the graph.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::engine::found_url::UrlSource;

/// Referring pages kept per URL (the total count is kept beyond it).
pub const MAX_REFERRERS: usize = 50;

/// Depth of a URL whose link distance is not known.
const UNKNOWN_DEPTH: u32 = u32::MAX;

/// One referring page of a URL: the page, how it referenced the URL, where and with which text.
/// `line` and `column` are 1-based; 0 means unknown.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkRef {
    pub source: u32,
    pub attr: i32,
    pub line: u32,
    pub column: u32,
    pub text: Option<Box<str>>,
}

/// The reference that queued a URL, carried by its queue entry so the `url` event can name it
/// while the URL is crawled.
#[derive(Debug, Clone)]
pub struct FirstRef {
    /// The referring page as the queue knows it.
    pub source_key: Arc<str>,
    /// The referring page as the results show it.
    pub source_url: Arc<str>,
    pub attr: i32,
    pub line: u32,
    pub column: u32,
    pub text: Option<Box<str>>,
}

/// A link found on the page being processed, before it is committed.
#[derive(Debug, Clone)]
pub struct PendingLink {
    /// The target as the crawler tracks it (queue/visited key, or the skipped URL).
    pub target: String,
    pub external: bool,
    pub attr: i32,
    pub offset: Option<usize>,
    pub line: u32,
    pub column: u32,
    pub text: Option<Box<str>>,
}

/// The links of one page while the crawler extracts them.
pub struct PageLinks {
    /// The page as the queue knows it.
    pub key: Arc<str>,
    /// The page as the results show it.
    pub url: Arc<str>,
    /// Link distance of the page from a start URL, as known now.
    pub depth: u32,
    /// Whether links are recorded (`--link-details`); without it only `first_ref` is used.
    collect: bool,
    /// Whether offsets of found URLs refer to the body as received (not to a rendered DOM or a
    /// rewritten body), so that positions can be given.
    positions_valid: bool,
    positions: HashMap<usize, (u32, u32)>,
    links: Vec<PendingLink>,
}

impl PageLinks {
    pub fn new(key: Arc<str>, url: Arc<str>, depth: u32, collect: bool, positions_valid: bool) -> Self {
        Self {
            key,
            url,
            depth,
            collect,
            positions_valid,
            positions: HashMap::new(),
            links: Vec::new(),
        }
    }

    pub fn collects(&self) -> bool {
        self.collect
    }

    /// Computes the line and column of these offsets in `content`, the body the URLs were found
    /// in. Does nothing unless links are recorded and the body is the one received.
    pub fn locate(&mut self, content: &str, offsets: impl Iterator<Item = usize>) {
        if !self.collect || !self.positions_valid {
            return;
        }
        let mut offsets: Vec<usize> = offsets.filter(|offset| !self.positions.contains_key(offset)).collect();
        self.positions.extend(line_columns(content, &mut offsets));
    }

    /// Line and column of a found URL, `(0, 0)` when unknown.
    pub fn position(&self, offset: Option<usize>) -> (u32, u32) {
        offset
            .and_then(|offset| self.positions.get(&offset).copied())
            .unwrap_or((0, 0))
    }

    /// The reference from this page that queues a URL.
    pub fn first_ref(&self, attr: i32, offset: Option<usize>, text: Option<&str>) -> Box<FirstRef> {
        let (line, column) = self.position(offset);
        Box::new(FirstRef {
            source_key: self.key.clone(),
            source_url: self.url.clone(),
            attr,
            line,
            column,
            text: text.map(Box::from),
        })
    }

    /// Records a link from this page (a no-op unless links are recorded).
    pub fn record(&mut self, target: String, external: bool, attr: i32, offset: Option<usize>, text: Option<&str>) {
        if !self.collect {
            return;
        }
        let (line, column) = self.position(offset);
        self.links.push(PendingLink {
            target,
            external,
            attr,
            offset,
            line,
            column,
            text: text.map(Box::from),
        });
    }

    /// The links of the page, one per target: a redirect, otherwise the first occurrence in the
    /// document. Ordered by position, so that the order does not depend on hash iteration.
    pub fn take_links(&mut self) -> Vec<PendingLink> {
        let mut by_target: HashMap<String, PendingLink> = HashMap::with_capacity(self.links.len());
        for link in self.links.drain(..) {
            match by_target.get_mut(&link.target) {
                Some(kept) if precedes(&link, kept) => *kept = link,
                Some(_) => {}
                None => {
                    by_target.insert(link.target.clone(), link);
                }
            }
        }
        let mut links: Vec<PendingLink> = by_target.into_values().collect();
        links.sort_by(|a, b| {
            sort_key(a)
                .cmp(&sort_key(b))
                .then_with(|| a.attr.cmp(&b.attr))
                .then_with(|| a.target.cmp(&b.target))
        });
        links
    }
}

fn is_redirect(link: &PendingLink) -> bool {
    link.attr == UrlSource::Redirect as i32
}

/// Whether `link` is kept instead of `kept` for the same target.
fn precedes(link: &PendingLink, kept: &PendingLink) -> bool {
    if is_redirect(link) != is_redirect(kept) {
        return is_redirect(link);
    }
    (sort_key(link), link.attr) < (sort_key(kept), kept.attr)
}

fn sort_key(link: &PendingLink) -> usize {
    link.offset.unwrap_or(usize::MAX)
}

/// 1-based line and column of each byte offset in `text`. A line ends at `\n`, `\r\n` or a lone
/// `\r`; the column counts characters (Unicode scalar values) from the start of the line. One
/// pass over the text up to the largest offset.
pub fn line_columns(text: &str, offsets: &mut Vec<usize>) -> Vec<(usize, (u32, u32))> {
    offsets.sort_unstable();
    offsets.dedup();
    let bytes = text.as_bytes();
    let mut result = Vec::with_capacity(offsets.len());
    let (mut line, mut column) = (1u32, 1u32);
    let mut next = offsets
        .iter()
        .copied()
        .filter(|offset| *offset <= bytes.len())
        .peekable();
    for (index, byte) in bytes.iter().enumerate() {
        while next.peek() == Some(&index) {
            result.push((index, (line, column)));
            next.next();
        }
        if next.peek().is_none() {
            return result;
        }
        match byte {
            b'\n' => {
                line += 1;
                column = 1;
            }
            b'\r' if bytes.get(index + 1) != Some(&b'\n') => {
                line += 1;
                column = 1;
            }
            b'\r' => {}
            // UTF-8 continuation bytes belong to the character already counted
            byte if byte & 0xC0 == 0x80 => {}
            _ => column += 1,
        }
    }
    for offset in next {
        result.push((offset, (line, column)));
    }
    result
}

struct Node {
    url: Arc<str>,
    /// The URL as the results show it, when it differs from `url`.
    shown: Option<Box<str>>,
    external: bool,
    referrers: Vec<LinkRef>,
    referrer_count: u32,
    /// Distinct targets of the page; `redirects` are those it redirects to.
    out: Vec<u32>,
    redirects: Vec<u32>,
    live_depth: u32,
    /// The reference that queued this URL, once it is crawled.
    first: Option<LinkRef>,
    visited: bool,
}

impl Node {
    fn shown_url(&self) -> &str {
        self.shown.as_deref().unwrap_or(&self.url)
    }
}

/// All links recorded during a crawl (see the module documentation).
#[derive(Default)]
pub struct LinkGraph {
    ids: HashMap<Arc<str>, u32>,
    nodes: Vec<Node>,
    seeds: Vec<u32>,
}

impl LinkGraph {
    pub fn new() -> Self {
        Self::default()
    }

    fn intern(&mut self, url: &str) -> u32 {
        if let Some(id) = self.ids.get(url) {
            return *id;
        }
        let id = self.nodes.len() as u32;
        let url: Arc<str> = Arc::from(url);
        self.nodes.push(Node {
            url: url.clone(),
            shown: None,
            external: false,
            referrers: Vec::new(),
            referrer_count: 0,
            out: Vec::new(),
            redirects: Vec::new(),
            live_depth: UNKNOWN_DEPTH,
            first: None,
            visited: false,
        });
        self.ids.insert(url, id);
        id
    }

    fn set_shown(&mut self, id: u32, shown: &str) {
        let node = &mut self.nodes[id as usize];
        if *node.url != *shown {
            node.shown = Some(Box::from(shown));
        }
    }

    /// A start URL (`--url`, `--url-list`): depth 0.
    pub fn add_seed(&mut self, url: &str) {
        let id = self.intern(url);
        self.nodes[id as usize].live_depth = 0;
        self.seeds.push(id);
    }

    /// Marks the URL `key` as crawled, shown as `shown_url` in the results, and queued by
    /// `first`. Returns its depth as known now.
    pub fn visit(&mut self, key: &str, shown_url: &str, external: bool, first: Option<&FirstRef>) -> Option<u32> {
        let id = self.intern(key);
        self.set_shown(id, shown_url);
        let first = first.map(|first| {
            let source = self.intern(&first.source_key);
            self.set_shown(source, &first.source_url);
            LinkRef {
                source,
                attr: first.attr,
                line: first.line,
                column: first.column,
                text: first.text.clone(),
            }
        });
        let node = &mut self.nodes[id as usize];
        node.visited = true;
        node.external = external;
        if first.is_some() {
            node.first = first;
        }
        (node.live_depth != UNKNOWN_DEPTH).then_some(node.live_depth)
    }

    /// Commits the links of the page `page` (one per target, see `PageLinks::take_links`), whose
    /// depth was `page_depth` when it was crawled.
    pub fn add_page_links(&mut self, page: &str, page_depth: u32, links: Vec<PendingLink>) {
        let source = self.intern(page);
        let source_depth = page_depth.min(self.nodes[source as usize].live_depth);
        // A page is committed once; should it come again, its known targets are not counted twice.
        let known: std::collections::HashSet<u32> = self.nodes[source as usize].out.iter().copied().collect();
        for link in links {
            let target = self.intern(&link.target);
            if known.contains(&target) {
                continue;
            }
            let redirect = link.attr == UrlSource::Redirect as i32;
            let source_node = &mut self.nodes[source as usize];
            source_node.out.push(target);
            if redirect {
                source_node.redirects.push(target);
            }
            let node = &mut self.nodes[target as usize];
            if !node.visited {
                node.external = link.external;
            }
            node.referrer_count += 1;
            if node.referrers.len() < MAX_REFERRERS {
                node.referrers.push(LinkRef {
                    source,
                    attr: link.attr,
                    line: link.line,
                    column: link.column,
                    text: link.text,
                });
            }
            let depth = source_depth.saturating_add(if redirect { 0 } else { 1 });
            node.live_depth = node.live_depth.min(depth);
        }
    }

    /// Shortest link distance of every URL from a start URL over the recorded links (a redirect
    /// adds no distance), by a 0-1 breadth-first search.
    fn final_depths(&self) -> Vec<u32> {
        let mut depths = vec![UNKNOWN_DEPTH; self.nodes.len()];
        let mut queue = VecDeque::new();
        for seed in &self.seeds {
            depths[*seed as usize] = 0;
            queue.push_back(*seed);
        }
        while let Some(id) = queue.pop_front() {
            let node = &self.nodes[id as usize];
            let depth = depths[id as usize];
            for target in &node.out {
                let weight = u32::from(!node.redirects.contains(target));
                if depth + weight < depths[*target as usize] {
                    depths[*target as usize] = depth + weight;
                    if weight == 0 {
                        queue.push_front(*target);
                    } else {
                        queue.push_back(*target);
                    }
                }
            }
        }
        depths
    }

    /// The recorded links for the output: per crawled URL its link facts, and one row per kept
    /// (target, referring page) pair.
    pub fn report(&self) -> LinkReport {
        let depths = self.final_depths();
        let mut urls = HashMap::new();
        let mut rows = Vec::new();
        for (id, node) in self.nodes.iter().enumerate() {
            if node.visited {
                let depth = match depths[id] {
                    UNKNOWN_DEPTH => node.live_depth,
                    depth => depth,
                };
                urls.insert(
                    node.shown_url().to_string(),
                    UrlLinks {
                        first: node.first.as_ref().map(|first| self.row_ref(first)),
                        external: node.external,
                        depth: (depth != UNKNOWN_DEPTH).then_some(depth),
                        in_links: node.referrer_count,
                        out_links: node.out.len() as u32,
                    },
                );
            }

            // The page that queued the URL comes first, as in its `source`.
            let mut referrers: Vec<&LinkRef> = node.referrers.iter().collect();
            if let Some(first) = &node.first
                && let Some(position) = referrers.iter().position(|r| r.source == first.source)
            {
                referrers[..=position].rotate_right(1);
            }
            for referrer in referrers {
                rows.push(LinkRow {
                    url: node.shown_url().to_string(),
                    reference: self.row_ref(referrer),
                    in_links: node.referrer_count,
                    external: node.external,
                });
            }
        }
        LinkReport { urls, rows }
    }

    fn row_ref(&self, link: &LinkRef) -> RowRef {
        RowRef {
            source: self.nodes[link.source as usize].shown_url().to_string(),
            source_attr: source_attr_label(link.attr),
            line: (link.line > 0).then_some(link.line),
            column: (link.column > 0).then_some(link.column),
            text: link.text.as_deref().map(str::to_string),
        }
    }
}

/// How a URL was referenced, as the crawler names it elsewhere (`<a href>`, `css url()`, …).
pub fn source_attr_label(attr: i32) -> &'static str {
    u8::try_from(attr)
        .ok()
        .and_then(UrlSource::from_code)
        .map_or("unknown", |source| source.short_name())
}

/// A reference in the output: the referring page and how, where and with which text.
#[derive(Debug, Clone, PartialEq)]
pub struct RowRef {
    pub source: String,
    pub source_attr: &'static str,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub text: Option<String>,
}

/// The link facts of one crawled URL.
#[derive(Debug, Clone, PartialEq)]
pub struct UrlLinks {
    /// The reference that queued it; none for a start URL.
    pub first: Option<RowRef>,
    pub external: bool,
    pub depth: Option<u32>,
    pub in_links: u32,
    pub out_links: u32,
}

/// One kept (target, referring page) pair.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkRow {
    pub url: String,
    pub reference: RowRef,
    pub in_links: u32,
    pub external: bool,
}

/// What `--link-details` adds to the JSON output.
#[derive(Debug, Default)]
pub struct LinkReport {
    /// Per crawled URL, keyed by the URL of its `results` row.
    pub urls: HashMap<String, UrlLinks>,
    /// Grouped by target in the order the targets were first found; per target in discovery order.
    pub rows: Vec<LinkRow>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(target: &str, attr: UrlSource, offset: Option<usize>) -> PendingLink {
        PendingLink {
            target: target.to_string(),
            external: false,
            attr: attr as i32,
            offset,
            line: 0,
            column: 0,
            text: None,
        }
    }

    fn page(key: &str, collect: bool) -> PageLinks {
        PageLinks::new(Arc::from(key), Arc::from(key), 0, collect, true)
    }

    #[test]
    fn positions_count_lines_and_characters() {
        let text = "ab\ncč d\r\nx\ry";
        let offset_of = |needle: &str| text.find(needle).unwrap();
        let mut offsets = vec![
            offset_of("a"),
            offset_of("d"),
            offset_of("x"),
            offset_of("y"),
            offset_of("č"),
        ];
        let positions: HashMap<usize, (u32, u32)> = line_columns(text, &mut offsets).into_iter().collect();
        assert_eq!(positions[&offset_of("a")], (1, 1));
        assert_eq!(positions[&offset_of("č")], (2, 2));
        // `č` is two bytes but one character
        assert_eq!(positions[&offset_of("d")], (2, 4));
        assert_eq!(positions[&offset_of("x")], (3, 1));
        // a lone \r ends a line too
        assert_eq!(positions[&offset_of("y")], (4, 1));
    }

    #[test]
    fn a_minified_line_gives_the_column() {
        let css = "a{b:url(/x.png)}c{d:url(/y.png)}";
        let mut offsets = vec![css.find("/y.png").unwrap()];
        assert_eq!(line_columns(css, &mut offsets), vec![(24, (1, 25))]);
    }

    #[test]
    fn a_page_keeps_one_link_per_target_the_first_in_the_document() {
        let mut links = page("http://s/", true);
        links.record(
            "http://s/a".into(),
            false,
            UrlSource::AHref as i32,
            Some(50),
            Some("later"),
        );
        links.record("http://s/a".into(), false, UrlSource::ImgSrc as i32, Some(10), None);
        links.record("http://s/b".into(), false, UrlSource::AHref as i32, Some(5), None);
        links.record("http://s/b".into(), false, UrlSource::Redirect as i32, None, None);
        let taken = links.take_links();
        assert_eq!(taken.len(), 2);
        // a redirect wins, and sorts last (no position)
        assert_eq!(taken[0].target, "http://s/a");
        assert_eq!(taken[0].attr, UrlSource::ImgSrc as i32);
        assert_eq!(taken[1].attr, UrlSource::Redirect as i32);
    }

    #[test]
    fn without_link_details_nothing_is_recorded() {
        let mut links = page("http://s/", false);
        links.record("http://s/a".into(), false, UrlSource::AHref as i32, Some(1), None);
        assert!(links.take_links().is_empty());
        links.locate("abc", [1usize].into_iter());
        assert_eq!(links.position(Some(1)), (0, 0));
    }

    #[test]
    fn referrers_are_capped_but_counted() {
        let mut graph = LinkGraph::new();
        graph.add_seed("http://s/");
        for n in 0..(MAX_REFERRERS + 12) {
            graph.add_page_links(
                &format!("http://s/p{n}"),
                1,
                vec![link("http://s/t", UrlSource::AHref, None)],
            );
        }
        let report = graph.report();
        let rows: Vec<&LinkRow> = report.rows.iter().filter(|row| row.url == "http://s/t").collect();
        assert_eq!(rows.len(), MAX_REFERRERS);
        assert_eq!(rows[0].in_links as usize, MAX_REFERRERS + 12);
        assert_eq!(rows[0].reference.source, "http://s/p0", "in discovery order");
        assert_eq!(rows[49].reference.source, "http://s/p49");
    }

    #[test]
    fn a_page_committed_twice_does_not_count_twice() {
        let mut graph = LinkGraph::new();
        graph.add_page_links("http://s/", 0, vec![link("http://s/t", UrlSource::AHref, None)]);
        graph.add_page_links("http://s/", 0, vec![link("http://s/t", UrlSource::AHref, None)]);
        assert_eq!(graph.report().rows.len(), 1);
    }

    #[test]
    fn depth_is_the_shortest_link_distance_and_redirects_add_none() {
        let mut graph = LinkGraph::new();
        graph.add_seed("http://s/");
        graph.visit("http://s/", "http://s/", false, None);
        // /c was crawled while it was known only at depth 5, and committed its link to /deep
        // before the start page committed its link to /c
        graph.add_page_links("http://s/c", 5, vec![link("http://s/deep", UrlSource::AHref, None)]);
        graph.add_page_links(
            "http://s/",
            0,
            vec![
                link("http://s/c", UrlSource::AHref, None),
                link("http://s/old", UrlSource::AHref, None),
            ],
        );
        graph.add_page_links("http://s/old", 1, vec![link("http://s/new", UrlSource::Redirect, None)]);
        graph.add_page_links("http://s/new", 1, vec![link("http://s/deep", UrlSource::AHref, None)]);
        for url in ["http://s/deep", "http://s/new", "http://s/c"] {
            graph.visit(url, url, false, None);
        }
        let report = graph.report();
        assert_eq!(report.urls["http://s/"].depth, Some(0));
        assert_eq!(report.urls["http://s/c"].depth, Some(1));
        assert_eq!(
            report.urls["http://s/new"].depth,
            Some(1),
            "a redirect adds no distance"
        );
        assert_eq!(
            report.urls["http://s/deep"].depth,
            Some(2),
            "not the 6 known during the crawl"
        );
        assert_eq!(report.urls["http://s/deep"].in_links, 2);
        assert_eq!(report.urls["http://s/"].out_links, 2);
    }

    #[test]
    fn the_page_that_queued_a_url_is_its_first_referrer() {
        let mut graph = LinkGraph::new();
        graph.add_page_links("http://s/x", 1, vec![link("http://s/t", UrlSource::AHref, None)]);
        graph.add_page_links("http://s/y", 1, vec![link("http://s/t", UrlSource::AHref, None)]);
        let first = FirstRef {
            source_key: Arc::from("http://s/y"),
            source_url: Arc::from("http://s/y"),
            attr: UrlSource::AHref as i32,
            line: 3,
            column: 7,
            text: Some(Box::from("Why")),
        };
        assert_eq!(graph.visit("http://s/t", "http://s/t", false, Some(&first)), Some(2));
        let report = graph.report();
        let facts = &report.urls["http://s/t"];
        let source = facts.first.as_ref().unwrap();
        assert_eq!(source.source, "http://s/y");
        assert_eq!((source.line, source.column), (Some(3), Some(7)));
        assert_eq!(source.text.as_deref(), Some("Why"));
        let rows: Vec<&str> = report.rows.iter().map(|row| row.reference.source.as_str()).collect();
        assert_eq!(rows, ["http://s/y", "http://s/x"]);
    }

    #[test]
    fn attribute_labels_are_the_crawlers_own() {
        assert_eq!(source_attr_label(UrlSource::AHref as i32), "<a href>");
        assert_eq!(source_attr_label(UrlSource::CssUrl as i32), "css url()");
        assert_eq!(source_attr_label(-1), "unknown");
    }
}
