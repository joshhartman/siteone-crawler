//! The URLs tab: a virtualized table of every crawled URL (#, status, type, time, size, cached,
//! URL, title) with a text filter, status-class toggles, a content-type filter, sortable columns,
//! *Follow* and a row context menu. The title of an internal HTML page comes from the result
//! JSON's `seo` table once the run has finished ([`PageTitles`], built once), or, for a URL retried
//! after the crawl, from the retry's answer. A retried row shows its newest answer, a queued one a
//! mark; a failed row's menu retries it or queues it (GUI-SPEC §4.9).
//!
//! *Depth* is the link distance from a start URL: the `url` event's while the crawl runs, the
//! result JSON's exact one with `--link-details`; *In links* (with `--link-details`) the number of
//! pages that reference the URL ([`LinkColumns`], built once). Each column is shown only when the
//! run has its data. The row menu lists the pages linking to a URL (*Pages linking here*): every
//! kept referrer of the crawler's `links` table, or the page of its `url` event.
//!
//! It stays fluid with 100 000 rows: [`UrlIndex`] keeps the filtered and sorted row numbers. Rows
//! that arrive are filtered and placed into it one by one; it is rebuilt only when the filter or the
//! sort changes. A frame only formats the rows it shows.

use eframe::egui::{self, Align, Layout, Rect, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;

use super::overview::failed_color;
use super::{RunAction, RunView, padded};
use crate::model::link_data::{CrawlerRef, LinkGraph};
use crate::model::results::ResultData;
use crate::model::run_state::{ContentKind, StatusClass, UrlRow};
use crate::theme;
use crate::ui::widgets::table::{self, HEADER_HEIGHT, ROW_HEIGHT, contains_ignore_case, sort_header};
use crate::ui::widgets::{self, Tone, format};

/// More new rows than this in one frame are merged by sorting again instead of one by one.
const INCREMENTAL_LIMIT: usize = 64;

/// From this width on, the filters fit in one row.
const ONE_ROW_FROM: f32 = 1100.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Arrival order.
    #[default]
    Index,
    Status,
    Kind,
    Time,
    Size,
    Cached,
    Depth,
    InLinks,
    Url,
    Title,
}

/// The status classes of the filter toggles, in display order.
pub const CLASSES: [StatusClass; 5] = [
    StatusClass::Success,
    StatusClass::Redirect,
    StatusClass::ClientError,
    StatusClass::ServerError,
    StatusClass::Error,
];

