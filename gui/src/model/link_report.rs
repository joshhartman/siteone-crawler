//! The link report of a run (GUI-SPEC §4.10): the app's own report of the run's links —
//! `<run>/link-report.html` — with what the crawler's reports do not have: the external link check,
//! link texts and the newest answers of retries. One self-contained HTML file: no script, no external resource, light
//! and dark by `prefers-color-scheme`, printable.
//!
//! This module renders a [`LinkReport`] that the app has gathered from the run's state. Every value
//! from the web is HTML-escaped; only `http` and `https` URLs become links.

use std::io;
use std::path::{Path, PathBuf};

use super::storage;

pub const REPORT_FILE: &str = "link-report.html";

/// One row of a group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportRow {
    /// The status in words or the code ("404", "Timeout", "Refused 403").
    pub status: String,
    pub url: String,
    /// The page title of a checked link, or the kind of a crawled URL ("Image").
    pub title_or_kind: String,
    pub link_text: String,
    /// Every page it was found on.
    pub found_on: Vec<String>,
}

/// A table of the report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportGroup {
    pub title: String,
    /// Shown folded (`<details>`), e.g. the working external links.
    pub folded: bool,
    pub rows: Vec<ReportRow>,
}

/// One page and what is broken on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageGroup<T> {
    /// `None`: the page is not known (the last group).
    pub page: Option<String>,
    pub items: Vec<T>,
}

/// Groups items by the pages they were found on (Xenu's "broken links, ordered by page"): an item
/// found on several pages is in the group of each, one without a known page in the last group.
/// Pages with more items come first, then by URL; the items keep their order. Pages are told apart
/// by URL without a trailing slash and shown as first met.
pub fn group_by_page<T>(items: impl IntoIterator<Item = (T, Vec<String>)>) -> Vec<PageGroup<T>>
where
    T: Clone,
{
    let mut groups: Vec<PageGroup<T>> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut unknown = Vec::new();
    for (item, pages) in items {
        let mut placed = false;
        let mut seen = Vec::new();
        for page in &pages {
            let page = page.trim();
            let key = page.trim_end_matches('/');
            if page.is_empty() || seen.contains(&key) {
                continue;
            }
            seen.push(key);
            placed = true;
            let at = *index.entry(key.to_string()).or_insert_with(|| {
                groups.push(PageGroup {
                    page: Some(page.to_string()),
                    items: Vec::new(),
                });
                groups.len() - 1
            });
            if let Some(group) = groups.get_mut(at) {
                group.items.push(item.clone());
            }
        }
        if !placed {
            unknown.push(item);
        }
    }
    groups.sort_by(|a, b| b.items.len().cmp(&a.items.len()).then_with(|| a.page.cmp(&b.page)));
    if !unknown.is_empty() {
        groups.push(PageGroup {
            page: None,
            items: unknown,
        });
    }
    groups
}

/// What the report says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkReport {
    pub site: String,
    /// When the site was crawled, for people.
    pub crawled_at: Option<String>,
    /// When the report was written, for people.
    pub written_at: String,
    /// `(label, count)` in the order shown.
    pub counts: Vec<(String, u64)>,
    /// *What was checked*: `(label, text)` — the scope, the crawled URLs and the external links by
    /// answer.
    pub checked: Vec<(String, String)>,
    /// *Not checked*: one sentence each.
    pub not_checked: Vec<String>,
    /// Groups without rows are left out.
    pub groups: Vec<ReportGroup>,
    /// The broken links by page; left out when empty (no page is known).
    pub by_page: Vec<PageGroup<ReportRow>>,
}

impl LinkReport {
    pub fn path(run_dir: &Path) -> PathBuf {
        run_dir.join(REPORT_FILE)
    }

    /// Writes the report atomically into the run folder, never re-creating a deleted one.
    pub fn write(&self, run_dir: &Path) -> io::Result<()> {
        storage::write_atomic_in_existing_dir(&Self::path(run_dir), self.render().as_bytes())
    }

