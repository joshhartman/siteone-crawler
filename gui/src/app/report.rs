//! The link report of a run (GUI-SPEC §4.10) as the app writes it: gathered from the run's state —
//! its broken crawled URLs, the external link check and link texts, with the newest answers of
//! retries, what was checked and what not, and the broken links by page — and written to
//! `<run>/link-report.html` when the crawl's result is loaded, when a link check ends, when link
//! texts arrive, after every retry pass, and when *Link report* opens it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{App, PastLoad};
use crate::backend::shell;
use crate::model::coverage::{self, CrawlScope, Gap, GapInput};
use crate::model::link_check::{Outcome, OutcomeCounts, RunLinks};
use crate::model::link_report::{LinkReport, ReportGroup, ReportRow, group_by_page};
use crate::model::link_text::RefPosition;
use crate::model::run_state::RunState;
use crate::ui::run::broken::{self, Where};
use crate::ui::widgets::format;

/// What the report is gathered from.
struct Source<'a> {
    state: &'a RunState,
    links: &'a RunLinks,
    site: String,
}

fn gather(source: &Source<'_>) -> LinkReport {
    let site_host = broken::site_host(&source.site);
    let texts = &source.links.texts;
    // Where each page a link was found on references it.
    let positions = |url: &str, pages: &[&str]| -> Vec<Option<RefPosition>> {
        pages.iter().map(|page| texts.position(url, page).copied()).collect()
    };
    let found_on: HashMap<&str, &str> = source
        .links
        .internal
        .iter()
        .map(|(url, page)| (url.trim_end_matches('/'), page.as_str()))
        .collect();
    let crawled_row = |url: &str, status: String, kind: &str| {
        let sources: Vec<String> = found_on
            .get(url.trim().trim_end_matches('/'))
            .map(|page| vec![page.to_string()])
            .unwrap_or_default();
        let pages: Vec<&str> = sources.iter().map(String::as_str).collect();
        ReportRow {
            status,
            url: url.to_string(),
            title_or_kind: kind.to_string(),
            link_text: texts.lookup(url, &pages).text().to_string(),
            positions: positions(url, &pages),
            found_on: sources,
        }
    };
    let link_row = |link: &crate::model::link_check::LinkResult| {
        let pages: Vec<&str> = link.sources.iter().map(String::as_str).collect();
        ReportRow {
            status: link.status_label(),
            url: link.url.clone(),
            title_or_kind: link.title.clone().unwrap_or_default(),
            link_text: texts.lookup(&link.url, &pages).text().to_string(),
            positions: positions(&link.url, &pages),
            found_on: link.sources.clone(),
        }
    };

    let mut pages = Vec::new();
    let mut assets = Vec::new();
    let mut external = Vec::new();
    for row in &source.state.urls {
        if !broken::is_broken(row.status) {
            continue;
        }
        let (place, kind) = broken::classify(&row.url, site_host.as_deref());
        let shown = crawled_row(&row.url, broken::status_text(row.status), kind.label());
        match place {
            Where::InternalPage => pages.push(shown),
            Where::InternalAsset => assets.push(shown),
            Where::External => external.push(shown),
        }
    }
    let results = source
        .links
        .report
        .as_ref()
        .map_or(&[][..], |report| &report.results[..]);
    let mut refused = Vec::new();
    let mut working = Vec::new();
    for link in results {
        match link.outcome {
            Outcome::Broken => external.push(link_row(link)),
            Outcome::Refused => refused.push(link_row(link)),
            Outcome::Ok => working.push(link_row(link)),
        }
    }
    let checked = OutcomeCounts::of(results);
    let counts = vec![
        ("broken internal pages".to_string(), pages.len() as u64),
        ("broken internal assets".to_string(), assets.len() as u64),
        ("broken external links".to_string(), external.len() as u64),
        ("refused external links".to_string(), refused.len() as u64),
        ("external links checked".to_string(), checked.total() as u64),
    ];

    // The broken and refused links by page, as the Link checker's *All broken* view groups them;
    // left out when it would only repeat the groups above: one group, or no page known.
    let flagged = pages.iter().chain(&assets).chain(&external).chain(&refused);
    let mut by_page = group_by_page(flagged.map(|row| (row.clone(), row.found_on.clone())));
    if by_page.len() < 2 || by_page.iter().all(|group| group.page.is_none()) {
        by_page.clear();
    }

    let state = source.state;
    let list = source.links.list.as_ref();
    let mut what = vec![(
        "Scope".to_string(),
        scope_text(&source.site, source.links.scope.as_ref()),
    )];
    what.push((
        "Crawled URLs".to_string(),
        format!(
            "{}: {}",
            format::count(state.urls.len() as u64),
            coverage::crawled_summary(&state.status_counts)
        ),
    ));
    if !results.is_empty() {
        what.push((
            "External links".to_string(),
            format!(
                "{} checked: {}",
                format::count(results.len() as u64),
                coverage::external_summary(results)
            ),
        ));
    }
    let not_checked = coverage::gaps(&GapInput {
        scope: source.links.scope.as_ref(),
        outcome: &state.outcome(),
        crawled: state.urls.len() as u64,
        external_unchecked: source.links.unchecked(),
        external_over_cap: list.map_or(0, |list| list.truncated),
        external_files: list
            .map(|list| list.files)
            .or(source.links.report.as_ref().map(|report| report.files_not_checked))
            .unwrap_or(0),
    })
    .iter()
    .map(Gap::text)
    .collect();

    let group = |title: &str, rows: Vec<ReportRow>| ReportGroup {
        title: title.to_string(),
        rows,
        ..ReportGroup::default()
    };
    LinkReport {
        crawled_at: source.state.started.as_ref().and_then(|started| {
            chrono::DateTime::parse_from_rfc3339(&started.executed_at)
                .ok()
                .map(|time| time.with_timezone(&chrono::Local).format("%d %b %Y, %H:%M").to_string())
        }),
        site: source.site.clone(),
        written_at: chrono::Local::now().format("%d %b %Y, %H:%M").to_string(),
        counts,
        checked: what,
        not_checked,
        groups: vec![
            group("Broken internal pages", pages),
            group("Broken internal assets", assets),
            group("Broken external links", external),
            group("Refused external links", refused),
            ReportGroup {
                folded: true,
                ..group("Working external links", working)
            },
        ],
        by_page,
    }
}