fn class_slot(class: StatusClass) -> usize {
    match class {
        StatusClass::Success => 0,
        StatusClass::Redirect => 1,
        StatusClass::ClientError => 2,
        StatusClass::ServerError => 3,
        StatusClass::Error => 4,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlFilter {
    pub text: String,
    /// Shown status classes, by [`CLASSES`] position.
    pub classes: [bool; 5],
    pub kind: Option<ContentKind>,
}

impl Default for UrlFilter {
    fn default() -> Self {
        Self {
            text: String::new(),
            classes: [true; 5],
            kind: None,
        }
    }
}

impl UrlFilter {
    pub fn is_active(&self) -> bool {
        !self.text.trim().is_empty() || self.classes.contains(&false) || self.kind.is_some()
    }

    fn matches(&self, row: &UrlRow, title: &str, needle_lower: &str) -> bool {
        self.classes
            .get(class_slot(StatusClass::of(i64::from(row.status))))
            .copied()
            .unwrap_or(true)
            && self.kind.is_none_or(|kind| row.kind == kind)
            && (contains_ignore_case(&row.url, needle_lower) || contains_ignore_case(title, needle_lower))
    }
}

/// The `<title>` of each URL row that is an internal HTML page, from the result JSON's `seo`
/// table (keyed by the URL's path and query, `urlPathAndQuery`). Built once when the result is
/// loaded: a frame only looks titles up.
#[derive(Debug, Default)]
pub struct PageTitles {
    titles: Vec<String>,
    /// Per URL row: an index into `titles`, or [`NO_TITLE`].
    by_row: Vec<u32>,
}

const NO_TITLE: u32 = u32::MAX;

/// The `urlPathAndQuery` of the `seo` table for a URL (`SeoAndOpenGraphAnalyzer`).
fn path_and_query(url: &url::Url) -> String {
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    }
}

impl PageTitles {
    /// The titles of `rows` (of a site whose start host is `site`, `www.` aside).
    pub fn build(data: &ResultData, rows: &[UrlRow], site: Option<&str>) -> Self {
        let mut titles = Vec::new();
        let mut by_key = std::collections::HashMap::new();
        if let Some(table) = data.table("seo") {
            let column = |key: &str| table.columns.iter().position(|column| column.key == key);
            if let (Some(path), Some(title)) = (column("urlPathAndQuery"), column("title")) {
                for row in &table.rows {
                    if let (Some(path), Some(title)) = (row.get(path), row.get(title))
                        && !title.trim().is_empty()
                        && let Ok(index) = u32::try_from(titles.len())
                    {
                        by_key.entry(path.clone()).or_insert(index);
                        titles.push(title.split_whitespace().collect::<Vec<_>>().join(" "));
                    }
                }
            }
        }
        let by_row = rows
            .iter()
            .map(|row| {
                if row.kind != ContentKind::Html || by_key.is_empty() {
                    return NO_TITLE;
                }
                let internal = site.is_none_or(|site| super::broken::site_host(&row.url).as_deref() == Some(site));
                match url::Url::parse(&row.url) {
                    Ok(url) if internal => by_key.get(&path_and_query(&url)).copied().unwrap_or(NO_TITLE),
                    _ => NO_TITLE,
                }
            })
            .collect();
        Self { titles, by_row }
    }

    /// Sets the title of URL row `row` (a retry's answer).
    pub fn set(&mut self, row: usize, title: &str) {
        if self.by_row.len() <= row {
            self.by_row.resize(row + 1, NO_TITLE);
        }
        if let (Some(slot), Ok(index)) = (self.by_row.get_mut(row), u32::try_from(self.titles.len())) {
            *slot = index;
            self.titles.push(title.to_string());
        }
    }

    /// The title of URL row `row` ("" without one).
    pub fn get(&self, row: usize) -> &str {
        self.by_row
            .get(row)
            .and_then(|index| self.titles.get(*index as usize))
            .map_or("", String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.titles.is_empty()
    }
}

/// *Depth* and *In links* of each URL row from the crawler's link facts (`--link-details`, the
/// result JSON's `results` rows), built once when the result is loaded; without them *Depth* is the
/// `url` event's.
#[derive(Debug, Default)]
pub struct LinkColumns {
    /// Per URL row: the exact depth, or [`NONE`].
    depth: Vec<u32>,
    /// Per URL row: pages referencing it, or [`NONE`].
    in_links: Vec<u32>,
    /// The result has link facts (the In links column is shown).
    pub has_in_links: bool,
}

const NONE: u32 = u32::MAX;

impl LinkColumns {
    pub fn build(graph: Option<&LinkGraph>, rows: &[UrlRow]) -> Self {
        let Some(graph) = graph else {
            return Self::default();
        };
        let mut columns = Self {
            depth: Vec::with_capacity(rows.len()),
            in_links: Vec::with_capacity(rows.len()),
            has_in_links: false,
        };
        for row in rows {
            let facts = graph.facts(&row.url);
            let in_links = facts
                .and_then(|facts| facts.in_links)
                .or_else(|| graph.target(&row.url).map(|target| target.in_links));
            columns.has_in_links |= in_links.is_some();
            columns.depth.push(facts.and_then(|facts| facts.depth).unwrap_or(NONE));
            columns.in_links.push(in_links.unwrap_or(NONE));
        }
        columns
    }

    /// The depth of URL row `index`: the exact one, else the event's.
    pub fn depth(&self, rows: &[UrlRow], index: usize) -> Option<u32> {
        self.depth
            .get(index)
            .copied()
            .filter(|depth| *depth != NONE)
            .or_else(|| rows.get(index).and_then(|row| row.depth))
    }

    pub fn in_links(&self, index: usize) -> Option<u32> {
        self.in_links.get(index).copied().filter(|count| *count != NONE)
    }

    /// Whether it was built from link facts.
    fn is_built(&self) -> bool {
        !self.depth.is_empty()
    }
}

/// What rows are sorted by besides their own fields.
#[derive(Clone, Copy)]
pub struct RowExtras<'a> {
    pub titles: &'a PageTitles,
    pub links: &'a LinkColumns,
}

/// The filtered and sorted row numbers of the URL rows.
#[derive(Debug, Default)]
pub struct UrlIndex {
    rows: Vec<u32>,
    /// How many URL rows have been looked at.
    scanned: usize,
    /// What the index was built for (and whether titles and link facts were known).
    built_for: Option<(UrlFilter, SortKey, bool, bool, bool)>,
    /// How often the index was rebuilt (tests check that new rows do not rebuild it).
    pub rebuilds: u64,
}

impl UrlIndex {
    /// The visible rows, in display order (indices into the URL rows).
    pub fn rows(&self) -> &[u32] {
        &self.rows
    }

    /// The rows changed in place (a retry): the next sync rebuilds.
    pub fn invalidate(&mut self) {
        self.built_for = None;
    }

    /// Brings the index up to date with `rows`.
    pub fn sync(&mut self, rows: &[UrlRow], extras: RowExtras<'_>, filter: &UrlFilter, sort: SortKey, ascending: bool) {
        let titles = extras.titles;
        let titled = !titles.is_empty();
        let faceted = extras.links.is_built();
        // Compared in place: an unchanged frame allocates nothing.
        let current =
            self.built_for
                .as_ref()
                .is_some_and(|(built, built_sort, built_ascending, built_titled, built_faceted)| {
                    built == filter
                        && *built_sort == sort
                        && *built_ascending == ascending
                        && *built_titled == titled
                        && *built_faceted == faceted
                });
        if current && rows.len() == self.scanned {
            return;
        }
        let needle = filter.text.trim().to_lowercase();
        if !current || rows.len() < self.scanned {
            self.rebuilds += 1;
            self.rows = (0..rows.len())
                .filter(|&index| filter.matches(&rows[index], titles.get(index), &needle))
                .filter_map(|index| u32::try_from(index).ok())
                .collect();
            self.scanned = rows.len();
            self.built_for = Some((filter.clone(), sort, ascending, titled, faceted));
            sort_rows(&mut self.rows, rows, extras, sort, ascending);
            return;
        }
        let new: Vec<u32> = (self.scanned..rows.len())
            .filter(|&index| filter.matches(&rows[index], titles.get(index), &needle))
            .filter_map(|index| u32::try_from(index).ok())
            .collect();
        self.scanned = rows.len();
        if sort == SortKey::Index && ascending {
            self.rows.extend(new);
        } else if new.len() > INCREMENTAL_LIMIT {
            self.rows.extend(new);
            sort_rows(&mut self.rows, rows, extras, sort, ascending);
        } else {
            for index in new {
                let at = self
                    .rows
                    .partition_point(|&other| compare(rows, extras, other, index, sort, ascending).is_lt());
                self.rows.insert(at, index);
            }
        }
    }
}

fn compare(
    rows: &[UrlRow],
    extras: RowExtras<'_>,
    a: u32,
    b: u32,
    sort: SortKey,
    ascending: bool,
) -> std::cmp::Ordering {
    let (Some(row_a), Some(row_b)) = (rows.get(a as usize), rows.get(b as usize)) else {
        return a.cmp(&b);
    };
    let (titles, links) = (extras.titles, extras.links);
    // A row without a value sorts after the rows with one.
    let value = |value: Option<u32>| value.unwrap_or(NONE);
    let ordering = match sort {
        SortKey::Index => std::cmp::Ordering::Equal,
        SortKey::Status => row_a.status.cmp(&row_b.status),
        SortKey::Kind => row_a.kind.label().cmp(row_b.kind.label()),
        SortKey::Time => row_a.time_ms.cmp(&row_b.time_ms),
        SortKey::Size => row_a.size.cmp(&row_b.size),
        SortKey::Cached => row_a.cached.cmp(&row_b.cached),
        SortKey::Depth => value(links.depth(rows, a as usize)).cmp(&value(links.depth(rows, b as usize))),
        SortKey::InLinks => value(links.in_links(a as usize)).cmp(&value(links.in_links(b as usize))),
        SortKey::Url => row_a.url.cmp(&row_b.url),
        SortKey::Title => titles.get(a as usize).cmp(titles.get(b as usize)),
    }
    // Ties keep the arrival order, so the order is total and stable.
    .then(a.cmp(&b));
    if ascending { ordering } else { ordering.reverse() }
}

fn sort_rows(index: &mut [u32], rows: &[UrlRow], extras: RowExtras<'_>, sort: SortKey, ascending: bool) {
    if sort == SortKey::Index && ascending {
        return;
    }
    index.sort_unstable_by(|a, b| compare(rows, extras, *a, *b, sort, ascending));
}

/// UI state of the URLs tab.
#[derive(Debug)]
pub struct UrlsUi {
    pub filter: UrlFilter,
    pub sort: SortKey,
    pub ascending: bool,
    /// Keep the newest row in view while the crawl runs.
    pub follow: bool,
    pub index: UrlIndex,
    /// Page titles, built once the result JSON is loaded.
    pub titles: Option<PageTitles>,
    /// Depth and In links from the crawler's link facts, built once the result JSON is loaded.
    pub links: Option<LinkColumns>,
    /// The revisions of the rows and the retries the titles and the index were built for.
    revisions: (u64, u64),
    /// Where the table was drawn last frame (to tell a scroll in it).
    table_rect: Option<Rect>,
    /// The row count Follow last scrolled to (it scrolls again only when rows arrive).
    followed: Option<usize>,
}

impl Default for UrlsUi {
    fn default() -> Self {
        Self {
            filter: UrlFilter::default(),
            sort: SortKey::Index,
            ascending: true,
            follow: true,
            index: UrlIndex::default(),
            titles: None,
            links: None,
            revisions: (0, 0),
            table_rect: None,
            followed: None,
        }
    }
}

impl UrlsUi {
    /// Sorts by `key` (again: flips the direction). Switches *Follow* off.
    pub fn sort_by(&mut self, key: SortKey) {
        if self.sort == key {
            self.ascending = !self.ascending;
        } else {
            self.sort = key;
            self.ascending = true;
        }
        self.follow = false;
    }
}

/// The width of the *Depth* column.
const DEPTH_WIDTH: f32 = 54.0;
/// The width of the *In links* column.
const IN_LINKS_WIDTH: f32 = 66.0;

/// The pages linking to a URL, as the row menu's dialog lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkingPages {
    pub url: String,
    /// One line per referring page: the page with where it references the URL, how, and the link
    /// text (`https://site.example/a.html:45 · <a href> · “Gone”`).
    pub lines: Vec<String>,
    /// Pages beyond the listed ones (the crawler lists 50 per URL).
    pub more: usize,
    /// Every referring page is known (`--link-details`), not only the one that queued the URL.
    pub complete: bool,
    /// The crawl is still running.
    pub live: bool,
}