    /// The HTML of the report.
    pub fn render(&self) -> String {
        let mut html = String::with_capacity(16 * 1024);
        html.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
        html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
        html.push_str(&format!("<title>Link report · {}</title>\n", escape(&self.site)));
        html.push_str(STYLE);
        html.push_str("</head>\n<body>\n<header>\n<h1>Link report</h1>\n");
        html.push_str(&format!(
            "<p class=\"site\">{}</p>\n<p class=\"meta\">",
            link(&self.site)
        ));
        if let Some(crawled) = &self.crawled_at {
            html.push_str(&format!("Crawled {} · ", escape(crawled)));
        }
        html.push_str(&format!(
            "Report written {} by SiteOne Crawler GUI</p>\n</header>\n",
            escape(&self.written_at)
        ));
        if !self.counts.is_empty() {
            html.push_str("<ul class=\"counts\">\n");
            for (label, count) in &self.counts {
                html.push_str(&format!("<li><strong>{}</strong> {}</li>\n", count, escape(label)));
            }
            html.push_str("</ul>\n");
        }
        self.checked_html(&mut html);
        let shown: Vec<&ReportGroup> = self.groups.iter().filter(|group| !group.rows.is_empty()).collect();
        if shown.is_empty() {
            html.push_str("<p class=\"empty\">No broken or refused links.</p>\n");
        }
        for group in shown {
            group_html(&mut html, group);
        }
        if !self.by_page.is_empty() {
            html.push_str("<section class=\"by-page\">\n<h2>Broken links by page</h2>\n");
            html.push_str("<p class=\"note\">Each page with the broken and refused links found on it; a link found on several pages is listed under each.</p>\n");
            for group in &self.by_page {
                page_html(&mut html, group);
            }
            html.push_str("</section>\n");
        }
        html.push_str("</body>\n</html>\n");
        html
    }

    /// *What was checked* and *Not checked*.
    fn checked_html(&self, html: &mut String) {
        if self.checked.is_empty() && self.not_checked.is_empty() {
            return;
        }
        html.push_str("<section class=\"checked\">\n<h2>What was checked</h2>\n<table class=\"facts\">\n");
        for (label, text) in &self.checked {
            html.push_str(&format!(
                "<tr><th>{}</th><td>{}</td></tr>\n",
                escape(label),
                escape(text)
            ));
        }
        html.push_str("<tr><th>Not checked</th><td>");
        if self.not_checked.is_empty() {
            html.push_str("Nothing that the crawl's data names.");
        } else {
            html.push_str("<ul>");
            for gap in &self.not_checked {
                html.push_str(&format!("<li>{}</li>", escape(gap)));
            }
            html.push_str("</ul>");
        }
        html.push_str("</td></tr>\n</table>\n</section>\n");
    }
}

/// The same column widths in every table, so that the tables line up.
const COLUMNS: &str = "<colgroup><col class=\"c-status\"><col class=\"c-url\"><col class=\"c-title\"><col class=\"c-text\"><col></colgroup>\n";

fn group_html(html: &mut String, group: &ReportGroup) {
    let heading = format!("{} <span class=\"n\">{}</span>", escape(&group.title), group.rows.len());
    if group.folded {
        html.push_str(&format!("<details>\n<summary><h2>{heading}</h2></summary>\n"));
    } else {
        html.push_str(&format!("<section>\n<h2>{heading}</h2>\n"));
    }
    html.push_str("<table>\n");
    html.push_str(COLUMNS);
    html.push_str("<thead><tr><th>Status</th><th>URL</th><th>Title or kind</th><th>Link text</th><th>Found on</th></tr></thead>\n<tbody>\n");
    for row in &group.rows {
        let found_on: Vec<String> = row.found_on.iter().map(|page| link(page)).collect();
        html.push_str(&format!(
            "<tr><td class=\"status\">{}</td><td class=\"url\">{}</td><td>{}</td><td>{}</td><td class=\"url\">{}</td></tr>\n",
            escape(&row.status),
            link(&row.url),
            escape(&row.title_or_kind),
            escape(&row.link_text),
            found_on.join("<br>")
        ));
    }
    html.push_str("</tbody>\n</table>\n");
    html.push_str(if group.folded { "</details>\n" } else { "</section>\n" });
}

/// A page of *Broken links by page*: the page as the heading, its links without *Found on*.
fn page_html(html: &mut String, group: &PageGroup<ReportRow>) {
    let page = match &group.page {
        Some(page) => link(page),
        None => "Page not reported by the crawler".to_string(),
    };
    html.push_str(&format!(
        "<h3>{page} <span class=\"n\">{}</span></h3>\n",
        group.items.len()
    ));
    if group.page.is_none() {
        html.push_str("<p class=\"note\">The crawler reports the page a link was found on only for a 404, and only the first such page.</p>\n");
    }
    html.push_str("<table>\n<colgroup><col class=\"c-status\"><col class=\"c-url-wide\"><col class=\"c-title-wide\"><col></colgroup>\n");
    html.push_str(
        "<thead><tr><th>Status</th><th>URL</th><th>Title or kind</th><th>Link text</th></tr></thead>\n<tbody>\n",
    );
    for row in &group.items {
        html.push_str(&format!(
            "<tr><td class=\"status\">{}</td><td class=\"url\">{}</td><td>{}</td><td>{}</td></tr>\n",
            escape(&row.status),
            link(&row.url),
            escape(&row.title_or_kind),
            escape(&row.link_text)
        ));
    }
    html.push_str("</tbody>\n</table>\n");
}

