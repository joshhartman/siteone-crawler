//! What a run looked at and what it did not (GUI-SPEC §4.10, *What was checked*): the crawl's scope
//! and limits as the result JSON states them, the answers by kind, and every kind of URL the crawl
//! or the app's external link check left out — with a count where the data has one.
//!
//! The crawler lists the URLs it skipped for another host or for robots.txt (the `skipped` and
//! `skipped-summary` tables). It does not count the URLs it left out for `--ignore-regex`,
//! `--max-depth` or `--max-visited-urls`: for those the app says that they were left out, never how
//! many.

use super::config::ACTION_LINK_PATTERNS;
use super::link_check::{LinkResult, Outcome, REASON_OTHER_HOST, cell, column};
use super::results::ResultData;
use super::run_state::{RunOutcome, StatusCounts};

/// The reason of a URL skipped for robots.txt (`SkippedUrlsAnalyzer`).
const REASON_ROBOTS: &str = "Robots.txt";

/// The content types a run can leave out: `(option, what in words)`.
const DISABLE_OPTIONS: &[(&str, &str)] = &[
    ("disableJavascript", "scripts"),
    ("disableStyles", "styles"),
    ("disableFonts", "fonts"),
    ("disableImages", "images"),
    ("disableFiles", "documents and other files"),
];

/// The scope and limits of a crawl, from its result JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrawlScope {
    /// URLs the crawler skipped, per reason, other than those on another host (the app's link
    /// list counts those): `(reason, count)`, in the order of the file.
    pub skipped: Vec<(String, u64)>,
    pub ignore_regex: Vec<String>,
    pub include_regex: Vec<String>,
    /// `--allowed-domain-for-crawling`.
    pub allowed_domains: Vec<String>,
    pub max_visited_urls: Option<u64>,
    /// 0 = no limit.
    pub max_depth: u64,
    pub single_page: bool,
    /// Content types the crawl did not download, in words.
    pub disabled: Vec<&'static str>,
}