impl LinkingPages {
    /// The dialog's text above the list.
    pub fn message(&self) -> String {
        let total = self.lines.len() + self.more;
        let mut message = format!(
            "{} {} {}.",
            format::plural(total as u64, "page"),
            if total == 1 { "references" } else { "reference" },
            self.url
        );
        if self.more > 0 {
            message.push_str(&format!(
                " The crawler lists the first {} of them.",
                format::count(self.lines.len() as u64)
            ));
        }
        if !self.complete && self.live {
            message.push_str(
                " While the crawl runs, only the page that queued the URL is known; with “Record every referring page” on (--link-details), every page is listed when it finishes.",
            );
        } else if !self.complete {
            message.push_str(
                " This run names only the page that queued the URL; with “Record every referring page” on (--link-details), the crawler lists all of them.",
            );
        }
        message
    }

    /// The list as text for the clipboard (one page per line, the count of the others last).
    pub fn copy_text(&self) -> String {
        let mut text = self.lines.join("\n");
        if self.more > 0 {
            text.push_str(&format!("\n{}", super::broken::more_pages(self.more)));
        }
        text.push('\n');
        text
    }
}

/// The pages linking to `url`: every kept referrer of the crawler's `links` table, else the page of
/// its `url` event (`row_link`); `None` when neither names one. `live`: the crawl is running.
pub fn linking_pages(
    url: &str,
    graph: Option<&LinkGraph>,
    row_link: Option<&CrawlerRef>,
    live: bool,
) -> Option<LinkingPages> {
    let line = |reference: &CrawlerRef| {
        let mut line = reference.source.to_string();
        if let Some(position) = reference.position() {
            line.push_str(&position.suffix());
        }
        if !reference.attr.is_empty() {
            line.push_str(&format!(" · {}", reference.attr));
        }
        if let Some(text) = &reference.text {
            line.push_str(&format!(" · “{text}”"));
        }
        line
    };
    if let Some(target) = graph
        .and_then(|graph| graph.target(url))
        .filter(|target| !target.refs.is_empty())
    {
        return Some(LinkingPages {
            url: url.to_string(),
            lines: target.refs.iter().map(line).collect(),
            more: target.more(),
            complete: true,
            live,
        });
    }
    let reference = row_link?;
    Some(LinkingPages {
        url: url.to_string(),
        lines: vec![line(reference)],
        more: 0,
        // With `--link-details` the result's `links` table has every page (a run whose result is
        // not loaded yet, or has none, knows only this one).
        complete: false,
        live,
    })
}