/// Escapes text for HTML content and attribute values.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// A link for an `http`/`https` URL, the escaped text for anything else.
fn link(url: &str) -> String {
    let is_web = url::Url::parse(url.trim()).is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https"));
    if is_web {
        format!("<a href=\"{0}\">{0}</a>", escape(url.trim()))
    } else {
        escape(url)
    }
}

const STYLE: &str = r#"<style>
:root { --bg: #ffffff; --text: #1d232b; --weak: #5d6773; --border: #dde2e8; --alt: #f4f6f8; --accent: #1f6feb; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #16191d; --text: #e6e9ed; --weak: #9aa4b0; --border: #2f353d; --alt: #1d2126; --accent: #6ea8fe; }
}
* { box-sizing: border-box; }
body { margin: 0 auto; max-width: 1400px; padding: 24px 16px 48px; background: var(--bg); color: var(--text);
  font: 14px/1.45 "Segoe UI", system-ui, -apple-system, sans-serif; }
h1 { font-size: 22px; margin: 0 0 4px; }
h2 { font-size: 16px; margin: 24px 0 8px; display: inline-block; }
.n { color: var(--weak); font-weight: normal; }
.site { margin: 0; font-size: 15px; }
.meta, .note, .empty { color: var(--weak); }
.counts { list-style: none; padding: 0; display: flex; flex-wrap: wrap; gap: 8px 20px; }
table { width: 100%; border-collapse: collapse; table-layout: fixed; }
.c-status { width: 150px; }
.c-url { width: 36%; }
.c-title, .c-text { width: 15%; }
.c-url-wide { width: 48%; }
.c-title-wide { width: 18%; }
h3 { font-size: 14px; margin: 18px 0 6px; font-weight: 600; overflow-wrap: anywhere; }
.facts { table-layout: auto; margin-bottom: 8px; }
.facts th { width: 150px; background: none; vertical-align: top; }
.facts ul { margin: 0; padding-left: 18px; }
.facts li + li { margin-top: 2px; }
th, td { text-align: left; vertical-align: top; padding: 5px 8px; border-bottom: 1px solid var(--border);
  overflow-wrap: anywhere; }