/// What the crawl covered: the site's host (with and without `www.`, as the crawler treats it),
/// more domains and the include patterns where the result JSON names them.
fn scope_text(site: &str, scope: Option<&CrawlScope>) -> String {
    let host = url::Url::parse(site.trim())
        .ok()
        .and_then(|url| url.host_str().map(|host| host.trim_start_matches("www.").to_string()))
        .unwrap_or_else(|| site.to_string());
    let mut text = format!("Pages and files on {host}, with or without www.");
    if let Some(scope) = scope {
        if !scope.allowed_domains.is_empty() {
            text.push_str(&format!(
                " Also crawled: {} (--allowed-domain-for-crawling).",
                scope.allowed_domains.join(", ")
            ));
        }
        if !scope.include_regex.is_empty() {
            text.push_str(&format!(
                " Only URLs matching {} (--include-regex).",
                scope.include_regex.join(", ")
            ));
        }
    }
    text
}

impl App {
    /// The link report of the run in `run_dir` (the current run or the one shown), from its state
    /// now.
    fn link_report(&self, run_dir: &Path) -> Option<LinkReport> {
        if let Some(run) = &self.current_run
            && run.plan.run_dir == run_dir
        {
            return Some(gather(&Source {
                state: &run.state,
                links: &run.links,
                site: super::lifecycle::site_url(&run.state, &run.config.url),
            }));
        }
        let past = self.past_run.as_ref().filter(|past| past.run_dir == run_dir)?;
        let PastLoad::Ready(data) = &past.load else {
            return None;
        };
        Some(gather(&Source {
            state: &data.state,
            links: &data.links,
            site: super::lifecycle::site_url(&data.state, &data.record.url),
        }))
    }

    /// Writes the link report of the run in `run_dir` from its current state (on a worker).
    pub fn write_link_report(&mut self, run_dir: &Path) {
        let Some(report) = self.link_report(run_dir) else {
            return;
        };
        let dir = run_dir.to_path_buf();
        self.write_latest(LinkReport::path(run_dir), "The link report", move || report.write(&dir));
    }

    /// *Link report* (and the Files tab's Open / Show in folder): writes the report of `run_dir`
    /// from the state now, then opens it (or, with `reveal`, shows it in Explorer).
    pub fn open_link_report(&mut self, run_dir: PathBuf, reveal: bool) {
        let Some(report) = self.link_report(&run_dir) else {
            return;
        };
        let path = LinkReport::path(&run_dir);
        self.spawn_task(move || {
            let written = report.write(&run_dir);
            Box::new(move |app: &mut App| match written {
                Ok(()) => {
                    let opened = if reveal {
                        shell::reveal_in_explorer(&path)
                    } else {
                        shell::open_path(&path)
                    };
                    if let Err(error) = opened {
                        app.toasts
                            .error(format!("{} could not be opened: {error}", path.display()));
                    }
                }
                Err(error) => app
                    .toasts
                    .error(format!("The link report could not be written: {error}")),
            })
        });
    }
}