/// The tone of a status code's chip.
pub fn status_tone(status: i32) -> Tone {
    match StatusClass::of(i64::from(status)) {
        StatusClass::Success => Tone::Success,
        StatusClass::Redirect => Tone::Info,
        StatusClass::ClientError => Tone::Warning,
        StatusClass::ServerError | StatusClass::Error => Tone::Danger,
    }
}

/// The text of a status chip: the code, or what a negative code means.
pub fn status_text(status: i32) -> String {
    match status {
        -1 => "Conn. error".to_string(),
        -2 => "Timeout".to_string(),
        -4 => "Send error".to_string(),
        code if code < 0 => format!("Error {code}"),
        code => code.to_string(),
    }
}

pub fn show(ui: &mut Ui, view: &RunView<'_>, urls: &mut UrlsUi, focus: bool, actions: &mut Vec<RunAction>) {
    let rows = &view.state.urls;
    let revisions = (view.state.revision, view.retry.state.revision);
    if urls.revisions != revisions {
        urls.revisions = revisions;
        urls.titles = None;
        urls.links = None;
        urls.index.invalidate();
    }
    if urls.links.is_none() && !view.live && view.data().is_some() {
        urls.links = Some(LinkColumns::build(view.links.graph, rows));
    }
    if urls.titles.is_none()
        && !view.live
        && let Some(data) = view.data()
    {
        let site = view
            .state
            .started
            .as_ref()
            .map(|started| started.url.as_str())
            .filter(|url| !url.is_empty())
            .unwrap_or(view.url);
        let mut titles = PageTitles::build(data, rows, super::broken::site_host(site).as_deref());
        if !view.retry.state.is_empty() {
            for (index, row) in rows.iter().enumerate() {
                if let Some(title) = view
                    .retry
                    .state
                    .entry(&row.url)
                    .and_then(|entry| entry.newest.as_ref())
                    .and_then(|newest| newest.title())
                {
                    titles.set(index, title);
                }
            }
        }
        urls.titles = Some(titles);
    }
    let titles = urls.titles.take();
    let links = urls.links.take();
    let no_titles = PageTitles::default();
    let no_links = LinkColumns::default();
    let extras = RowExtras {
        titles: titles.as_ref().unwrap_or(&no_titles),
        links: links.as_ref().unwrap_or(&no_links),
    };
    urls.index.sync(rows, extras, &urls.filter, urls.sort, urls.ascending);
    padded(ui, |ui| {
        toolbar(ui, view, urls, focus);
        ui.add_space(theme::GRID);
        if rows.is_empty() {
            widgets::empty_state(
                ui,
                icons::ICON_LINK,
                if view.live {
                    "Waiting for the first URL"
                } else {
                    "No URLs were crawled"
                },
                if view.live {
                    "Every URL the crawler visits appears here as it is crawled."
                } else {
                    "The crawler visited no URL in this run. The Console tab shows why."
                },
                None,
            );
            return;
        }
        if urls.index.rows().is_empty() {
            if widgets::empty_state(
                ui,
                icons::ICON_FILTER_ALT_OFF,
                "No URL matches the filter",
                "Change the filter, or clear it to see every URL.",
                Some("Clear filter"),
            ) {
                urls.filter = UrlFilter::default();
            }
            return;
        }
        url_table(ui, view, urls, extras, actions);
    });
    urls.titles = titles;
    urls.links = links;
}