impl CrawlScope {
    pub fn from_result(data: &ResultData) -> Self {
        let strings = |key: &str| -> Vec<String> {
            data.option(key)
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .filter(|item| !item.trim().is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let flag = |key: &str| data.option(key).and_then(serde_json::Value::as_bool) == Some(true);
        Self {
            skipped: skipped_by_reason(data),
            ignore_regex: strings("ignoreRegex"),
            include_regex: strings("includeRegex"),
            allowed_domains: strings("allowedDomainsForCrawling"),
            max_visited_urls: data
                .option("maxVisitedUrls")
                .and_then(serde_json::Value::as_u64)
                .filter(|limit| *limit > 0),
            max_depth: data.option("maxDepth").and_then(serde_json::Value::as_u64).unwrap_or(0),
            single_page: flag("singlePage"),
            disabled: DISABLE_OPTIONS
                .iter()
                .filter(|(key, _)| flag(key))
                .map(|(_, what)| *what)
                .collect(),
        }
    }

    /// The skip patterns that are not the app's action-link patterns.
    pub fn own_skip_patterns(&self) -> Vec<&str> {
        self.ignore_regex
            .iter()
            .map(String::as_str)
            .filter(|pattern| !ACTION_LINK_PATTERNS.contains(pattern))
            .collect()
    }

    /// Whether the run skipped the app's action-link patterns (any of them).
    pub fn skips_action_links(&self) -> bool {
        self.ignore_regex
            .iter()
            .any(|pattern| ACTION_LINK_PATTERNS.contains(&pattern.as_str()))
    }
}

/// `(reason, count)` of the skipped URLs other than other hosts: summed from `skipped-summary`
/// (count per reason and domain), or counted in `skipped` when the summary is missing.
fn skipped_by_reason(data: &ResultData) -> Vec<(String, u64)> {
    let mut counts: Vec<(String, u64)> = Vec::new();
    let mut add = |reason: &str, count: u64| {
        let reason = reason.trim();
        if reason.is_empty() || reason == REASON_OTHER_HOST || count == 0 {
            return;
        }
        match counts.iter_mut().find(|(known, _)| known == reason) {
            Some((_, total)) => *total += count,
            None => counts.push((reason.to_string(), count)),
        }
    };
    if let Some(table) = data.table("skipped-summary") {
        let (reason, count) = (column(table, "reason"), column(table, "count"));
        for row in &table.rows {
            add(cell(row, reason), cell(row, count).trim().parse().unwrap_or(0));
        }
    } else if let Some(table) = data.table("skipped") {
        let reason = column(table, "reason");
        for row in &table.rows {
            add(cell(row, reason), 1);
        }
    }
    counts
}

/// Something a run did not check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gap {
    /// The crawl did not finish (the outcome's label).
    Unfinished(&'static str),
    /// The crawl reached `--max-visited-urls`.
    MaxUrls(u64),
    /// The app's action-link patterns were in `--ignore-regex`.
    ActionLinks,
    /// The user's own `--ignore-regex` patterns.
    SkipPatterns(Vec<String>),
    Robots(u64),
    /// Another reason of the crawler's `skipped` table.
    Skipped(String, u64),
    SinglePage,
    MaxDepth(u64),
    /// Content types not downloaded.
    Disabled(Vec<&'static str>),
    /// External links found that no check covers.
    ExternalUnchecked(u64),
    /// External links over the app's cap.
    ExternalOverCap(u64),
    /// Files on other websites.
    ExternalFiles(u64),
}

impl Gap {
    /// The count, where the data has one.
    pub fn count(&self) -> Option<u64> {
        match self {
            Gap::Robots(count)
            | Gap::Skipped(_, count)
            | Gap::ExternalUnchecked(count)
            | Gap::ExternalOverCap(count)
            | Gap::ExternalFiles(count) => Some(*count),
            _ => None,
        }
    }

    /// One sentence for the report and the tooltip.
    pub fn text(&self) -> String {
        match self {
            Gap::Unfinished(outcome) => {
                format!("The crawl did not finish ({outcome}): pages it had not reached were not checked.")
            }
            Gap::MaxUrls(limit) => format!(
                "The crawl stopped at its limit of {} (--max-visited-urls): URLs found after that were not requested.",
                plural(*limit, "URL")
            ),
            Gap::ActionLinks => "Cart and action links (add to cart, remove from the cart, log out) were not requested; the crawler does not count them.".to_string(),
            Gap::SkipPatterns(patterns) => format!(
                "URLs matching {} (--ignore-regex) were not requested; the crawler does not count them.",
                patterns.join(", ")
            ),
            Gap::Robots(count) => format!(
                "{} disallowed by robots.txt {} not requested.",
                plural(*count, "URL"),
                were(*count)
            ),
            Gap::Skipped(reason, count) => format!(
                "{} {} skipped by the crawler ({reason}).",
                plural(*count, "URL"),
                were(*count)
            ),
            Gap::SinglePage => {
                "Only the start page was crawled (--single-page): links to other pages were not followed.".to_string()
            }
            Gap::MaxDepth(depth) => format!(
                "Pages more than {} away from the start page were not crawled (--max-depth); the crawler does not count them.",
                plural(*depth, "link")
            ),
            Gap::Disabled(what) => format!(
                "{} were not downloaded: broken ones cannot show up.",
                capitalized(&what.join(", "))
            ),
            Gap::ExternalUnchecked(count) => format!(
                "{} {} found but not checked.",
                plural(*count, "external link"),
                were(*count)
            ),
            Gap::ExternalOverCap(count) => format!(
                "{} over the app's limit of {} {} not checked.",
                plural(*count, "external link"),
                thousands(super::link_check::MAX_LINKS as u64),
                were(*count)
            ),
            Gap::ExternalFiles(count) => format!(
                "{} on other websites (images, scripts, styles, fonts) {} not checked: the app checks links only.",
                plural(*count, "file"),
                were(*count)
            ),
        }
    }

    /// A few words for the Link checker tab's line.
    pub fn short(&self) -> String {
        match self {
            Gap::Unfinished(_) => "pages the crawl had not reached".to_string(),
            Gap::MaxUrls(limit) => format!("URLs over the limit of {}", thousands(*limit)),
            Gap::ActionLinks => "cart and action links".to_string(),
            Gap::SkipPatterns(_) => "URLs matching your skip patterns".to_string(),
            Gap::Robots(count) => format!("{} disallowed by robots.txt", plural(*count, "URL")),
            Gap::Skipped(reason, count) => format!("{} skipped ({reason})", plural(*count, "URL")),
            Gap::SinglePage => "links to other pages".to_string(),
            Gap::MaxDepth(depth) => format!("pages more than {} deep", plural(*depth, "link")),
            Gap::Disabled(what) => what.join(", "),
            Gap::ExternalUnchecked(count) => plural(*count, "external link"),
            Gap::ExternalOverCap(count) => format!("{} over the limit", plural(*count, "external link")),
            Gap::ExternalFiles(count) => plural(*count, "external file"),
        }
    }
}

/// What [`gaps`] looks at.
#[derive(Debug, Clone, Copy)]
pub struct GapInput<'a> {
    /// `None` until the result JSON is loaded (or when the run has none).
    pub scope: Option<&'a CrawlScope>,
    pub outcome: &'a RunOutcome,
    /// URLs crawled.
    pub crawled: u64,
    pub external_unchecked: usize,
    pub external_over_cap: usize,
    pub external_files: usize,
}

/// Everything the run did not check, the crawl's limits first, then what the crawler skipped, then
/// what the app's check left out.
pub fn gaps(input: &GapInput<'_>) -> Vec<Gap> {
    let mut gaps = Vec::new();
    if matches!(
        input.outcome,
        RunOutcome::Stopped
            | RunOutcome::Aborted
            | RunOutcome::Crashed { .. }
            | RunOutcome::Failed { .. }
            | RunOutcome::NoPagesCrawled
    ) {
        gaps.push(Gap::Unfinished(input.outcome.label()));
    }
    if let Some(scope) = input.scope {
        if let Some(limit) = scope.max_visited_urls
            && input.crawled >= limit
        {
            gaps.push(Gap::MaxUrls(limit));
        }
        if scope.single_page {
            gaps.push(Gap::SinglePage);
        }
        if scope.max_depth > 0 && !scope.single_page {
            gaps.push(Gap::MaxDepth(scope.max_depth));
        }
        if scope.skips_action_links() {
            gaps.push(Gap::ActionLinks);
        }
        let own = scope.own_skip_patterns();
        if !own.is_empty() {
            gaps.push(Gap::SkipPatterns(
                own.iter().map(|pattern| pattern.to_string()).collect(),
            ));
        }
        for (reason, count) in &scope.skipped {
            gaps.push(if reason == REASON_ROBOTS {
                Gap::Robots(*count)
            } else {
                Gap::Skipped(reason.clone(), *count)
            });
        }
        if !scope.disabled.is_empty() {
            gaps.push(Gap::Disabled(scope.disabled.clone()));
        }
    }
    if input.external_unchecked > 0 {
        gaps.push(Gap::ExternalUnchecked(input.external_unchecked as u64));
    }
    if input.external_over_cap > 0 {
        gaps.push(Gap::ExternalOverCap(input.external_over_cap as u64));
    }
    if input.external_files > 0 {
        gaps.push(Gap::ExternalFiles(input.external_files as u64));
    }
    gaps
}

/// The crawled URLs by answer: "268 OK (2xx), 1 redirect (3xx), 6 client errors (4xx)" — counts of
/// zero left out, OK always there.
pub fn crawled_summary(counts: &StatusCounts) -> String {
    let mut parts = vec![format!("{} OK (2xx)", thousands(counts.success))];
    let mut add = |count: u64, noun: &str, code: &str| {
        if count > 0 {
            parts.push(format!("{} ({code})", plural(count, noun)));
        }
    };
    add(counts.redirect, "redirect", "3xx");
    add(counts.client_error, "client error", "4xx");
    add(counts.server_error, "server error", "5xx");
    if counts.error > 0 {
        parts.push(format!(
            "{} without an answer (connection failed, timeout)",
            thousands(counts.error)
        ));
    }
    parts.join(", ")
}

/// The checked external links by answer: "69 OK (3 after a redirect), 7 refused (401, 403, 429),
/// 9 broken (4xx, 5xx), 3 without an answer (DNS, connection, TLS, timeout)".
pub fn external_summary(results: &[LinkResult]) -> String {
    let count = |wanted: fn(&LinkResult) -> bool| results.iter().filter(|link| wanted(link)).count() as u64;
    let ok = count(|link| link.outcome == Outcome::Ok);
    let redirected = count(|link| link.outcome == Outcome::Ok && link.final_url.is_some());
    let refused = count(|link| link.outcome == Outcome::Refused);
    let broken = count(|link| link.outcome == Outcome::Broken && link.status.is_some());
    let silent = count(|link| link.outcome == Outcome::Broken && link.status.is_none());
    let mut parts = vec![if redirected > 0 {
        format!("{} OK ({} after a redirect)", thousands(ok), thousands(redirected))
    } else {
        format!("{} OK", thousands(ok))
    }];
    if refused > 0 {
        parts.push(format!("{} refused (401, 403, 429)", thousands(refused)));
    }
    if broken > 0 {
        parts.push(format!("{} broken (4xx, 5xx)", thousands(broken)));
    }
    if silent > 0 {
        parts.push(format!(
            "{} without an answer (DNS, connection, TLS, timeout)",
            thousands(silent)
        ));
    }
    parts.join(", ")
}

/// A count with thousands separators (as `ui::widgets::format::count`; the model does not depend
/// on the UI).
fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn plural(value: u64, noun: &str) -> String {
    format!("{} {noun}{}", thousands(value), if value == 1 { "" } else { "s" })
}

fn were(count: u64) -> &'static str {
    if count == 1 { "was" } else { "were" }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::results::parse_result;

    /// The tables and options of a real crawl of a WooCommerce site (names replaced), cut
    /// down: 152 URLs on other hosts, robots.txt and a depth reason added.
    const RESULT: &str = r#"{
        "options": {"maxVisitedUrls": 10000, "maxDepth": 0, "singlePage": false,
            "disableImages": true, "disableFonts": false, "ignoreRegex": ["/[?&]add-to-cart=/",
            "/[?&]remove_item=/", "/[?&]undo_item=/", "/wp-login\\.php\\?(.*&)?action=logout/", "/\\.pdf$/"],
            "includeRegex": [], "allowedDomainsForCrawling": [], "url": "https://www.shop.example"},
        "tables": {
            "skipped-summary": {"columns": {"count": {"name": "Unique URLs"}, "domain": {"name": "Domain"},
                "reason": {"name": "Reason"}}, "rows": [
                {"count": "141", "domain": "cdn.shop-example.net", "reason": "Not allowed host"},
                {"count": "11", "domain": "demos.wpbeaverbuilder.com", "reason": "Not allowed host"},
                {"count": "3", "domain": "shop.example", "reason": "Robots.txt"},
                {"count": "2", "domain": "www.shop.example", "reason": "Robots.txt"},
                {"count": "1", "domain": "shop.example", "reason": "Max depth"}]}
        }
    }"#;