th { color: var(--weak); font-weight: 600; background: var(--alt); }
td.url { word-break: break-all; }
td.status { white-space: nowrap; }
a { color: var(--accent); }
summary { cursor: pointer; }
@media print {
  :root { --bg: #ffffff; --text: #000000; --weak: #444444; --border: #bbbbbb; --alt: #eeeeee; --accent: #000000; }
  details > summary { list-style: none; }
  tr { break-inside: avoid; }
}
</style>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> LinkReport {
        LinkReport {
            site: "https://site.example/".to_string(),
            crawled_at: Some("19 Nov 2026, 14:00".to_string()),
            written_at: "19 Nov 2026, 14:32".to_string(),
            counts: vec![
                ("broken internal pages".to_string(), 1),
                ("refused external links".to_string(), 1),
            ],
            groups: vec![
                ReportGroup {
                    title: "Broken internal pages".to_string(),
                    rows: vec![ReportRow {
                        status: "404".to_string(),
                        url: "https://site.example/<script>alert(1)</script>".to_string(),
                        title_or_kind: "Page".to_string(),
                        link_text: "Say \"hi\" & <b>bye</b>".to_string(),
                        found_on: vec![
                            "javascript:alert('x')".to_string(),
                            "https://site.example/a?x=1&y=2".to_string(),
                        ],
                    }],
                    ..ReportGroup::default()
                },
                ReportGroup {
                    title: "Refused external links".to_string(),
                    rows: vec![ReportRow {
                        status: "Refused 403".to_string(),
                        url: "https://other.example/".to_string(),
                        title_or_kind: "<script>x</script>'quoted'".to_string(),
                        ..ReportRow::default()
                    }],
                    ..ReportGroup::default()
                },
                ReportGroup {
                    title: "Working external links".to_string(),
                    folded: true,
                    ..ReportGroup::default()
                },
            ],
            checked: vec![(
                "Crawled URLs".to_string(),
                "73: 67 OK (2xx), 6 client errors (4xx)".to_string(),
            )],
            not_checked: vec!["2 external files are not checked.".to_string()],
            by_page: vec![PageGroup {
                page: Some("https://site.example/a?x=1&y=2".to_string()),
                items: vec![ReportRow {
                    status: "404".to_string(),
                    url: "https://site.example/<b>gone</b>".to_string(),
                    title_or_kind: "Page".to_string(),
                    link_text: "<i>Gone</i>".to_string(),
                    found_on: vec!["https://site.example/a?x=1&y=2".to_string()],
                }],
            }],
        }
    }

    #[test]
    fn links_are_grouped_by_every_page_they_were_found_on() {
        let pages = |list: &[&str]| list.iter().map(|page| page.to_string()).collect::<Vec<_>>();
        let groups = group_by_page([
            ("a", pages(&["https://s.example/x", "https://s.example/y/"])),
            ("b", pages(&["https://s.example/y"])),
            ("c", pages(&[])),
            ("d", pages(&["https://s.example/z", "https://s.example/z"])),
            ("e", pages(&["https://s.example/y/", ""])),
        ]);
        let shown: Vec<(Option<&str>, Vec<&str>)> = groups
            .iter()
            .map(|group| (group.page.as_deref(), group.items.clone()))
            .collect();
        assert_eq!(
            shown,
            [
                // Most links first; `y` and `y/` are one page, shown as first met.
                (Some("https://s.example/y/"), vec!["a", "b", "e"]),
                (Some("https://s.example/x"), vec!["a"]),
                (Some("https://s.example/z"), vec!["d"]),
                // The page is not known: the last group.
                (None, vec!["c"]),
            ]
        );
        assert!(group_by_page(Vec::<(u8, Vec<String>)>::new()).is_empty());
    }

    #[test]
    fn the_report_says_what_was_checked_and_lists_the_broken_links_by_page() {
        let html = report().render();
        let checked = html.find("What was checked").expect("the block");
        let first_group = html.find("Broken internal pages").expect("a group");
        let by_page = html.find("Broken links by page").expect("the section");
        assert!(checked < first_group && first_group < by_page, "{html}");
        assert!(html.contains("<tr><th>Crawled URLs</th><td>73: 67 OK (2xx), 6 client errors (4xx)</td></tr>"));
        assert!(html.contains("<li>2 external files are not checked.</li>"));
        // The page is the heading (a link), escaped like every value from the web.
        assert!(html.contains(
            "<h3><a href=\"https://site.example/a?x=1&amp;y=2\">https://site.example/a?x=1&amp;y=2</a> <span class=\"n\">1</span></h3>"
        ));
        assert!(html.contains("&lt;i&gt;Gone&lt;/i&gt;") && !html.contains("<i>Gone"));
        // Nothing broken: no section, and an empty gap list says so.
        let quiet = LinkReport {
            site: "https://site.example/".to_string(),
            checked: vec![("Crawled URLs".to_string(), "5: 5 OK (2xx)".to_string())],
            ..LinkReport::default()
        };
        let html = quiet.render();
        assert!(!html.contains("Broken links by page"));
        assert!(html.contains("Nothing that the crawl's data names."));
        assert!(html.contains("No broken or refused links."));
        let unknown = LinkReport {
            by_page: vec![PageGroup {
                page: None,
                items: vec![ReportRow::default()],
            }],
            ..LinkReport::default()
        };
        assert!(unknown.render().contains("Page not reported by the crawler"));
    }

    #[test]
    fn every_value_from_the_web_is_escaped_and_only_web_urls_are_links() {
        let html = report().render();
        assert!(!html.contains("<script>"), "{html}");
        assert!(!html.contains("<b>bye"), "{html}");
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("Say &quot;hi&quot; &amp; &lt;b&gt;bye&lt;/b&gt;"));
        assert!(html.contains("&#39;quoted&#39;"));
        assert!(!html.contains("href=\"javascript:"), "{html}");
        assert!(html.contains("javascript:alert(&#39;x&#39;)"));
        assert!(html.contains("<a href=\"https://site.example/a?x=1&amp;y=2\">"));
        // Self-contained: no script, no external resource.
        assert!(!html.contains("<script") && !html.contains("<link") && !html.contains("src="));
        assert!(html.contains("prefers-color-scheme: dark") && html.contains("@media print"));
    }

    #[test]
    fn groups_without_rows_are_left_out() {
        let html = report().render();
        assert!(!html.contains("Working external links"), "an empty group");
        assert!(html.contains("Refused external links <span class=\"n\">1</span>"));
        assert!(html.contains("<tr><td class=\"status\">Refused 403</td>"));
        assert!(html.contains("2 external files are not checked."));
        let empty = LinkReport {
            site: "https://site.example/".to_string(),
            ..LinkReport::default()
        };
        assert!(empty.render().contains("No broken or refused links."));

        let tmp = tempfile::tempdir().expect("temp dir");
        report().write(tmp.path()).expect("written");
        assert!(tmp.path().join(REPORT_FILE).is_file());
        let gone = tmp.path().join("deleted");
        assert_eq!(
            report().write(&gone).expect_err("no folder").kind(),
            io::ErrorKind::NotFound
        );
    }
}