fn toolbar(ui: &mut Ui, view: &RunView<'_>, urls: &mut UrlsUi, focus: bool) {
    let p = theme::palette(ui);
    let one_row = ui.available_width() >= ONE_ROW_FROM;
    let counts = view.state.status_counts;
    let class_counts = [
        counts.success,
        counts.redirect,
        counts.client_error,
        counts.server_error,
        counts.error,
    ];
    let class_chips = |ui: &mut Ui, urls: &mut UrlsUi| {
        for (slot, ((label, _, color, _), count)) in super::overview::status_classes(&counts, p)
            .into_iter()
            .zip(class_counts)
            .enumerate()
        {
            let color = if slot == 4 { failed_color(p) } else { color };
            if let Some(selected) = urls.filter.classes.get_mut(slot) {
                widgets::filter_chip(ui, selected, label, Some(count), color)
                    .on_hover_text(format!("Show or hide {label} responses"));
            }
        }
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(icons::ICON_SEARCH.rich_text().size(18.0).color(p.text_weak));
        let response = ui
            .add(
                egui::TextEdit::singleline(&mut urls.filter.text)
                    .hint_text("Filter URL or title   (Ctrl+F)")
                    .desired_width(if one_row { 260.0 } else { 300.0 })
                    .margin(egui::vec2(8.0, 5.0)),
            )
            .accessible_name("Filter URLs");
        if focus {
            response.request_focus();
        }
        kind_combo(ui, view, urls);
        if one_row {
            ui.add_space(4.0);
            class_chips(ui, urls);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if view.live {
                let before = urls.follow;
                widgets::toggle_labeled(ui, &mut urls.follow, "Follow")
                    .on_hover_text("Keep the newest URL in view (scrolling or sorting turns it off)");
                if urls.follow && !before && (urls.sort != SortKey::Index || !urls.ascending) {
                    // Following means arrival order.
                    urls.sort = SortKey::Index;
                    urls.ascending = true;
                }
            }
            let shown = urls.index.rows().len() as u64;
            let total = view.state.urls.len() as u64;
            let text = if shown == total {
                format::plural(total, "URL")
            } else {
                format!("{} of {}", format::count(shown), format::plural(total, "URL"))
            };
            ui.label(widgets::weak(ui, text));
            if urls.filter.is_active() && widgets::link_button(ui, "Clear filter").clicked() {
                urls.filter = UrlFilter::default();
            }
        });
    });
    if !one_row {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            class_chips(ui, urls);
        });
    }
}

fn kind_combo(ui: &mut Ui, view: &RunView<'_>, urls: &mut UrlsUi) {
    let selected = urls.filter.kind.map_or("All types", ContentKind::label);
    egui::ComboBox::from_id_salt("run-urls-kind")
        .selected_text(selected)
        .width(130.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut urls.filter.kind, None, "All types");
            for (kind, count) in &view.state.content_counts {
                ui.selectable_value(
                    &mut urls.filter.kind,
                    Some(*kind),
                    format!("{}  ({})", kind.label(), format::count(*count)),
                );
            }
        })
        .response
        .accessible_name("Content type")
        .on_hover_text("Show only one content type");
}