    #[test]
    fn the_scope_comes_from_the_options_and_the_skipped_summary() {
        let data = parse_result(RESULT).expect("parses");
        let scope = CrawlScope::from_result(&data);
        assert_eq!(
            scope.skipped,
            [("Robots.txt".to_string(), 5), ("Max depth".to_string(), 1)]
        );
        assert_eq!(scope.max_visited_urls, Some(10_000));
        assert_eq!(scope.disabled, ["images"]);
        assert!(scope.skips_action_links());
        assert_eq!(scope.own_skip_patterns(), [r"/\.pdf$/"]);
        // Without the summary, the rows of `skipped` are counted.
        let rows = parse_result(
            r#"{"tables":{"skipped":{"columns":{"reason":{},"url":{}},"rows":[
            {"reason":"Robots.txt","url":"/a"},{"reason":"Robots.txt","url":"/b"},
            {"reason":"Not allowed host","url":"https://x.example/"}]}}}"#,
        )
        .expect("parses");
        assert_eq!(CrawlScope::from_result(&rows).skipped, [("Robots.txt".to_string(), 2)]);
        // Options of an unexpected type are left out, never an error.
        let odd =
            parse_result(r#"{"options":{"maxVisitedUrls":"many","ignoreRegex":"x","singlePage":1}}"#).expect("parses");
        assert_eq!(CrawlScope::from_result(&odd), CrawlScope::default());
    }

    #[test]
    fn every_gap_is_named_with_its_count_where_the_data_has_one() {
        let data = parse_result(RESULT).expect("parses");
        let scope = CrawlScope::from_result(&data);
        let gaps = gaps(&GapInput {
            scope: Some(&scope),
            outcome: &RunOutcome::Finished,
            crawled: 172,
            external_unchecked: 0,
            external_over_cap: 3,
            external_files: 140,
        });
        assert_eq!(
            gaps,
            [
                Gap::ActionLinks,
                Gap::SkipPatterns(vec![r"/\.pdf$/".to_string()]),
                Gap::Robots(5),
                Gap::Skipped("Max depth".to_string(), 1),
                Gap::Disabled(vec!["images"]),
                Gap::ExternalOverCap(3),
                Gap::ExternalFiles(140),
            ]
        );
        let texts: Vec<String> = gaps.iter().map(Gap::text).collect();
        assert_eq!(
            texts[0],
            "Cart and action links (add to cart, remove from the cart, log out) were not requested; the crawler does not count them."
        );
        assert_eq!(texts[2], "5 URLs disallowed by robots.txt were not requested.");
        assert_eq!(texts[3], "1 URL was skipped by the crawler (Max depth).");
        assert_eq!(texts[4], "Images were not downloaded: broken ones cannot show up.");
        assert_eq!(
            texts[5],
            "3 external links over the app's limit of 2,000 were not checked."
        );
        assert_eq!(
            texts[6],
            "140 files on other websites (images, scripts, styles, fonts) were not checked: the app checks links only."
        );
        assert_eq!(gaps[6].short(), "140 external files");
        assert_eq!(gaps[2].count(), Some(5));
        assert_eq!(gaps[0].count(), None);

        // A stopped crawl at its URL limit, one page only, nothing known from the result.
        let limited = CrawlScope {
            max_visited_urls: Some(50),
            single_page: true,
            max_depth: 3,
            ..CrawlScope::default()
        };
        let gaps = super::gaps(&GapInput {
            scope: Some(&limited),
            outcome: &RunOutcome::Stopped,
            crawled: 50,
            external_unchecked: 12,
            external_over_cap: 0,
            external_files: 0,
        });
        assert_eq!(
            gaps,
            [
                Gap::Unfinished("Stopped"),
                Gap::MaxUrls(50),
                Gap::SinglePage,
                Gap::ExternalUnchecked(12)
            ]
        );
        assert_eq!(
            gaps[1].text(),
            "The crawl stopped at its limit of 50 URLs (--max-visited-urls): URLs found after that were not requested."
        );
        let none = super::gaps(&GapInput {
            scope: None,
            outcome: &RunOutcome::Finished,
            crawled: 10,
            external_unchecked: 0,
            external_over_cap: 0,
            external_files: 0,
        });
        assert!(none.is_empty());
    }

    #[test]
    fn answers_are_summed_up_by_kind() {
        let counts = StatusCounts {
            success: 1_268,
            redirect: 1,
            client_error: 6,
            server_error: 0,
            error: 2,
        };
        assert_eq!(
            crawled_summary(&counts),
            "1,268 OK (2xx), 1 redirect (3xx), 6 client errors (4xx), 2 without an answer (connection failed, timeout)"
        );
        assert_eq!(crawled_summary(&StatusCounts::default()), "0 OK (2xx)");
        let link = |outcome, status: Option<u16>, redirected: bool| LinkResult {
            outcome,
            status,
            final_url: redirected.then(|| "https://x.example/en/".to_string()),
            ..LinkResult::default()
        };
        let results = [
            link(Outcome::Ok, Some(200), false),
            link(Outcome::Ok, Some(200), true),
            link(Outcome::Refused, Some(403), false),
            link(Outcome::Broken, Some(404), false),
            link(Outcome::Broken, None, false),
        ];
        assert_eq!(
            external_summary(&results),
            "2 OK (1 after a redirect), 1 refused (401, 403, 429), 1 broken (4xx, 5xx), 1 without an answer (DNS, connection, TLS, timeout)"
        );
    }
}