fn url_table(ui: &mut Ui, view: &RunView<'_>, urls: &mut UrlsUi, extras: RowExtras<'_>, actions: &mut Vec<RunAction>) {
    let p = theme::palette(ui);
    let rows = &view.state.urls;
    let (titles, links) = (extras.titles, extras.links);
    // Each link column only when the run has its data: Depth from a crawler that reports it (the
    // `url` event's, then the exact one), In links with `--link-details`.
    let show_depth = rows.first().is_some_and(|row| row.depth.is_some()) || links.is_built();
    let show_in_links = links.has_in_links;
    // A scroll up in the table (wheel, or a press on its scroll bar) releases Follow.
    if urls.follow
        && let Some(rect) = urls.table_rect
    {
        let (pointer, scrolled_up, pressed) = ui.input(|input| {
            (
                input.pointer.hover_pos(),
                input.smooth_scroll_delta.y > 0.0,
                input.pointer.primary_pressed(),
            )
        });
        if let Some(pointer) = pointer
            && rect.contains(pointer)
            && (scrolled_up || (pressed && pointer.x > rect.right() - 14.0))
        {
            urls.follow = false;
        }
    }
    let count = urls.index.rows().len();
    let follow = view.live && urls.follow && count > 0 && urls.followed != Some(count);
    urls.followed = if view.live && urls.follow { Some(count) } else { None };
    let mut sort_click = None;
    let frame = egui::Frame::new()
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(theme::CARD_RADIUS)
        .inner_margin(egui::Margin::symmetric(6, 4));
    let response = frame.show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        // A click on a row's text selects the row, not the text.
        ui.style_mut().interaction.selectable_labels = false;
        // URL and Title share the room the fixed columns leave.
        let link_columns =
            if show_depth { DEPTH_WIDTH } else { 0.0 } + if show_in_links { IN_LINKS_WIDTH } else { 0.0 };
        // #, Status, Type, Time, Size, Cached: a little narrower next to the link columns.
        let fixed: [f32; 6] = if link_columns > 0.0 {
            [64.0, 96.0, 70.0, 70.0, 76.0, 58.0]
        } else {
            [64.0, 96.0, 78.0, 78.0, 84.0, 64.0]
        };
        let url_room = ui.available_width() - (fixed.iter().sum::<f32>() + link_columns);
        let spacing = ui.spacing().item_spacing.x;
        let mut builder = TableBuilder::new(ui)
            .id_salt("run-urls-table")
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center));
        for width in fixed {
            builder = builder.column(Column::exact(width));
        }
        if show_depth {
            builder = builder.column(Column::exact(DEPTH_WIDTH));
        }
        if show_in_links {
            builder = builder.column(Column::exact(IN_LINKS_WIDTH));
        }
        // With the link columns, URL and Title keep less room at least, so that a small window
        // (1000 px) shows every column.
        let (url_least, title_least) = if link_columns > 0.0 {
            (120.0, 90.0)
        } else {
            (160.0, 120.0)
        };
        // The URL's share leaves the Title its least width and the gaps between the columns.
        let gaps = spacing * if link_columns > 0.0 { 10.0 } else { 8.0 };
        let url_width = (url_room * 0.55).min(url_room - title_least - gaps).max(url_least);
        let mut builder = builder
            .column(
                Column::initial(url_width)
                    .at_least(url_least)
                    .clip(true)
                    .resizable(true),
            )
            .column(Column::remainder().at_least(title_least).clip(true))
            .min_scrolled_height(0.0)
            .auto_shrink([false, false])
            .animate_scrolling(false);
        if follow {
            builder = builder.scroll_to_row(count - 1, Some(Align::BOTTOM));
        }
        let header_cell = |header: &mut egui_extras::TableRow<'_, '_>,
                           label: &str,
                           key: SortKey,
                           right: bool,
                           sort_click: &mut Option<SortKey>| {
            header.col(|ui| {
                let sorted = (urls.sort == key).then_some(urls.ascending);
                if sort_header(ui, label, sorted, right).clicked() {
                    *sort_click = Some(key);
                }
            });
        };
        builder
            .header(HEADER_HEIGHT, |mut header| {
                header_cell(&mut header, "#", SortKey::Index, true, &mut sort_click);
                header_cell(&mut header, "Status", SortKey::Status, false, &mut sort_click);
                header_cell(&mut header, "Type", SortKey::Kind, false, &mut sort_click);
                header_cell(&mut header, "Time", SortKey::Time, true, &mut sort_click);
                header_cell(&mut header, "Size", SortKey::Size, true, &mut sort_click);
                header_cell(&mut header, "Cached", SortKey::Cached, false, &mut sort_click);
                if show_depth {
                    header_cell(&mut header, "Depth", SortKey::Depth, true, &mut sort_click);
                }
                if show_in_links {
                    header_cell(&mut header, "In links", SortKey::InLinks, true, &mut sort_click);
                }
                header_cell(&mut header, "URL", SortKey::Url, false, &mut sort_click);
                header_cell(&mut header, "Title", SortKey::Title, false, &mut sort_click);
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, count, |mut row| {
                    let Some(&number) = urls.index.rows().get(row.index()) else {
                        return;
                    };
                    let Some(url) = rows.get(number as usize) else {
                        return;
                    };
                    row.col(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(format::count(u64::from(number) + 1))
                                    .color(p.text_weak)
                                    .size(12.5),
                            );
                        });
                    });
                    row.col(|ui| {
                        widgets::chip(ui, status_text(url.status), status_tone(url.status));
                        super::broken::mark(ui, &view.retry, &url.url);
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(url.kind.label()).color(p.text));
                    });
                    row.col(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let color = match url.time_ms {
                                0..1000 => p.text,
                                1000..3000 => p.warning,
                                _ => p.danger,
                            };
                            ui.label(RichText::new(format::millis(u64::from(url.time_ms))).color(color));
                        });
                    });
                    row.col(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(format::bytes(url.size)).color(p.text));
                        });
                    });
                    row.col(|ui| {
                        if url.cached {
                            ui.label(icons::ICON_CHECK.rich_text().size(16.0).color(p.text_weak))
                                .on_hover_text("Served from the HTTP cache");
                        }
                    });
                    let number_cell = |ui: &mut Ui, value: Option<u32>, tip: &str| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match value {
                            Some(value) => {
                                ui.label(RichText::new(format::count(u64::from(value))).color(p.text))
                                    .on_hover_text(tip);
                            }
                            None => {
                                ui.label(widgets::weak(ui, "—"));
                            }
                        });
                    };
                    if show_depth {
                        row.col(|ui| {
                            number_cell(
                                ui,
                                links.depth(rows, number as usize),
                                "Link distance from a start URL (a redirect adds nothing)",
                            )
                        });
                    }
                    if show_in_links {
                        row.col(|ui| {
                            number_cell(
                                ui,
                                links.in_links(number as usize),
                                "Pages that reference this URL; Pages linking here in the row menu lists them",
                            )
                        });
                    }
                    row.col(|ui| {
                        // Cut in the middle: the end of a URL tells one page from another.
                        let font = egui::TextStyle::Body.resolve(ui.style());
                        let shown = table::middle_truncate(ui, &url.url, &font, ui.available_width());
                        let response = ui.add(egui::Label::new(RichText::new(&shown).color(p.text)).truncate());
                        if shown != *url.url {
                            response.on_hover_text(&*url.url);
                        }
                    });
                    row.col(|ui| {
                        let title = titles.get(number as usize);
                        if !title.is_empty() {
                            table::cell_text(ui, RichText::new(title).color(p.text)).on_hover_text(title);
                        }
                    });
                    row.response().context_menu(|ui| {
                        // A failed row can be retried after the crawl.
                        if !view.live && super::broken::is_broken(url.status) {
                            super::broken::menu_entries(ui, &view.retry, &url.url, &Vec::new, 0, actions);
                        }
                        if widgets::button(ui, Some(icons::ICON_OPEN_IN_NEW), "Open in browser").clicked() {
                            actions.push(RunAction::OpenUrl(url.url.to_string()));
                            ui.close();
                        }
                        if widgets::button(ui, Some(icons::ICON_CONTENT_COPY), "Copy URL").clicked() {
                            actions.push(RunAction::Copy {
                                text: url.url.to_string(),
                                what: "the URL".to_string(),
                            });
                            ui.close();
                        }
                        if let Some(pages) = linking_pages(&url.url, view.links.graph, url.link.as_deref(), view.live)
                            && widgets::button(ui, Some(icons::ICON_ACCOUNT_TREE), "Pages linking here…")
                                .on_hover_text("The pages that reference this URL, with the link text and line")
                                .clicked()
                        {
                            actions.push(RunAction::ShowLinkingPages(pages));
                            ui.close();
                        }
                    });
                });
            });
    });
    urls.table_rect = Some(response.response.rect);
    if let Some(key) = sort_click {
        urls.sort_by(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::results::parse_result;

    const FIXTURE: &str = include_str!("../../../../docs/OUTPUT-crawler.siteone.io.json");

    fn row(url: &str, kind: ContentKind) -> UrlRow {
        UrlRow::new(url, 200, kind, 1, 1, false)
    }

    #[test]
    fn page_titles_come_from_the_seo_table_for_internal_html_pages_only() {
        let data = parse_result(FIXTURE).expect("fixture");
        let rows = vec![
            row("https://crawler.siteone.io/advanced-topics/caching/", ContentKind::Html),
            row(
                "https://www.crawler.siteone.io/advanced-topics/caching/",
                ContentKind::Html,
            ),
            row("https://crawler.siteone.io/advanced-topics/caching/", ContentKind::Css),
            row("https://other.example/advanced-topics/caching/", ContentKind::Html),
            row("https://crawler.siteone.io/no-such-page/", ContentKind::Html),
        ];
        let titles = PageTitles::build(&data, &rows, Some("crawler.siteone.io"));
        assert_eq!(titles.get(0), "Caching • SiteOne Crawler");
        assert_eq!(titles.get(1), "Caching • SiteOne Crawler", "www. is the same site");
        assert_eq!(
            (titles.get(2), titles.get(3), titles.get(4), titles.get(99)),
            ("", "", "", "")
        );

        // The filter and the sort read them; a frame with titles rebuilds once.
        let mut index = UrlIndex::default();
        let filter = UrlFilter {
            text: "caching •".to_string(),
            ..UrlFilter::default()
        };
        let links = LinkColumns::default();
        let no_titles = PageTitles::default();
        let extras = |titles| RowExtras { titles, links: &links };
        index.sync(&rows, extras(&no_titles), &filter, SortKey::Index, true);
        assert!(index.rows().is_empty());
        index.sync(&rows, extras(&titles), &filter, SortKey::Index, true);
        assert_eq!(index.rows(), [0, 1]);
        index.sync(&rows, extras(&titles), &UrlFilter::default(), SortKey::Title, false);
        assert!(
            matches!(index.rows(), [1, 0, ..]),
            "titled rows first, ties reversed too"
        );
    }

    #[test]
    fn depth_and_in_links_come_from_the_link_facts_and_the_linking_pages_from_the_links_table() {
        use crate::model::link_data::{LinkRow, UrlFacts};
        let link = |source: &str, text: &str, line: Option<u32>| LinkRow {
            url: "https://site.example/a".to_string(),
            source_url: source.to_string(),
            source_attr: "<a href>".to_string(),
            link_text: text.to_string(),
            source_line: line,
            source_column: line.map(|_| 7),
            in_links: Some(53),
            external: false,
        };
        let graph = LinkGraph::build(
            [
                link("https://site.example/", "A", Some(12)),
                link("https://site.example/b", "", None),
            ],
            [(
                "https://site.example/a".to_string(),
                UrlFacts {
                    depth: Some(1),
                    in_links: Some(53),
                    out_links: Some(0),
                },
            )],
        );
        let mut rows = vec![
            row("https://site.example/", ContentKind::Html),
            row("https://site.example/a", ContentKind::Html),
            row("https://site.example/b", ContentKind::Html),
        ];
        rows[0].depth = Some(0);
        rows[1].depth = Some(2);
        rows[2].depth = Some(4);
        let links = LinkColumns::build(Some(&graph), &rows);
        assert!(links.has_in_links);
        // The exact depth wins over the event's; without facts, the event's.
        assert_eq!(
            (links.depth(&rows, 0), links.depth(&rows, 1), links.depth(&rows, 2)),
            (Some(0), Some(1), Some(4))
        );
        assert_eq!((links.in_links(0), links.in_links(1)), (None, Some(53)));
        let titles = PageTitles::default();
        let extras = RowExtras {
            titles: &titles,
            links: &links,
        };
        let mut index = UrlIndex::default();
        index.sync(&rows, extras, &UrlFilter::default(), SortKey::InLinks, true);
        assert_eq!(index.rows(), [1, 0, 2], "rows without a count last");
        index.sync(&rows, extras, &UrlFilter::default(), SortKey::Depth, false);
        assert_eq!(index.rows(), [2, 1, 0]);
        // Without link facts: no In links, Depth from the events; an older run has neither.
        assert!(!LinkColumns::build(None, &rows).has_in_links);

        let pages = linking_pages("https://site.example/a", Some(&graph), None, false).expect("pages");
        assert_eq!(
            pages.lines,
            [
                "https://site.example/:12 · <a href> · “A”",
                "https://site.example/b · <a href>"
            ]
        );
        assert_eq!((pages.more, pages.complete), (51, true));
        assert!(
            pages
                .message()
                .starts_with("53 pages reference https://site.example/a.")
        );
        assert!(pages.copy_text().ends_with("\nand 51 more pages\n"));
        // Only the event's page: said so; nothing known: no entry.
        let first = graph
            .target("https://site.example/a")
            .map(|target| target.refs[0].clone());
        let live = linking_pages("https://site.example/x", None, first.as_ref(), true).expect("one page");
        assert_eq!((live.lines.len(), live.complete), (1, false));
        assert!(live.message().contains("While the crawl runs"));
        assert_eq!(linking_pages("https://site.example/", Some(&graph), None, false), None);
    }
}
