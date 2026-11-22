//! The Link checker tab: every crawled URL that answered 4xx or 5xx or failed without an answer,
//! split into internal pages and internal assets (with the page or stylesheet it was found on, from
//! the result JSON's `404` table once the run has finished), and the external links as the app's
//! check after the crawl found them (GUI-SPEC §4.7, §6.3).
//!
//! The crawler does not request other hosts, so crawled URLs are internal; an older run that did
//! crawl external URLs still shows its broken ones as *External*. The check's links are listed
//! when they are broken or refused, and all of them — the working ones with their page title — in
//! the *All external* view.
//!
//! After the crawl, rows can be selected (a checkbox column) and requested again: the retry queue
//! and its passes (GUI-SPEC §4.9) — the *Retry queue* view, the queue bar, the row marks and the
//! actions on the selection are in the `retry` submodule, the selection in `selection`. A retry's
//! answer replaces the row's: a URL that answers OK now leaves the broken views.
//!
//! [`BrokenIndex`] collects the broken crawled rows (and the queued ones) as URL rows
//! arrive (only new rows are looked at; a retry that changes a row rebuilds it); [`BrokenView`]
//! keeps crawled rows and checked links filtered and sorted, rebuilt only when a filter, the sort
//! or a retry changes (new rows and new results are merged in); [`PageView`] lays those rows out
//! *by page* — a heading per page they were found on, then the links of no known page.
//!
//! Above the views a line says what the run did not check (`model::coverage`, the link report's
//! *Not checked* in a few words).

mod retry;
mod selection;

pub use retry::{mark, menu_entries};
pub use selection::Selection;

use std::collections::HashMap;

use eframe::egui::{self, Align, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;

use super::{CheckStatus, RunAction, RunView, padded};
use crate::model::coverage::{self, Gap, GapInput};
use crate::model::link_check::{LinkResult, Outcome, OutcomeCounts, classify_status};
use crate::model::link_report::group_by_page;
use crate::model::link_text::{LinkTexts, RefPosition, TextLookup};
use crate::model::results::{ResultData, ResultLoad};
use crate::model::retry::RetryState;
use crate::model::run_state::{StatusClass, UrlRow};
use crate::theme;
use crate::ui::widgets::table::{HEADER_HEIGHT, ROW_HEIGHT, contains_ignore_case, middle_truncate, sort_header};
use crate::ui::widgets::{self, Tone, format, progress};

/// More new rows than this in one frame are merged by sorting again instead of one by one.
const INCREMENTAL_LIMIT: usize = 64;

/// Where a broken URL lives, relative to the crawled site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Where {
    InternalPage,
    InternalAsset,
    External,
}

impl Where {
    pub const ALL: [Where; 3] = [Where::InternalPage, Where::InternalAsset, Where::External];

    pub fn label(self) -> &'static str {
        match self {
            Where::InternalPage => "Internal page",
            Where::InternalAsset => "Internal asset",
            Where::External => "External",
        }
    }

    fn slot(self) -> usize {
        self as usize
    }
}

/// Whether a status is broken: 4xx, 5xx or a transport error.
pub fn is_broken(status: i32) -> bool {
    matches!(
        StatusClass::of(i64::from(status)),
        StatusClass::ClientError | StatusClass::ServerError | StatusClass::Error
    )
}

/// The status in words for a negative code, the code otherwise.
pub fn status_text(status: i32) -> String {
    match status {
        -1 => "Connection failed".to_string(),
        -2 => "Timeout".to_string(),
        -4 => "Send error".to_string(),
        code if code < 0 => format!("Error {code}"),
        code => code.to_string(),
    }
}

/// The host of a URL for "same site": lower-case, without a leading `www.`.
pub fn site_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    Some(host.strip_prefix("www.").map(str::to_string).unwrap_or(host))
}

/// What a broken URL points to, read from the extension of its path (the reported content type
/// is the error page's, `HTML`, for a missing image).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinkKind {
    Page,
    Image,
    Stylesheet,
    Script,
    Font,
    Document,
    Other,
}

impl LinkKind {
    pub fn label(self) -> &'static str {
        match self {
            LinkKind::Page => "Page",
            LinkKind::Image => "Image",
            LinkKind::Stylesheet => "Stylesheet",
            LinkKind::Script => "Script",
            LinkKind::Font => "Font",
            LinkKind::Document => "Document",
            LinkKind::Other => "Other",
        }
    }
}

/// The extension of the last path segment, lower-case (query and fragment ignored).
fn extension(url: &str) -> Option<String> {
    let path = match url::Url::parse(url.trim()) {
        Ok(parsed) => parsed.path().to_string(),
        Err(_) => url.split(['?', '#']).next().unwrap_or(url).to_string(),
    };
    let last = path.rsplit('/').next().unwrap_or("");
    match last.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => Some(extension.to_ascii_lowercase()),
        _ => None,
    }
}

/// The kind of a URL from its extension: no extension or a page extension is a page.
pub fn link_kind(url: &str) -> LinkKind {
    let Some(extension) = extension(url) else {
        return LinkKind::Page;
    };
    match extension.as_str() {
        "html" | "htm" | "php" | "asp" | "aspx" | "jsp" | "shtml" | "xhtml" => LinkKind::Page,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "avif" | "ico" | "bmp" | "tif" | "tiff" => LinkKind::Image,
        "css" => LinkKind::Stylesheet,
        "js" | "mjs" | "cjs" => LinkKind::Script,
        "woff" | "woff2" | "ttf" | "otf" | "eot" => LinkKind::Font,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" | "rtf" | "txt" | "csv" => {
            LinkKind::Document
        }
        _ => LinkKind::Other,
    }
}

/// Where a broken URL lives and what it points to: External when its host differs from the start
/// URL's `site` host (`www.` aside); otherwise an internal asset when its path has a non-page
/// extension, else an internal page.
pub fn classify(url: &str, site: Option<&str>) -> (Where, LinkKind) {
    let kind = link_kind(url);
    let external = matches!((site, site_host(url).as_deref()), (Some(site), Some(host)) if site != host);
    let place = if external {
        Where::External
    } else if kind == LinkKind::Page {
        Where::InternalPage
    } else {
        Where::InternalAsset
    };
    (place, kind)
}

/// The broken URL rows of a run (and the queued ones), collected as rows arrive.
#[derive(Debug, Default)]
pub struct BrokenIndex {
    /// `(URL row, where, kind)` of every broken or queued row, in arrival order.
    pub entries: Vec<(u32, Where, LinkKind)>,
    /// Counts of the broken rows by [`Where`].
    pub counts: [u64; 3],
    scanned: usize,
    site: Option<String>,
    /// The revisions of the rows and of the retries the index was built for.
    revisions: (u64, u64),
}

impl BrokenIndex {
    /// Brings the index up to date with `rows`; `revision` is theirs (a retry changes rows in
    /// place).
    pub fn sync(&mut self, rows: &[UrlRow], site: Option<&str>, retries: &RetryState, revision: u64) {
        let revisions = (revision, retries.revision);
        if self.site.as_deref() != site || rows.len() < self.scanned || self.revisions != revisions {
            *self = Self {
                site: site.map(str::to_string),
                revisions,
                ..Self::default()
            };
        }
        for (index, row) in rows.iter().enumerate().skip(self.scanned) {
            let broken = is_broken(row.status);
            if (broken || retries.is_queued(&row.url))
                && let Ok(index) = u32::try_from(index)
            {
                let (place, kind) = classify(&row.url, site);
                if broken && let Some(count) = self.counts.get_mut(place.slot()) {
                    *count += 1;
                }
                self.entries.push((index, place, kind));
            }
        }
        self.scanned = rows.len();
    }

    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }
}

/// "Found on" of the result JSON's `404` table: broken URL → referring page or stylesheet.
#[derive(Debug, Default)]
pub struct FoundOn(HashMap<String, String>);

fn join_key(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

impl FoundOn {
    pub fn from_result(data: &ResultData) -> Self {
        let mut map = HashMap::new();
        if let Some(table) = data.table("404") {
            let column = |key: &str| table.columns.iter().position(|column| column.key == key);
            if let (Some(url), Some(source)) = (column("url"), column("sourceUqId")) {
                for row in &table.rows {
                    if let (Some(url), Some(source)) = (row.get(url), row.get(source))
                        && !url.is_empty()
                    {
                        map.entry(join_key(url).to_string()).or_insert_with(|| source.clone());
                    }
                }
            }
        }
        Self(map)
    }

    pub fn get(&self, url: &str) -> Option<&str> {
        self.0.get(join_key(url)).map(String::as_str)
    }
}

/// The views of the tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// Every broken crawled URL and every broken or refused external link.
    #[default]
    AllBroken,
    InternalPages,
    InternalAssets,
    /// Broken or refused external links (and broken crawled external URLs of older runs).
    ExternalBroken,
    /// Every external link, the working ones included.
    AllExternal,
    /// The queued URLs, in queue order.
    RetryQueue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrokenSort {
    /// Where, then broken before refused before OK, then status, then URL.
    #[default]
    Where,
    Status,
    Kind,
    Title,
    Url,
    LinkText,
    FoundOn,
}

/// The filters of the tab.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrokenFilter {
    pub scope: Scope,
    /// Only this status class (set from the Overview's breakdown).
    pub class: Option<StatusClass>,
    pub text: String,
}

/// A row of the tab: a broken crawled URL (position in [`BrokenIndex::entries`]) or a checked
/// external link (position in the check's results).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowRef {
    Crawled(u32),
    Link(u32),
}

/// What a row shows.
enum Item<'a> {
    Crawled {
        row: &'a UrlRow,
        place: Where,
        kind: LinkKind,
        found_on: Option<&'a str>,
    },
    Link(&'a LinkResult),
}

impl<'a> Item<'a> {
    fn url(&self) -> &'a str {
        match self {
            Item::Crawled { row, .. } => &row.url,
            Item::Link(link) => &link.url,
        }
    }

    fn place(&self) -> Where {
        match self {
            Item::Crawled { place, .. } => *place,
            Item::Link(_) => Where::External,
        }
    }

    fn kind(&self) -> LinkKind {
        match self {
            Item::Crawled { kind, .. } => *kind,
            Item::Link(link) => link_kind(&link.url),
        }
    }

    /// Broken first, then refused, then OK.
    fn outcome(&self) -> Outcome {
        match self {
            Item::Crawled { row, .. } if is_broken(row.status) => Outcome::Broken,
            Item::Crawled { .. } => Outcome::Ok,
            Item::Link(link) => link.outcome,
        }
    }

    /// The HTTP status, or a negative number without an answer.
    fn status(&self) -> i64 {
        match self {
            Item::Crawled { row, .. } => i64::from(row.status),
            Item::Link(link) => link.status.map_or(-1, i64::from),
        }
    }

    fn class(&self) -> StatusClass {
        StatusClass::of(self.status())
    }

    fn status_text(&self) -> String {
        match self {
            Item::Crawled { row, .. } => status_text(row.status),
            Item::Link(link) => link.status_label(),
        }
    }

    fn tone(&self) -> Tone {
        match self {
            Item::Crawled { row, .. } => super::urls::status_tone(row.status),
            Item::Link(link) => match (link.outcome, link.status) {
                (Outcome::Refused, _) => Tone::Warning,
                (_, Some(status)) => super::urls::status_tone(i32::from(status)),
                (_, None) => Tone::Danger,
            },
        }
    }

    fn title(&self) -> &str {
        match self {
            Item::Link(link) => link.title.as_deref().unwrap_or(""),
            Item::Crawled { .. } => "",
        }
    }

    /// The first page it was found on.
    fn found_on(&self) -> &str {
        match self {
            Item::Crawled { found_on, .. } => found_on.unwrap_or(""),
            Item::Link(link) => link.sources.first().map_or("", String::as_str),
        }
    }

    /// Every page it was found on.
    fn sources(&self) -> Vec<&str> {
        match self {
            Item::Crawled { found_on, .. } => found_on.iter().copied().collect(),
            Item::Link(link) => link.sources.iter().map(String::as_str).collect(),
        }
    }

    /// The link text on the page(s) it was found on.
    fn link_text<'t>(&self, texts: &'t LinkTexts) -> TextLookup<'t> {
        texts.lookup(self.url(), &self.sources())
    }

    fn in_scope(&self, scope: Scope, retries: &RetryState) -> bool {
        let flagged = self.outcome() != Outcome::Ok;
        let crawled_in = |wanted: Where| matches!(self, Item::Crawled { place, .. } if *place == wanted);
        match scope {
            Scope::AllBroken => flagged,
            Scope::InternalPages => flagged && crawled_in(Where::InternalPage),
            Scope::InternalAssets => flagged && crawled_in(Where::InternalAsset),
            Scope::ExternalBroken => flagged && self.place() == Where::External,
            Scope::AllExternal => self.place() == Where::External,
            Scope::RetryQueue => retries.is_queued(self.url()),
        }
    }
}

/// The filtered and sorted rows.
#[derive(Debug, Default)]
pub struct BrokenView {
    rows: Vec<RowRef>,
    scanned: usize,
    scanned_links: usize,
    built_for: Option<(BrokenFilter, BrokenSort, bool, bool, usize)>,
    /// The revisions of the rows and the retries the rows were built for.
    built_revisions: (u64, u64),
    pub rebuilds: u64,
}

struct Context<'a> {
    rows: &'a [UrlRow],
    entries: &'a [(u32, Where, LinkKind)],
    found_on: Option<&'a FoundOn>,
    links: &'a [LinkResult],
    texts: &'a LinkTexts,
    retries: &'a RetryState,
    /// The revision of the rows (a retry changes rows in place).
    revision: u64,
    /// The default order is the queue's (the *Retry queue* view).
    queue_order: bool,
}

impl<'a> Context<'a> {
    fn item(&self, row: RowRef) -> Option<Item<'a>> {
        match row {
            RowRef::Crawled(position) => {
                let (number, place, kind) = self.entries.get(position as usize)?;
                let row = self.rows.get(*number as usize)?;
                Some(Item::Crawled {
                    row,
                    place: *place,
                    kind: *kind,
                    found_on: self.found_on.and_then(|found| found.get(&row.url)),
                })
            }
            RowRef::Link(position) => self.links.get(position as usize).map(Item::Link),
        }
    }

    fn matches(&self, row: RowRef, filter: &BrokenFilter, needle: &str) -> bool {
        let Some(item) = self.item(row) else {
            return false;
        };
        item.in_scope(filter.scope, self.retries)
            && filter.class.is_none_or(|class| item.class() == class)
            && (contains_ignore_case(item.url(), needle)
                || contains_ignore_case(item.title(), needle)
                || contains_ignore_case(item.link_text(self.texts).text(), needle)
                || item.sources().iter().any(|source| contains_ignore_case(source, needle)))
    }

    fn compare(&self, a: RowRef, b: RowRef, sort: BrokenSort, ascending: bool) -> std::cmp::Ordering {
        let (Some(item_a), Some(item_b)) = (self.item(a), self.item(b)) else {
            return std::cmp::Ordering::Equal;
        };
        let default = || {
            if self.queue_order {
                let position = |item: &Item<'_>| self.retries.queue_position(item.url()).unwrap_or(usize::MAX);
                return position(&item_a).cmp(&position(&item_b));
            }
            item_a
                .place()
                .cmp(&item_b.place())
                .then(item_a.outcome().cmp(&item_b.outcome()))
                .then(item_a.status().cmp(&item_b.status()))
                .then_with(|| item_a.url().cmp(item_b.url()))
        };
        let ordering = match sort {
            BrokenSort::Where => default(),
            BrokenSort::Status => item_a
                .outcome()
                .cmp(&item_b.outcome())
                .then(item_a.status().cmp(&item_b.status()))
                .then_with(default),
            BrokenSort::Kind => item_a.kind().label().cmp(item_b.kind().label()).then_with(default),
            BrokenSort::Title => item_a.title().cmp(item_b.title()).then_with(default),
            BrokenSort::Url => item_a.url().cmp(item_b.url()),
            BrokenSort::LinkText => item_a
                .link_text(self.texts)
                .text()
                .cmp(item_b.link_text(self.texts).text())
                .then_with(default),
            BrokenSort::FoundOn => item_a.found_on().cmp(item_b.found_on()).then_with(default),
        }
        .then_with(|| order_key(a).cmp(&order_key(b)));
        if ascending { ordering } else { ordering.reverse() }
    }
}

/// A total order of rows for ties.
fn order_key(row: RowRef) -> (u8, u32) {
    match row {
        RowRef::Crawled(position) => (0, position),
        RowRef::Link(position) => (1, position),
    }
}

impl BrokenView {
    pub fn rows(&self) -> &[RowRef] {
        &self.rows
    }

    fn sync(&mut self, context: &Context<'_>, filter: &BrokenFilter, sort: BrokenSort, ascending: bool) {
        let joined = context.found_on.is_some();
        // New link texts change what the filter and the sort see.
        let texts = context.texts.pages.len();
        let total = context.entries.len();
        let links = context.links.len();
        let revisions = (context.revision, context.retries.revision);
        // Compared in place: an unchanged frame allocates nothing.
        let current = self.built_revisions == revisions
            && self.built_for.as_ref().is_some_and(
                |(built, built_sort, built_ascending, built_joined, built_texts)| {
                    built == filter
                        && *built_sort == sort
                        && *built_ascending == ascending
                        && *built_joined == joined
                        && *built_texts == texts
                },
            );
        if current && total == self.scanned && links == self.scanned_links {
            return;
        }
        let needle = filter.text.trim().to_lowercase();
        let crawled = |range: std::ops::Range<usize>| {
            range
                .filter_map(|position| u32::try_from(position).ok())
                .map(RowRef::Crawled)
                .filter(|row| context.matches(*row, filter, &needle))
                .collect::<Vec<_>>()
        };
        let checked = |range: std::ops::Range<usize>| {
            range
                .filter_map(|position| u32::try_from(position).ok())
                .map(RowRef::Link)
                .filter(|row| context.matches(*row, filter, &needle))
                .collect::<Vec<_>>()
        };
        if !current || total < self.scanned || links < self.scanned_links {
            self.rebuilds += 1;
            self.rows = crawled(0..total);
            self.rows.extend(checked(0..links));
            self.rows
                .sort_unstable_by(|a, b| context.compare(*a, *b, sort, ascending));
            self.scanned = total;
            self.scanned_links = links;
            self.built_for = Some((filter.clone(), sort, ascending, joined, texts));
            self.built_revisions = revisions;
            return;
        }
        let mut new = crawled(self.scanned..total);
        new.extend(checked(self.scanned_links..links));
        self.scanned = total;
        self.scanned_links = links;
        if new.len() > INCREMENTAL_LIMIT {
            self.rows.extend(new);
            self.rows
                .sort_unstable_by(|a, b| context.compare(*a, *b, sort, ascending));
        } else {
            for row in new {
                let at = self
                    .rows
                    .partition_point(|&other| context.compare(other, row, sort, ascending).is_lt());
                self.rows.insert(at, row);
            }
        }
    }
}

/// A row of the *by page* layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageRow {
    /// The heading of the group at this position in [`PageView::groups`].
    Page(u32),
    /// A link found on the page of the group at this position in [`PageView::groups`].
    Link(RowRef, u32),
}

/// The *by page* layout of the shown rows (Xenu's "broken links, ordered by page"): one group per
/// page they were found on — a link found on several pages is in each — then the links whose page
/// is not known. Built from [`BrokenView`]'s rows, so filter and sort apply (the sort within each
/// group); rebuilt only when those rows change.
#[derive(Debug, Default)]
pub struct PageView {
    rows: Vec<PageRow>,
    /// `(page, number of links)`; `None` is the group of links whose page is not known.
    pub groups: Vec<(Option<String>, usize)>,
    /// The rebuild and length of the view and the filter text the rows were built for (a change of
    /// the sort or the filter rebuilds the view).
    built_for: Option<(u64, usize, String)>,
}

impl PageView {
    pub fn rows(&self) -> &[PageRow] {
        &self.rows
    }

    /// The number of pages with a known address.
    pub fn pages(&self) -> usize {
        self.groups.iter().filter(|(page, _)| page.is_some()).count()
    }

    /// The page of the group at `at` (`None` when it is not known).
    pub fn page(&self, at: u32) -> Option<&str> {
        self.groups.get(at as usize).and_then(|(page, _)| page.as_deref())
    }

    fn sync(&mut self, view: &BrokenView, context: &Context<'_>, filter: &BrokenFilter) {
        let needle = filter.text.trim().to_lowercase();
        let key = (view.rebuilds, view.rows.len(), needle);
        if self.built_for.as_ref() == Some(&key) {
            return;
        }
        let needle = &key.2;
        let items = view.rows.iter().filter_map(|row| {
            let item = context.item(*row)?;
            let sources = item.sources();
            // A link whose own text does not match was shown for the page the filter names: it is
            // listed under that page only.
            let own = needle.is_empty()
                || contains_ignore_case(item.url(), needle)
                || contains_ignore_case(item.title(), needle)
                || contains_ignore_case(item.link_text(context.texts).text(), needle);
            let pages = sources
                .into_iter()
                .filter(|source| own || contains_ignore_case(source, needle))
                .map(str::to_string)
                .collect();
            Some((*row, pages))
        });
        let groups = group_by_page(items);
        self.rows.clear();
        self.groups.clear();
        for (at, group) in groups.into_iter().enumerate() {
            let at = u32::try_from(at).unwrap_or(u32::MAX);
            self.rows.push(PageRow::Page(at));
            self.rows.extend(group.items.iter().map(|row| PageRow::Link(*row, at)));
            self.groups.push((group.page, group.items.len()));
        }
        self.built_for = Some(key);
    }
}

/// Whether a view can be laid out by page: the broken views (*All external* and *Retry queue* list
/// links only).
pub fn groups_by_page(scope: Scope) -> bool {
    matches!(
        scope,
        Scope::AllBroken | Scope::InternalPages | Scope::InternalAssets | Scope::ExternalBroken
    )
}

/// The rows' context of `view` from the parts of a [`BrokenUi`] (so that its selection can change
/// while the context is in use).
fn context_of<'a>(
    index: &'a BrokenIndex,
    found_on: Option<&'a FoundOn>,
    scope: Scope,
    view: &'a RunView<'_>,
) -> Context<'a> {
    Context {
        rows: &view.state.urls,
        entries: &index.entries,
        found_on,
        links: view.links.results,
        texts: view.links.texts,
        retries: view.retry.state,
        revision: view.state.revision,
        queue_order: scope == Scope::RetryQueue,
    }
}

/// A shown row, for Copy list and the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownRow {
    /// The status chip: a code, "Refused 403", or the failure in words.
    pub status: String,
    /// The HTTP status, or a negative number without an answer.
    pub code: i64,
    pub place: Where,
    pub url: String,
    pub title: Option<String>,
    pub final_url: Option<String>,
    /// The link text on the page it was found on, or what became of it ("not found on the page",
    /// "source not fetched", "in a stylesheet"); empty when unknown.
    pub link_text: String,
    /// Every page it was found on (empty when unknown).
    pub sources: Vec<String>,
    /// For each of `sources`, where it references the URL (`:1929`, `:1:20345`), or empty.
    pub positions: Vec<String>,
    /// A checked external link (not a crawled URL).
    pub checked: bool,
}

impl ShownRow {
    /// The first page it was found on.
    pub fn found_on(&self) -> Option<&str> {
        self.sources.first().map(String::as_str)
    }

    /// Every page it was found on, each with where it references the URL (`…/style.css:1929`).
    pub fn found_on_with_positions(&self) -> Vec<String> {
        self.sources
            .iter()
            .enumerate()
            .map(|(at, source)| format!("{source}{}", self.positions.get(at).map_or("", String::as_str)))
            .collect()
    }

    /// Where `page` (one of `sources`, a trailing `/` aside) references the URL, or empty.
    pub fn position_on(&self, page: &str) -> &str {
        let key = page.trim().trim_end_matches('/');
        self.sources
            .iter()
            .position(|source| source.trim().trim_end_matches('/') == key)
            .and_then(|at| self.positions.get(at))
            .map_or("", String::as_str)
    }
}

/// Counts of the views.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScopeCounts {
    pub all_broken: u64,
    pub internal_pages: u64,
    pub internal_assets: u64,
    pub external_broken: u64,
    pub all_external: u64,
    pub retry_queue: u64,
}

/// UI state of the tab.
#[derive(Debug)]
pub struct BrokenUi {
    pub filter: BrokenFilter,
    pub sort: BrokenSort,
    pub ascending: bool,
    pub index: BrokenIndex,
    pub view: BrokenView,
    /// Built once the result JSON is loaded.
    pub found_on: Option<FoundOn>,
    pub counts: ScopeCounts,
    /// The rows chosen with the checkboxes.
    pub selection: Selection,
    /// The broken views are laid out by page (one group per page) instead of by link.
    pub by_page: bool,
    /// The *by page* layout of the shown rows (built while it is shown).
    pub pages: PageView,
    /// The scope the sort was chosen for (a view change resets the sort to its default).
    sorted_scope: Scope,
}

impl Default for BrokenUi {
    fn default() -> Self {
        Self {
            filter: BrokenFilter::default(),
            sort: BrokenSort::Where,
            ascending: true,
            index: BrokenIndex::default(),
            view: BrokenView::default(),
            found_on: None,
            counts: ScopeCounts::default(),
            selection: Selection::default(),
            by_page: false,
            pages: PageView::default(),
            sorted_scope: Scope::AllBroken,
        }
    }
}

impl BrokenUi {
    /// Brings the index, the "found on" join, the counts and the view up to date (cheap when
    /// nothing changed).
    pub fn sync(&mut self, view: &RunView<'_>) {
        let site = view
            .state
            .started
            .as_ref()
            .map(|started| started.url.as_str())
            .filter(|url| !url.is_empty())
            .unwrap_or(view.url);
        let site = site_host(site);
        let retries = view.retry.state;
        self.index
            .sync(&view.state.urls, site.as_deref(), retries, view.state.revision);
        if self.found_on.is_none()
            && let Some(data) = view.data()
        {
            self.found_on = Some(FoundOn::from_result(data));
        }
        let links = view.links.results;
        let outcomes = OutcomeCounts::of(links);
        let flagged = (outcomes.broken + outcomes.refused) as u64;
        let [pages, assets, external] = self.index.counts;
        self.counts = ScopeCounts {
            all_broken: self.index.total() + flagged,
            internal_pages: pages,
            internal_assets: assets,
            external_broken: external + flagged,
            all_external: external + links.len() as u64,
            retry_queue: retries.queue.len() as u64,
        };
        // The queue view is shown only while it has rows.
        if self.filter.scope == Scope::RetryQueue && self.counts.retry_queue == 0 && view.retry.running.is_none() {
            self.filter.scope = Scope::AllBroken;
        }
        if self.sorted_scope != self.filter.scope {
            self.sorted_scope = self.filter.scope;
            self.sort = BrokenSort::Where;
            self.ascending = true;
        }
        let context = Context {
            rows: &view.state.urls,
            entries: &self.index.entries,
            found_on: self.found_on.as_ref(),
            links,
            texts: view.links.texts,
            retries,
            revision: view.state.revision,
            queue_order: self.filter.scope == Scope::RetryQueue,
        };
        self.view.sync(&context, &self.filter, self.sort, self.ascending);
        self.selection.follow(&self.filter, &self.view, &context);
        if self.shows_by_page() {
            self.pages.sync(&self.view, &context, &self.filter);
        }
    }

    /// Whether the shown view is laid out by page.
    pub fn shows_by_page(&self) -> bool {
        self.by_page && groups_by_page(self.filter.scope)
    }

    /// The URL of each row as drawn (`None` for a page heading), for a click on a checkbox.
    fn shown_urls(&self, context: &Context<'_>) -> Vec<Option<String>> {
        let url = |row: RowRef| context.item(row).map(|item| item.url().to_string());
        if self.shows_by_page() {
            self.pages
                .rows()
                .iter()
                .map(|row| match row {
                    PageRow::Page(_) => None,
                    PageRow::Link(row, _) => url(*row),
                })
                .collect()
        } else {
            self.view.rows().iter().map(|row| url(*row)).collect()
        }
    }

    /// The rows' context for `view` (after [`Self::sync`]).
    fn context<'a>(&'a self, view: &'a RunView<'_>) -> Context<'a> {
        context_of(&self.index, self.found_on.as_ref(), self.filter.scope, view)
    }

    /// The tab's badge: every broken crawled URL and every broken or refused external link.
    pub fn broken_total(&self) -> u64 {
        self.counts.all_broken
    }

    fn scope_count(&self, scope: Scope) -> u64 {
        match scope {
            Scope::AllBroken => self.counts.all_broken,
            Scope::InternalPages => self.counts.internal_pages,
            Scope::InternalAssets => self.counts.internal_assets,
            Scope::ExternalBroken => self.counts.external_broken,
            Scope::AllExternal => self.counts.all_external,
            Scope::RetryQueue => self.counts.retry_queue,
        }
    }

    fn sort_by(&mut self, key: BrokenSort) {
        if self.sort == key {
            self.ascending = !self.ascending;
        } else {
            self.sort = key;
            self.ascending = true;
        }
    }

    /// The shown rows (after [`Self::sync`] with the run's URL rows and checked links).
    pub fn shown(
        &self,
        urls: &[UrlRow],
        links: &[LinkResult],
        texts: &LinkTexts,
        retries: &RetryState,
    ) -> Vec<ShownRow> {
        let context = Context {
            rows: urls,
            entries: &self.index.entries,
            found_on: self.found_on.as_ref(),
            links,
            texts,
            retries,
            revision: 0,
            queue_order: false,
        };
        self.view
            .rows()
            .iter()
            .filter_map(|row| context.item(*row))
            .map(|item| shown_row(&item, item.link_text(texts), texts))
            .collect()
    }

    /// The shown rows of the *by page* layout: `(page, its rows)`, `None` for the links whose page
    /// is not known; the link text is the one on that page (after [`Self::sync`]).
    pub fn shown_by_page(
        &self,
        urls: &[UrlRow],
        links: &[LinkResult],
        texts: &LinkTexts,
        retries: &RetryState,
    ) -> Vec<(Option<String>, Vec<ShownRow>)> {
        let context = Context {
            rows: urls,
            entries: &self.index.entries,
            found_on: self.found_on.as_ref(),
            links,
            texts,
            retries,
            revision: 0,
            queue_order: false,
        };
        let mut groups: Vec<(Option<String>, Vec<ShownRow>)> = Vec::new();
        for row in self.pages.rows() {
            match row {
                PageRow::Page(at) => groups.push((self.pages.page(*at).map(str::to_string), Vec::new())),
                PageRow::Link(row, at) => {
                    if let (Some(item), Some((_, rows))) = (context.item(*row), groups.last_mut()) {
                        rows.push(shown_row(&item, page_text(texts, &item, self.pages.page(*at)), texts));
                    }
                }
            }
        }
        groups
    }
}

/// What Copy list and the tests see of a row.
fn shown_row(item: &Item<'_>, link_text: TextLookup<'_>, texts: &LinkTexts) -> ShownRow {
    let sources = item.sources();
    ShownRow {
        status: item.status_text(),
        code: item.status(),
        place: item.place(),
        url: item.url().to_string(),
        title: Some(item.title().to_string()).filter(|title| !title.is_empty()),
        final_url: match item {
            Item::Link(link) => link.final_url.clone(),
            Item::Crawled { .. } => None,
        },
        link_text: link_text.text().to_string(),
        positions: sources
            .iter()
            .map(|source| {
                texts
                    .position(item.url(), source)
                    .map(RefPosition::suffix)
                    .unwrap_or_default()
            })
            .collect(),
        sources: sources.iter().map(|source| source.to_string()).collect(),
        checked: matches!(item, Item::Link(_)),
    }
}

/// The item's own spelling of `page` (the group shows the page as first met; the item's spelling
/// is the key of its texts).
fn own_source<'b>(item: &'b Item<'_>, page: &'b str) -> &'b str {
    let key = page.trim().trim_end_matches('/');
    item.sources()
        .into_iter()
        .find(|source| source.trim().trim_end_matches('/') == key)
        .unwrap_or(page)
}

/// The link text of `item` on `page` (on any page it was found on when the page is not known).
fn page_text<'t>(texts: &'t LinkTexts, item: &Item<'_>, page: Option<&str>) -> TextLookup<'t> {
    let Some(page) = page else {
        return item.link_text(texts);
    };
    texts.lookup(item.url(), &[own_source(item, page)])
}

pub fn show(ui: &mut Ui, view: &RunView<'_>, broken: &mut BrokenUi, focus: bool, actions: &mut Vec<RunAction>) {
    broken.sync(view);
    padded(ui, |ui| {
        if !view.live {
            check_banner(ui, view, actions);
            retry::queue_bar(ui, view, actions);
        }
        let counts = broken.counts;
        let any_rows = counts.all_broken > 0 || counts.all_external > 0 || counts.retry_queue > 0;
        if !any_rows {
            if view.live {
                widgets::empty_state(
                    ui,
                    icons::ICON_LINK_OFF,
                    "No broken links so far",
                    "Pages and assets of the site that answer 4xx or 5xx, or do not answer at all, appear here as soon as the crawler meets them.",
                    None,
                );
            } else {
                no_broken_links(ui, view);
            }
            return;
        }
        toolbar(ui, view, broken, focus, actions);
        ui.add_space(theme::GRID);
        if broken.view.rows().is_empty() {
            let filtered = !broken.filter.text.trim().is_empty() || broken.filter.class.is_some();
            if filtered {
                if widgets::empty_state(
                    ui,
                    icons::ICON_FILTER_ALT_OFF,
                    "No link matches the filter",
                    "Change the filter, or clear it to see every row of this view.",
                    Some("Clear filter"),
                ) {
                    broken.filter.text.clear();
                    broken.filter.class = None;
                }
            } else if broken.filter.scope == Scope::AllExternal {
                widgets::empty_state(
                    ui,
                    icons::ICON_LINK,
                    "No external links checked",
                    "Links to other websites appear here, with their status and page title, once the app has checked them.",
                    None,
                );
            } else {
                // Other views have rows: say only what this view covers.
                match broken.filter.scope {
                    Scope::InternalPages => all_clear(
                        ui,
                        "No broken internal pages",
                        "Every crawled page of the site answered without an error.",
                    ),
                    Scope::InternalAssets => all_clear(
                        ui,
                        "No broken internal assets",
                        "Every crawled image, script, stylesheet and other file of the site answered without an error.",
                    ),
                    Scope::ExternalBroken if matches!(view.links.status, CheckStatus::Done) => all_clear(
                        ui,
                        "No broken external links",
                        "Every external link the app checked answered without an error.",
                    ),
                    Scope::ExternalBroken => {
                        widgets::empty_state(
                            ui,
                            icons::ICON_LINK,
                            "No external links checked",
                            "Broken and refused links to other websites appear here once the app has checked them.",
                            None,
                        );
                    }
                    _ => no_broken_links(ui, view),
                }
            }
            return;
        }
        broken_table(ui, view, broken, actions);
    });
}

/// The "nothing is broken" state.
fn no_broken_links(ui: &mut Ui, view: &RunView<'_>) {
    let text = match view.links.status {
        CheckStatus::Done if !view.links.results.is_empty() => {
            "Every crawled page and asset, and every external link, answered without an error."
        }
        _ => "Every crawled page and asset answered without an error.",
    };
    all_clear(ui, "No broken links found", text);
}

/// A green "nothing here is broken" state.
fn all_clear(ui: &mut Ui, title: &str, text: &str) {
    let p = theme::palette(ui);
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.22).clamp(24.0, 160.0));
        ui.label(icons::ICON_CHECK_CIRCLE.rich_text().size(44.0).color(p.success));
        ui.add_space(theme::GRID);
        ui.label(theme::semibold(title).size(18.0).color(p.text));
        ui.add_space(4.0);
        ui.set_max_width(460.0);
        ui.label(widgets::weak(ui, text));
    });
}

/// Where the external link check stands, with *Check external links now* where it applies, and
/// what is not checked (external files, links over the cap).
fn check_banner(ui: &mut Ui, view: &RunView<'_>, actions: &mut Vec<RunAction>) {
    let links = &view.links;
    let found = links.list.map_or(0, |list| list.links.len());
    let check_now = |ui: &mut Ui, actions: &mut Vec<RunAction>| {
        let response = ui
            .add_enabled(links.blocker.is_none(), egui::Button::new("Check external links now"))
            .accessible_name("Check external links now")
            .on_hover_text("Request each external link once and read its page title; nothing else is loaded")
            .on_disabled_hover_text(links.blocker.unwrap_or(""));
        if response.clicked() {
            actions.push(RunAction::CheckLinks);
        }
    };
    let mut shown = true;
    match &links.status {
        CheckStatus::Running { done, total } => {
            let message = format!(
                "Checking external links: {} of {}. Each link is requested once; only its page title is read.",
                format::count(*done as u64),
                format::count(*total as u64)
            );
            // Stop check is the header's primary action while the check runs.
            let stop = widgets::banner(ui, Tone::Info, &message, |ui| {
                progress::spinner(ui, 16.0, theme::palette(ui).accent);
                false
            });
            if stop {
                actions.push(RunAction::StopCheck);
            }
        }
        CheckStatus::Waiting { another } => {
            let message = if *another {
                format!(
                    "{} will be checked when the check of another run has finished.",
                    format::plural(found as u64, "external link")
                )
            } else {
                format!("{} will be checked now.", format::plural(found as u64, "external link"))
            };
            widgets::banner(ui, Tone::Info, &message, |_| {});
        }
        CheckStatus::Done => {
            let counts = OutcomeCounts::of(links.results);
            if counts.total() > 0 {
                let p = theme::palette(ui);
                ui.horizontal(|ui| {
                    ui.label(icons::ICON_LINK.rich_text().size(16.0).color(p.text_weak));
                    ui.label(widgets::weak(ui, counts.summary()));
                });
            } else {
                shown = false;
            }
        }
        CheckStatus::Stopped if links.unchecked() > 0 => {
            let total = links.report.map_or(found, |report| report.total);
            let message = format!(
                "The external link check was stopped after {} of {} links.",
                format::count(links.results.len() as u64),
                format::count(total as u64)
            );
            widgets::banner(ui, Tone::Info, &message, |ui| check_now(ui, actions));
        }
        CheckStatus::Failed(error) => {
            widgets::banner(ui, Tone::Warning, error, |ui| {
                if found > 0 {
                    check_now(ui, actions);
                }
            });
        }
        CheckStatus::NotRequested | CheckStatus::Stopped if found > 0 => {
            let message = format!(
                "{} were found but not checked.",
                format::plural(found as u64, "external link")
            );
            widgets::banner(ui, Tone::Info, &message, |ui| check_now(ui, actions));
        }
        _ => shown = false,
    }
    // What was not checked, in a few words; the sentences of the link report in the tooltip. The
    // external links the check has not covered are the banner's.
    let gaps = coverage::gaps(&GapInput {
        scope: links.scope,
        outcome: &view.outcome,
        crawled: view.state.urls.len() as u64,
        external_unchecked: 0,
        external_over_cap: links.truncated().0,
        external_files: links.files(),
    });
    if !gaps.is_empty() {
        if shown {
            ui.add_space(4.0);
        }
        let p = theme::palette(ui);
        let short: Vec<String> = gaps.iter().map(Gap::short).collect();
        let full: Vec<String> = gaps.iter().map(Gap::text).collect();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let tip = full.join("\n");
            ui.label(icons::ICON_INFO.rich_text().size(14.0).color(p.text_weak))
                .on_hover_text(&tip);
            // One line, cut at the edge: the tooltip has it all.
            ui.add(
                egui::Label::new(widgets::weak(ui, format!("Not checked: {}", short.join(" · "))).size(12.5))
                    .truncate(),
            )
            .on_hover_text(tip);
        });
        shown = true;
    }
    if shown {
        ui.add_space(theme::GRID);
    }
}

fn toolbar(ui: &mut Ui, view: &RunView<'_>, broken: &mut BrokenUi, focus: bool, actions: &mut Vec<RunAction>) {
    let options = [
        (Scope::AllBroken, "All broken"),
        (Scope::InternalPages, "Internal pages"),
        (Scope::InternalAssets, "Internal assets"),
        (Scope::ExternalBroken, "External broken"),
        (Scope::AllExternal, "All external"),
    ];
    let mut options: Vec<(Scope, &str, u64)> = options
        .into_iter()
        .map(|(scope, label)| (scope, label, broken.scope_count(scope)))
        .collect();
    let queued = broken.scope_count(Scope::RetryQueue);
    if queued > 0 || broken.filter.scope == Scope::RetryQueue {
        options.push((Scope::RetryQueue, "Retry queue", queued));
    }
    // The views and the filter do not fit one line in a small window. After the crawl the line
    // must also hold Link report and the actions of a selection: their room is counted whether or
    // not rows are selected, so that ticking a checkbox never moves the table under the pointer.
    // The By page chip takes its room on either line.
    let room = if options.len() > 5 { 1_400.0 } else { 1_080.0 } + if view.live { 0.0 } else { 380.0 } + 100.0;
    let two_lines = ui.available_width() < room;
    let mut scope = broken.filter.scope;
    if two_lines {
        ui.horizontal(|ui| {
            if widgets::segmented_counts(ui, &mut scope, &options) {
                broken.filter.scope = scope;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| layout_switch(ui, broken));
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| filter_row(ui, view, broken, focus, actions));
    } else {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if widgets::segmented_counts(ui, &mut scope, &options) {
                broken.filter.scope = scope;
            }
            ui.add_space(6.0);
            layout_switch(ui, broken);
            ui.add_space(6.0);
            filter_row(ui, view, broken, focus, actions);
        });
    }
}

/// *By page*: the broken views laid out by page instead of by link (a chip: it fits the views row
/// of a small window next to all six views). *All external* and *Retry queue* are listed by link
/// only.
fn layout_switch(ui: &mut Ui, broken: &mut BrokenUi) {
    let enabled = groups_by_page(broken.filter.scope);
    let mut by_page = broken.by_page && enabled;
    let response = ui
        .add_enabled_ui(enabled, |ui| widgets::check_chip(ui, &mut by_page, "By page"))
        .inner
        .on_hover_text("One group per page, with the broken links found on it; a link found on several pages is listed under each. Off: one row per link, with the page it was found on.")
        .on_disabled_hover_text("This view lists links one by one; By page is for the broken views.");
    if response.clicked() {
        broken.by_page = by_page;
    }
}

/// The text filter, the status-class chip, Copy list and the row count.
fn filter_row(ui: &mut Ui, view: &RunView<'_>, broken: &mut BrokenUi, focus: bool, actions: &mut Vec<RunAction>) {
    let p = theme::palette(ui);
    ui.spacing_mut().item_spacing.x = 6.0;
    ui.label(icons::ICON_SEARCH.rich_text().size(18.0).color(p.text_weak));
    let response = ui
        .add(
            egui::TextEdit::singleline(&mut broken.filter.text)
                .hint_text("Filter URL, text or page")
                .desired_width(220.0)
                .margin(egui::vec2(8.0, 5.0)),
        )
        .accessible_name("Filter links");
    if focus {
        response.request_focus();
    }
    if let Some(class) = broken.filter.class {
        let label = match class {
            StatusClass::ClientError => "Only 4xx",
            StatusClass::ServerError => "Only 5xx",
            _ => "Only failed requests",
        };
        let clear = ui
            .add(
                egui::Button::new((
                    RichText::new(label).color(p.accent),
                    icons::ICON_CLOSE.rich_text().size(14.0).color(p.accent),
                ))
                .fill(p.accent_soft)
                .corner_radius(12),
            )
            .accessible_name("Show every status")
            .on_hover_text("Show links of every status");
        if clear.clicked() {
            broken.filter.class = None;
        }
    }
    let all_external = broken.filter.scope == Scope::AllExternal;
    let by_page = broken.shows_by_page();
    let noun = match broken.filter.scope {
        Scope::AllExternal => "external link",
        Scope::RetryQueue => "queued URL",
        _ => "broken link",
    };
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let copy = ui
            .add(egui::Button::new(widgets::with_icon(
                icons::ICON_CONTENT_COPY,
                "Copy list",
            )))
            .accessible_name("Copy list")
            .on_hover_text(if all_external {
                "Copy the shown rows as tab-separated text (Status, Where, URL, Link text, Found on, Title, Final URL)"
            } else if by_page {
                "Copy the shown rows as tab-separated text, one line per link and page (Found on, Status, Where, URL, Link text)"
            } else {
                "Copy the shown rows as tab-separated text (Status, Where, URL, Link text, Found on)"
            });
        if copy.clicked() {
            let (urls, links, texts, retries) =
                (&view.state.urls, view.links.results, view.links.texts, view.retry.state);
            let (text, count) = if by_page {
                let groups = broken.shown_by_page(urls, links, texts, retries);
                (copy_text_by_page(&groups), broken.view.rows().len())
            } else {
                let rows = broken.shown(urls, links, texts, retries);
                (copy_text(&rows, all_external), rows.len())
            };
            actions.push(RunAction::Copy {
                text,
                what: format::plural(count as u64, noun),
            });
        }
        if !view.live
            && widgets::button(ui, Some(icons::ICON_SUMMARIZE), "Link report")
                .on_hover_text(
                    "Write this run's link report (broken, refused and working links) and open it in the browser",
                )
                .clicked()
        {
            actions.push(RunAction::OpenLinkReport(view.run_dir.to_path_buf(), false));
        }
        if !view.live && !broken.selection.is_empty() {
            // The selection's actions take the place of the row count.
            retry::selection_actions(ui, view, broken, actions);
            return;
        }
        let shown = broken.view.rows().len() as u64;
        let total = broken.scope_count(broken.filter.scope);
        let mut text = if shown == total {
            format::plural(total, noun)
        } else {
            format!("{} of {}", format::count(shown), format::count(total))
        };
        if by_page && broken.pages.pages() > 0 {
            text.push_str(&format!(" on {}", format::plural(broken.pages.pages() as u64, "page")));
        }
        ui.label(widgets::weak(ui, text));
    });
}

/// The *by page* rows as tab-separated text: one line per link and page, the page first (empty
/// when it is not known) with where it references the link (`…/style.css:1929`).
pub fn copy_text_by_page(groups: &[(Option<String>, Vec<ShownRow>)]) -> String {
    let mut text = String::from("Found on\tStatus\tWhere\tURL\tLink text\n");
    for (page, rows) in groups {
        for row in rows {
            let found_on = page
                .as_deref()
                .map(|page| format!("{page}{}", row.position_on(page)))
                .unwrap_or_default();
            text.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                found_on,
                row.status,
                row.place.label(),
                row.url,
                row.link_text
            ));
        }
    }
    text
}

/// The rows as tab-separated text; every page a link was found on (with where it references the
/// link: `…/style.css:1929`), joined by ` | `.
pub fn copy_text(rows: &[ShownRow], all_external: bool) -> String {
    let mut text = String::from(if all_external {
        "Status\tWhere\tURL\tLink text\tFound on\tTitle\tFinal URL\n"
    } else {
        "Status\tWhere\tURL\tLink text\tFound on\n"
    });
    for row in rows {
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}",
            row.status,
            row.place.label(),
            row.url,
            row.link_text,
            row.found_on_with_positions().join(" | ")
        ));
        if all_external {
            text.push_str(&format!(
                "\t{}\t{}",
                row.title.as_deref().unwrap_or(""),
                row.final_url.as_deref().unwrap_or("")
            ));
        }
        text.push('\n');
    }
    text
}

fn broken_table(ui: &mut Ui, view: &RunView<'_>, broken: &mut BrokenUi, actions: &mut Vec<RunAction>) {
    if broken.shows_by_page() {
        page_table(ui, view, broken, actions);
        return;
    }
    let p = theme::palette(ui);
    let mut sort_click = None;
    let count = broken.view.rows().len();
    let result_ready = matches!(view.result, Some(ResultLoad::Loaded(_)));
    let all_external = broken.filter.scope == Scope::AllExternal;
    let scope = broken.filter.scope;
    // Rows are selected for a retry once the crawl has ended.
    let selectable = !view.live;
    let mut click = None;
    let context = broken.context(view);
    let rows = broken.view.rows();
    let selection = &broken.selection;
    let selected = || selection.in_view_order(&broken.view, &context);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(theme::CARD_RADIUS)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let sorted = |key: BrokenSort| (broken.sort == key).then_some(broken.ascending);
            // A click on a row's text selects the row, not the text.
            ui.style_mut().interaction.selectable_labels = false;
            let status_width = 140.0;
            let width = ui.available_width();
            // The room of the resizable text columns, shared out by `parts`.
            let share = |fixed: f32, part: f32| ((width - fixed).max(420.0) * part).max(110.0);
            let text_column = |initial: f32| Column::initial(initial).at_least(90.0).clip(true).resizable(true);
            let check_width = if selectable { 28.0 } else { 0.0 };
            let mut builder = TableBuilder::new(ui)
                .id_salt(if all_external {
                    "run-external-table"
                } else {
                    "run-broken-table"
                })
                .striped(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center));
            if selectable {
                builder = builder.column(Column::exact(check_width));
            }
            builder = builder.column(Column::exact(status_width));
            let status_width = status_width + check_width;
            let headers: &[(&str, BrokenSort)] = if all_external {
                builder = builder
                    .column(text_column(share(status_width, 0.22)))
                    .column(text_column(share(status_width, 0.28)))
                    .column(text_column(share(status_width, 0.22)))
                    .column(Column::remainder().at_least(90.0).clip(true));
                &[
                    ("Status", BrokenSort::Status),
                    ("Title", BrokenSort::Title),
                    ("URL", BrokenSort::Url),
                    ("Link text", BrokenSort::LinkText),
                    ("Found on", BrokenSort::FoundOn),
                ]
            } else {
                // The Kind column becomes State (Retry queue).
                let second = if scope == Scope::RetryQueue { 104.0 } else { 70.0 };
                let fixed = status_width + second + 110.0;
                builder = builder
                    .column(Column::exact(second))
                    .column(Column::exact(110.0))
                    .column(text_column(share(fixed, 0.36)))
                    .column(text_column(share(fixed, 0.25)))
                    .column(Column::remainder().at_least(90.0).clip(true));
                match scope {
                    Scope::RetryQueue => &[
                        ("Status", BrokenSort::Status),
                        ("State", BrokenSort::Kind),
                        ("Where", BrokenSort::Where),
                        ("URL", BrokenSort::Url),
                        ("Link text", BrokenSort::LinkText),
                        ("Found on", BrokenSort::FoundOn),
                    ],
                    _ => &[
                        ("Status", BrokenSort::Status),
                        ("Kind", BrokenSort::Kind),
                        ("Where", BrokenSort::Where),
                        ("URL", BrokenSort::Url),
                        ("Link text", BrokenSort::LinkText),
                        ("Found on", BrokenSort::FoundOn),
                    ],
                }
            };
            builder
                .min_scrolled_height(0.0)
                .auto_shrink([false, false])
                .header(HEADER_HEIGHT, |mut header| {
                    if selectable {
                        header.col(|ui| {
                            if let Some(clicked) = selection::header_checkbox(ui, selection, count) {
                                click = Some(clicked);
                            }
                        });
                    }
                    for (label, key) in headers {
                        header.col(|ui| {
                            // State is not sorted on.
                            if *label == "State" {
                                crate::ui::widgets::table::header_label(ui, label);
                            } else if sort_header(ui, label, sorted(*key), false).clicked() {
                                sort_click = Some(*key);
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, count, |mut row| {
                        let position = row.index();
                        let Some(item) = rows.get(position).and_then(|row| context.item(*row)) else {
                            return;
                        };
                        if selectable {
                            row.col(|ui| {
                                if let Some(clicked) = selection::row_checkbox(ui, selection, position, item.url()) {
                                    click = Some(clicked);
                                }
                            });
                        }
                        row.col(|ui| {
                            let chip = widgets::chip(ui, item.status_text(), item.tone());
                            if let Item::Link(link) = &item
                                && let Some(error) = &link.error
                            {
                                chip.on_hover_text(error);
                            }
                            retry::mark(ui, &view.retry, item.url());
                        });
                        if all_external {
                            row.col(|ui| title_cell(ui, &item));
                        } else {
                            row.col(|ui| match scope {
                                Scope::RetryQueue => retry::state_cell(ui, &view.retry, item.url()),
                                _ => {
                                    ui.label(RichText::new(item.kind().label()).color(p.text));
                                }
                            });
                            row.col(|ui| {
                                let place = item.place();
                                let color = if place == Where::External { p.info } else { p.text };
                                ui.label(RichText::new(place.label()).color(color));
                            });
                        }
                        row.col(|ui| url_cell(ui, &item));
                        row.col(|ui| {
                            link_text_cell(ui, view, item.link_text(view.links.texts), !item.sources().is_empty())
                        });
                        row.col(|ui| found_on_cell(ui, view, &item, result_ready));
                        let response = row.response();
                        if response.double_clicked() {
                            actions.push(RunAction::OpenUrl(item.url().to_string()));
                        }
                        response.context_menu(|ui| {
                            if selectable {
                                let in_selection = if selection.contains(item.url()) {
                                    selection.len()
                                } else {
                                    0
                                };
                                retry::menu_entries(ui, &view.retry, item.url(), &selected, in_selection, actions);
                            }
                            row_menu(ui, &item, actions)
                        });
                    });
                });
        });
    finish_clicks(view, broken, click, sort_click);
}

/// Applies a checkbox click and a sort click of the table drawn this frame.
fn finish_clicks(
    view: &RunView<'_>,
    broken: &mut BrokenUi,
    click: Option<selection::Click>,
    sort_click: Option<BrokenSort>,
) {
    if let Some(click) = click {
        let context = context_of(&broken.index, broken.found_on.as_ref(), broken.filter.scope, view);
        let shown = broken.shown_urls(&context);
        broken.selection.apply(click, &shown);
    }
    if let Some(key) = sort_click {
        broken.sort_by(key);
    }
}

/// The *by page* layout: a heading row per page (the page, the number of links on it), under it
/// the links found on that page — Status, Kind, Where, URL, the line where that page references
/// the link and the link text on that page. The
/// checkboxes, the row menu, the sort (within each page) and the filter work as by link.
fn page_table(ui: &mut Ui, view: &RunView<'_>, broken: &mut BrokenUi, actions: &mut Vec<RunAction>) {
    // Turned on in this frame's toolbar, after the sync: bring the layout up to date first.
    {
        let context = context_of(&broken.index, broken.found_on.as_ref(), broken.filter.scope, view);
        broken.pages.sync(&broken.view, &context, &broken.filter);
    }
    let p = theme::palette(ui);
    let mut sort_click = None;
    let mut click = None;
    let selectable = !view.live;
    let result_ready = matches!(view.result, Some(ResultLoad::Loaded(_)));
    let context = broken.context(view);
    let pages = &broken.pages;
    let rows = pages.rows();
    let count = rows.len();
    let links = broken.view.rows().len();
    let selection = &broken.selection;
    let selected = || selection.in_view_order(&broken.view, &context);
    let unknown = if view.live {
        "Pages known when the crawl finishes"
    } else if !result_ready {
        "Page not available"
    } else {
        "Page not reported by the crawler"
    };
    egui::Frame::new()
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(theme::CARD_RADIUS)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.style_mut().interaction.selectable_labels = false;
            let sorted = |key: BrokenSort| (broken.sort == key).then_some(broken.ascending);
            let across = ui.max_rect().x_range();
            let check_width = if selectable { 28.0 } else { 0.0 };
            let fixed = check_width + 140.0 + 70.0 + 110.0 + LINE_WIDTH;
            let url_width = ((ui.available_width() - fixed).max(420.0) * 0.6).max(110.0);
            let mut builder = TableBuilder::new(ui)
                .id_salt("run-broken-by-page-table")
                .striped(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center));
            if selectable {
                builder = builder.column(Column::exact(check_width));
            }
            builder
                .column(Column::exact(140.0))
                .column(Column::exact(70.0))
                .column(Column::exact(110.0))
                .column(Column::exact(url_width).clip(true))
                .column(Column::exact(LINE_WIDTH).clip(true))
                .column(Column::remainder().at_least(90.0).clip(true))
                .min_scrolled_height(0.0)
                .auto_shrink([false, false])
                .header(HEADER_HEIGHT, |mut header| {
                    if selectable {
                        header.col(|ui| {
                            if let Some(clicked) = selection::header_checkbox(ui, selection, links) {
                                click = Some(clicked);
                            }
                        });
                    }
                    for (label, key) in [
                        ("Status", Some(BrokenSort::Status)),
                        ("Kind", Some(BrokenSort::Kind)),
                        ("Where", Some(BrokenSort::Where)),
                        ("URL", Some(BrokenSort::Url)),
                        // Where the page references the link (not sorted on).
                        ("Line", None),
                        ("Link text", Some(BrokenSort::LinkText)),
                    ] {
                        header.col(|ui| match key {
                            Some(key) => {
                                if sort_header(ui, label, sorted(key), false).clicked() {
                                    sort_click = Some(key);
                                }
                            }
                            None => crate::ui::widgets::table::header_label(ui, label),
                        });
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, count, |mut row| {
                        let position = row.index();
                        match rows.get(position) {
                            Some(PageRow::Page(at)) => {
                                let (page, links) = pages
                                    .groups
                                    .get(*at as usize)
                                    .map_or((None, 0), |(page, links)| (page.as_deref(), *links));
                                row.col(|ui| page_heading(ui, across, page, links, unknown));
                                let response = row.response();
                                match page {
                                    Some(page) => {
                                        if response.double_clicked() {
                                            actions.push(RunAction::OpenUrl(page.to_string()));
                                        }
                                        let response = response.on_hover_text(page);
                                        response.context_menu(|ui| page_menu(ui, page, actions));
                                    }
                                    None => {
                                        response.on_hover_text(
                                            "The crawler reports the page a URL was found on only for a 404, and only the first page that links to it.",
                                        );
                                    }
                                }
                            }
                            Some(PageRow::Link(link_row, at)) => {
                                let Some(item) = context.item(*link_row) else {
                                    return;
                                };
                                let page = pages.page(*at);
                                if selectable {
                                    row.col(|ui| {
                                        if let Some(clicked) =
                                            selection::row_checkbox(ui, selection, position, item.url())
                                        {
                                            click = Some(clicked);
                                        }
                                    });
                                }
                                row.col(|ui| {
                                    let chip = widgets::chip(ui, item.status_text(), item.tone());
                                    if let Item::Link(link) = &item
                                        && let Some(error) = &link.error
                                    {
                                        chip.on_hover_text(error);
                                    }
                                    retry::mark(ui, &view.retry, item.url());
                                });
                                row.col(|ui| {
                                    ui.label(RichText::new(item.kind().label()).color(p.text));
                                });
                                row.col(|ui| {
                                    let place = item.place();
                                    let color = if place == Where::External { p.info } else { p.text };
                                    ui.label(RichText::new(place.label()).color(color));
                                });
                                row.col(|ui| url_cell(ui, &item));
                                row.col(|ui| {
                                    let position = page
                                        .and_then(|page| view.links.texts.position(item.url(), own_source(&item, page)));
                                    line_cell(ui, position);
                                });
                                row.col(|ui| {
                                    let text = page_text(view.links.texts, &item, page);
                                    link_text_cell(ui, view, text, page.is_some() || !item.sources().is_empty())
                                });
                                let response = row.response();
                                if response.double_clicked() {
                                    actions.push(RunAction::OpenUrl(item.url().to_string()));
                                }
                                response.context_menu(|ui| {
                                    if selectable {
                                        let in_selection = if selection.contains(item.url()) {
                                            selection.len()
                                        } else {
                                            0
                                        };
                                        retry::menu_entries(ui, &view.retry, item.url(), &selected, in_selection, actions);
                                    }
                                    row_menu(ui, &item, actions)
                                });
                            }
                            None => {}
                        }
                    });
                });
        });
    finish_clicks(view, broken, click, sort_click);
}

/// The heading row of a page, drawn across the whole row: the page (cut in the middle) or what is
/// known of it, and the number of links found on it.
fn page_heading(ui: &mut Ui, across: egui::Rangef, page: Option<&str>, links: usize, unknown: &str) {
    let p = theme::palette(ui);
    let cell = ui.max_rect();
    let row = egui::Rect::from_x_y_ranges(across, cell.y_range());
    let mut painter = ui.painter().clone();
    painter.set_clip_rect(egui::Rect::from_x_y_ranges(across, ui.clip_rect().y_range()));
    painter.rect_filled(row, 0.0, p.surface_alt);
    let y = row.center().y;
    let icon = if page.is_some_and(is_stylesheet) {
        icons::ICON_CSS
    } else if page.is_some() {
        icons::ICON_DESCRIPTION
    } else {
        icons::ICON_HELP
    };
    painter.text(
        egui::pos2(row.left() + 8.0, y),
        egui::Align2::LEFT_CENTER,
        icon.codepoint,
        egui::FontId::new(16.0, icon.font_family()),
        p.text_weak,
    );
    let count = format::plural(links as u64, "broken link");
    let count_font = egui::FontId::proportional(12.5);
    let count_width = painter
        .layout_no_wrap(count.clone(), count_font.clone(), p.text_weak)
        .size()
        .x;
    let left = row.left() + 30.0;
    let room = (row.width() - 30.0 - count_width - 24.0).max(60.0);
    let (text, font, color) = match page {
        Some(page) => (page, theme::semibold_font(14.0), p.text),
        None => (unknown, egui::FontId::proportional(14.0), p.text_weak),
    };
    // Painted across the row, so it is named for screen readers (and tests) here.
    let name = match page {
        Some(page) => format!("Page {page}, {count}"),
        None => format!("{unknown}, {count}"),
    };
    ui.allocate_rect(cell, Sense::hover())
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &name));
    let shown = middle_truncate(ui, text, &font, room);
    let galley = painter.layout_no_wrap(shown, font, color);
    let text_width = galley.size().x;
    painter.galley(egui::pos2(left, y - galley.size().y / 2.0), galley, color);
    painter.text(
        egui::pos2(left + text_width + 12.0, y),
        egui::Align2::LEFT_CENTER,
        count,
        count_font,
        p.text_weak,
    );
}

/// The menu of a page heading.
fn page_menu(ui: &mut Ui, page: &str, actions: &mut Vec<RunAction>) {
    if widgets::button(ui, Some(icons::ICON_OPEN_IN_NEW), "Open the page").clicked() {
        actions.push(RunAction::OpenUrl(page.to_string()));
        ui.close();
    }
    if widgets::button(ui, Some(icons::ICON_CONTENT_COPY), "Copy page URL").clicked() {
        actions.push(RunAction::Copy {
            text: page.to_string(),
            what: "the page URL".to_string(),
        });
        ui.close();
    }
}

/// A cell with `text` cut in the middle to the cell's width; the full text in the tooltip (with
/// `tip` after it, when given).
fn middle_cell(ui: &mut Ui, text: &str, color: egui::Color32, tip: Option<String>) -> egui::Response {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let shown = middle_truncate(ui, text, &font, ui.available_width());
    let response = ui.add(egui::Label::new(RichText::new(&shown).color(color)).truncate());
    match (shown != text, tip) {
        (true, Some(tip)) => response.on_hover_text(format!("{text}\n{tip}")),
        (true, None) => response.on_hover_text(text),
        (false, Some(tip)) => response.on_hover_text(tip),
        (false, None) => response,
    }
}

fn title_cell(ui: &mut Ui, item: &Item<'_>) {
    let p = theme::palette(ui);
    match item {
        Item::Link(link) => match &link.title {
            Some(title) => {
                middle_cell(ui, title, p.text, None);
            }
            None if link.outcome == Outcome::Ok => {
                ui.label(widgets::weak(ui, "no title"))
                    .on_hover_text("The answer is not an HTML page, or the page has no title in its first 256 KB.");
            }
            None => {
                ui.label(widgets::weak(ui, "—"));
            }
        },
        Item::Crawled { .. } => {
            ui.label(widgets::weak(ui, "crawled"))
                .on_hover_text("Crawled by an older version of the external link check.");
        }
    }
}

fn url_cell(ui: &mut Ui, item: &Item<'_>) {
    let p = theme::palette(ui);
    let mut tip = Vec::new();
    if let Item::Link(link) = item {
        if let Some(title) = &link.title {
            tip.push(title.clone());
        }
        if let Some(final_url) = &link.final_url {
            tip.push(format!("Redirects to {final_url}"));
        }
        if let Some(status) = link.status
            && classify_status(status) != link.outcome
        {
            tip.push(format!("Status {status}"));
        }
    }
    let tip = (!tip.is_empty()).then(|| tip.join("\n"));
    let url = item.url();
    // The URL heads the tooltip when it explains more than the URL.
    let tip = tip.map(|tip| if url.is_empty() { tip } else { format!("{url}\n{tip}") });
    let font = egui::TextStyle::Body.resolve(ui.style());
    let shown = middle_truncate(ui, url, &font, ui.available_width());
    let response = ui.add(egui::Label::new(RichText::new(&shown).color(p.text)).truncate());
    match tip {
        Some(tip) => {
            response.on_hover_text(tip);
        }
        None if shown != url => {
            response.on_hover_text(url);
        }
        None => {}
    }
}

/// The link text: the text of the link on the page it was found on (`lookup`), or what became of
/// it; `has_source`: the page is known.
fn link_text_cell(ui: &mut Ui, view: &RunView<'_>, lookup: TextLookup<'_>, has_source: bool) {
    let p = theme::palette(ui);
    let weak = |ui: &mut Ui, text: &str, tip: &str| {
        ui.label(widgets::weak(ui, text)).on_hover_text(tip);
    };
    match lookup {
        TextLookup::Text(text) => {
            middle_cell(ui, text, p.text, None);
        }
        TextLookup::NotFound => weak(
            ui,
            "not found on the page",
            "The page it was found on does not reference this URL now: it changed, or a script builds the link.",
        ),
        TextLookup::Stylesheet => weak(
            ui,
            "in a stylesheet",
            "Referenced by url() or @import in a stylesheet: a stylesheet has no link text. Found on gives the line.",
        ),
        TextLookup::NotInStylesheet => weak(
            ui,
            "not found in the stylesheet",
            "The stylesheet it was found on does not reference this URL now: it changed, or a script builds the URL.",
        ),
        TextLookup::NotFetched(reason) => weak(ui, "source not fetched", reason),
        TextLookup::Unknown if !has_source => {
            ui.label(widgets::weak(ui, "—"));
        }
        TextLookup::Unknown if view.links.texts_pending => weak(
            ui,
            "pending…",
            "The page it was found on is being fetched to read the link text.",
        ),
        TextLookup::Unknown => weak(
            ui,
            "source not fetched",
            "The page it was found on has not been fetched. For external links, Check external links now fetches it.",
        ),
    }
}

fn found_on_cell(ui: &mut Ui, view: &RunView<'_>, item: &Item<'_>, result_ready: bool) {
    let sources = item.sources();
    match sources.first() {
        Some(source) => {
            let texts = view.links.texts;
            let position = texts.position(item.url(), source);
            // With a position, the cell ends with `.css:1929`: the chip would only take its room.
            if is_stylesheet(source) && position.is_none() {
                widgets::chip(ui, "CSS", Tone::Accent)
                    .on_hover_text("Referenced from a stylesheet (e.g. a background image)");
            }
            if sources.len() > 1 {
                // The chip first: the URL is cut at the column's edge.
                let all: Vec<String> = sources
                    .iter()
                    .map(|source| texts.found_on_with_position(item.url(), source))
                    .collect();
                widgets::chip(ui, format!("+{} more", sources.len() - 1), Tone::Neutral).on_hover_text(all.join("\n"));
            }
            source_cell(ui, source, position);
        }
        None if view.live => {
            ui.label(widgets::weak(ui, "available when the crawl finishes"));
        }
        None if !result_ready => {
            ui.label(widgets::weak(ui, "not available"))
                .on_hover_text("This run has no readable result file.");
        }
        None => {
            ui.label(widgets::weak(ui, "not reported by the crawler"))
                .on_hover_text("The crawler records the referring page for 404 responses only.");
        }
    }
}

/// The width of the *Line* column of the by-page layout.
const LINE_WIDTH: f32 = 84.0;

/// Where a page references a link, in words, for a tooltip.
fn position_tip(position: &RefPosition) -> String {
    format!(
        "Referenced at {} in the file as fetched after the crawl.",
        position.describe()
    )
}

/// A page a link was found on, cut in the middle to the cell's width — never its position
/// (`:1929`), which is the point of it; the full page and the position in words in the tooltip.
/// Opening the page opens it without the position.
fn source_cell(ui: &mut Ui, source: &str, position: Option<&RefPosition>) {
    let p = theme::palette(ui);
    let Some(position) = position else {
        middle_cell(ui, source, p.text, None);
        return;
    };
    let font = egui::TextStyle::Body.resolve(ui.style());
    let suffix = position.suffix();
    let suffix_width = ui
        .painter()
        .layout_no_wrap(suffix.clone(), font.clone(), p.text)
        .size()
        .x;
    let shown = middle_truncate(ui, source, &font, (ui.available_width() - suffix_width - 2.0).max(0.0));
    let mut job = egui::text::LayoutJob::default();
    job.append(&shown, 0.0, egui::TextFormat::simple(font.clone(), p.text));
    job.append(&suffix, 0.0, egui::TextFormat::simple(font, p.accent));
    ui.add(egui::Label::new(job).extend())
        .on_hover_text(format!("{source}{suffix}\n{}", position_tip(position)));
}

/// The *Line* cell of the by-page layout: `1929`, or `1:20345` on a long line.
fn line_cell(ui: &mut Ui, position: Option<&RefPosition>) {
    let p = theme::palette(ui);
    match position {
        Some(position) => {
            let line = position.suffix();
            ui.label(RichText::new(line.trim_start_matches(':')).color(p.accent))
                .on_hover_text(position_tip(position));
        }
        None => {
            ui.label(widgets::weak(ui, "—")).on_hover_text(
                "No position: the page was not read, or it does not reference this URL now (it changed, or a script builds the link).",
            );
        }
    }
}

fn row_menu(ui: &mut Ui, item: &Item<'_>, actions: &mut Vec<RunAction>) {
    let url = item.url().to_string();
    let found = item.found_on().to_string();
    if widgets::button(ui, Some(icons::ICON_OPEN_IN_NEW), "Open URL").clicked() {
        actions.push(RunAction::OpenUrl(url.clone()));
        ui.close();
    }
    if let Item::Link(link) = item
        && let Some(final_url) = &link.final_url
        && widgets::button(ui, Some(icons::ICON_OPEN_IN_NEW), "Open where it redirects").clicked()
    {
        actions.push(RunAction::OpenUrl(final_url.clone()));
        ui.close();
    }
    if !found.is_empty()
        && widgets::button(ui, Some(icons::ICON_DESCRIPTION), "Open the page it was found on").clicked()
    {
        actions.push(RunAction::OpenUrl(found.clone()));
        ui.close();
    }
    if widgets::button(ui, Some(icons::ICON_CONTENT_COPY), "Copy URL").clicked() {
        actions.push(RunAction::Copy {
            text: url.clone(),
            what: "the URL".to_string(),
        });
        ui.close();
    }
    if !found.is_empty() && widgets::button(ui, Some(icons::ICON_CONTENT_COPY), "Copy URL ← found on").clicked() {
        actions.push(RunAction::Copy {
            text: format!("{url} ← {}", item.sources().join(" | ")),
            what: "the URL and where it was found".to_string(),
        });
        ui.close();
    }
}

/// Whether a URL points to a stylesheet (`.css`, query and fragment aside).
fn is_stylesheet(url: &str) -> bool {
    url.split(['?', '#'])
        .next()
        .is_some_and(|path| path.to_ascii_lowercase().ends_with(".css"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::link_check::Failure;
    use crate::model::run_state::ContentKind;

    fn row(url: &str, status: i32, kind: ContentKind) -> UrlRow {
        UrlRow {
            url: url.into(),
            status,
            kind,
            time_ms: 1,
            size: 1,
            cached: false,
        }
    }

    fn link(url: &str, outcome: Outcome, status: Option<u16>) -> LinkResult {
        LinkResult {
            url: url.to_string(),
            sources: vec!["https://site.example/".to_string()],
            count: 1,
            outcome,
            status,
            failure: status.is_none().then_some(Failure::Dns),
            ..LinkResult::default()
        }
    }

    #[test]
    fn urls_are_classified_by_their_address_not_their_content_type() {
        let site = site_host("http://127.0.0.1:8911/");
        let site = site.as_deref();
        assert_eq!(
            classify("http://127.0.0.1:8911/img/missing-background.jpg", site),
            (Where::InternalAsset, LinkKind::Image)
        );
        assert_eq!(
            classify("http://127.0.0.1:8911/missing-page.html", site),
            (Where::InternalPage, LinkKind::Page)
        );
        assert_eq!(
            classify("http://127.0.0.1:8911/practice_areas/agricultural-law/", site),
            (Where::InternalPage, LinkKind::Page)
        );
        assert_eq!(
            classify("https://example.com/missing-external-bg.png", site),
            (Where::External, LinkKind::Image)
        );
        assert_eq!(
            classify("http://dead-domain.invalid/page", site),
            (Where::External, LinkKind::Page)
        );
        // www. and the bare host are one site; the query and fragment do not count.
        let site = site_host("https://www.example.com/");
        assert_eq!(
            classify("https://example.com/css/site.css?v=3#x", site.as_deref()),
            (Where::InternalAsset, LinkKind::Stylesheet)
        );
        assert_eq!(link_kind("https://example.com/fonts/a.woff2"), LinkKind::Font);
        assert_eq!(link_kind("https://example.com/docs/terms.pdf"), LinkKind::Document);
        assert_eq!(link_kind("https://example.com/app.mjs"), LinkKind::Script);
        assert_eq!(link_kind("https://example.com/archive.zip"), LinkKind::Other);
        assert_eq!(link_kind("https://example.com/index.php?id=4"), LinkKind::Page);
    }

    #[test]
    fn statuses_and_stylesheets() {
        assert!(is_broken(404) && is_broken(503) && is_broken(-2) && !is_broken(301) && !is_broken(200));
        assert_eq!(status_text(-1), "Connection failed");
        assert_eq!(status_text(-9), "Error -9");
        assert!(is_stylesheet("https://example.com/css/site.css?v=3"));
    }

    #[test]
    fn the_index_only_looks_at_new_rows() {
        let mut rows = vec![
            row("https://example.com/", 200, ContentKind::Html),
            row("https://example.com/a", 404, ContentKind::Html),
        ];
        let mut index = BrokenIndex::default();
        index.sync(&rows, Some("example.com"), &RetryState::default(), 0);
        assert_eq!(index.entries, vec![(1, Where::InternalPage, LinkKind::Page)]);
        rows.push(row("https://ext.org/", -2, ContentKind::Other));
        index.sync(&rows, Some("example.com"), &RetryState::default(), 0);
        assert_eq!(index.counts, [1, 0, 1]);
    }

    #[test]
    fn checked_links_join_the_views_broken_first() {
        let no_retries = RetryState::default();
        let rows = vec![row("https://site.example/gone", 404, ContentKind::Html)];
        let texts = LinkTexts::default();
        let mut index = BrokenIndex::default();
        index.sync(&rows, Some("site.example"), &RetryState::default(), 0);
        let mut links = vec![
            link("https://ok.example/", Outcome::Ok, Some(200)),
            link("https://refused.example/", Outcome::Refused, Some(403)),
        ];
        let mut view = BrokenView::default();
        let filter = |scope| BrokenFilter {
            scope,
            ..BrokenFilter::default()
        };
        let context = |links: &[LinkResult]| -> Vec<RowRef> {
            let context = Context {
                rows: &rows,
                entries: &index.entries,
                found_on: None,
                links,
                texts: &texts,
                retries: &no_retries,
                revision: 0,
                queue_order: false,
            };
            let mut view = BrokenView::default();
            view.sync(&context, &filter(Scope::AllExternal), BrokenSort::Where, true);
            view.rows
        };
        assert_eq!(context(&links), [RowRef::Link(1), RowRef::Link(0)]);

        // All broken: the crawled 404 and the refused link, not the working one.
        let all_broken = {
            let context = Context {
                rows: &rows,
                entries: &index.entries,
                found_on: None,
                links: &links,
                texts: &texts,
                retries: &no_retries,
                revision: 0,
                queue_order: false,
            };
            view.sync(&context, &filter(Scope::AllBroken), BrokenSort::Where, true);
            view.rows.clone()
        };
        assert_eq!(all_broken, [RowRef::Crawled(0), RowRef::Link(1)]);

        // A new broken result is merged in without a rebuild, before the refused one.
        links.push(link("https://dead.example/", Outcome::Broken, None));
        let rebuilds = view.rebuilds;
        let context_now = Context {
            rows: &rows,
            entries: &index.entries,
            found_on: None,
            links: &links,
            texts: &texts,
            retries: &no_retries,
            revision: 0,
            queue_order: false,
        };
        view.sync(&context_now, &filter(Scope::AllBroken), BrokenSort::Where, true);
        assert_eq!(view.rows, [RowRef::Crawled(0), RowRef::Link(2), RowRef::Link(1)]);
        assert_eq!(view.rebuilds, rebuilds);
        // The text filter also searches the title and every page a link was found on.
        let mut by_text = filter(Scope::AllExternal);
        by_text.text = "SITE.example".to_string();
        view.sync(&context_now, &by_text, BrokenSort::Where, true);
        assert_eq!(view.rows.len(), 3);
        // A new check (fewer results) rebuilds.
        let fresh = vec![link("https://ok.example/", Outcome::Ok, Some(200))];
        let context_fresh = Context {
            rows: &rows,
            entries: &index.entries,
            found_on: None,
            links: &fresh,
            texts: &texts,
            retries: &no_retries,
            revision: 0,
            queue_order: false,
        };
        view.sync(&context_fresh, &by_text, BrokenSort::Where, true);
        assert_eq!(view.rows, [RowRef::Link(0)]);
    }

    #[test]
    fn copy_list_names_every_page_a_link_was_found_on() {
        let rows = vec![ShownRow {
            status: "Refused 403".to_string(),
            code: 403,
            place: Where::External,
            url: "https://refused.example/".to_string(),
            title: Some("Forbidden".to_string()),
            final_url: None,
            link_text: "Our partner".to_string(),
            sources: vec![
                "https://site.example/a".to_string(),
                "https://site.example/b".to_string(),
            ],
            positions: vec![":212".to_string(), String::new()],
            checked: true,
        }];
        // Each page with where it references the link, when it is known.
        assert_eq!(
            copy_text(&rows, false),
            "Status\tWhere\tURL\tLink text\tFound on\nRefused 403\tExternal\thttps://refused.example/\tOur partner\thttps://site.example/a:212 | https://site.example/b\n"
        );
        assert!(copy_text(&rows, true).contains("\tForbidden\t\n"));
    }

    /// The URLs of `scope`'s rows.
    fn scope_urls(
        rows: &[UrlRow],
        index: &BrokenIndex,
        retries: &RetryState,
        revision: u64,
        scope: Scope,
    ) -> Vec<String> {
        let texts = LinkTexts::default();
        let context = Context {
            rows,
            entries: &index.entries,
            found_on: None,
            links: &[],
            texts: &texts,
            retries,
            revision,
            queue_order: scope == Scope::RetryQueue,
        };
        let mut view = BrokenView::default();
        let filter = BrokenFilter {
            scope,
            ..BrokenFilter::default()
        };
        view.sync(&context, &filter, BrokenSort::Where, true);
        view.rows()
            .iter()
            .filter_map(|row| context.item(*row))
            .map(|item| item.url().to_string())
            .collect()
    }

    #[test]
    fn a_row_retried_ok_leaves_the_broken_views_and_the_queue_view_keeps_queue_order() {
        use crate::model::retry::{Answer, CrawledAnswer};
        let mut state = crate::model::run_state::RunState::new();
        for (url, status) in [
            ("https://site.example/a", 404),
            ("https://site.example/b", -2),
            ("https://site.example/c", 500),
        ] {
            state.urls.push(row(url, status, ContentKind::Html));
        }
        let first = |row: &UrlRow| Answer::Crawled(CrawledAnswer::of_row(row));
        let mut retries = RetryState::default();
        retries.enqueue("https://site.example/c", first(&state.urls[2]));
        retries.enqueue("https://site.example/a", first(&state.urls[0]));
        let mut index = BrokenIndex::default();
        index.sync(&state.urls, Some("site.example"), &retries, state.revision);
        let urls = |index: &BrokenIndex, retries: &RetryState, revision, scope| {
            scope_urls(&state.urls, index, retries, revision, scope)
        };
        assert_eq!(
            urls(&index, &retries, 0, Scope::RetryQueue),
            ["https://site.example/c", "https://site.example/a"],
            "queue order"
        );

        // /a answers 200 now.
        let answer = Answer::Crawled(CrawledAnswer {
            status: 200,
            content_type: "HTML".to_string(),
            ..CrawledAnswer::default()
        });
        retries.record_answer("https://site.example/a", answer);
        retries.apply_to_state(&mut state);
        let revision = state.revision;
        index.sync(&state.urls, Some("site.example"), &retries, revision);
        assert_eq!(index.total(), 2, "the counts follow");
        assert_eq!(index.entries.len(), 2, "neither broken nor queued: out of the index");
        let urls = |scope| scope_urls(&state.urls, &index, &retries, revision, scope);
        assert_eq!(
            urls(Scope::AllBroken),
            ["https://site.example/b", "https://site.example/c"]
        );
        assert_eq!(urls(Scope::InternalPages).len(), 2);
        assert_eq!(urls(Scope::RetryQueue), ["https://site.example/c"]);
    }

    #[test]
    fn the_selection_extends_with_shift_and_never_keeps_a_hidden_row() {
        use selection::Click;
        let rows: Vec<UrlRow> = (0..6)
            .map(|n| row(&format!("https://site.example/{n}"), 404, ContentKind::Html))
            .collect();
        let texts = LinkTexts::default();
        let retries = RetryState::default();
        let mut index = BrokenIndex::default();
        index.sync(&rows, Some("site.example"), &retries, 0);
        let context = Context {
            rows: &rows,
            entries: &index.entries,
            found_on: None,
            links: &[],
            texts: &texts,
            retries: &retries,
            revision: 0,
            queue_order: false,
        };
        let mut filter = BrokenFilter::default();
        let mut view = BrokenView::default();
        view.sync(&context, &filter, BrokenSort::Url, true);
        let shown = |view: &BrokenView| -> Vec<Option<String>> {
            view.rows()
                .iter()
                .map(|row| context.item(*row).map(|item| item.url().to_string()))
                .collect()
        };
        let mut selection = Selection::default();
        selection.follow(&filter, &view, &context);
        selection.apply(Click::Row(1, false), &shown(&view));
        selection.apply(Click::Row(4, true), &shown(&view));
        assert_eq!(
            selection.in_view_order(&view, &context),
            (1..=4).map(|n| format!("https://site.example/{n}")).collect::<Vec<_>>()
        );
        // Shift on a selected row clears the range from the last click.
        selection.apply(Click::Row(2, true), &shown(&view));
        assert_eq!(selection.in_view_order(&view, &context), ["https://site.example/1"]);
        selection.apply(Click::All(true), &shown(&view));
        assert_eq!(selection.len(), 6);
        selection.follow(&filter, &view, &context);
        assert_eq!(selection.len(), 6, "an unchanged view keeps the selection");
        // A filter that hides rows clears the selection.
        filter.text = "/3".to_string();
        view.sync(&context, &filter, BrokenSort::Url, true);
        selection.follow(&filter, &view, &context);
        assert!(selection.is_empty());
        selection.apply(Click::All(true), &shown(&view));
        assert_eq!(selection.len(), 1);
        selection.apply(Click::All(false), &shown(&view));
        assert!(selection.is_empty());
        // Rows as drawn by page: a heading has no URL, a URL may be drawn twice; Shift ranges run
        // over the drawn rows and select each URL once.
        let drawn = vec![
            None,
            Some("https://site.example/0".to_string()),
            Some("https://site.example/1".to_string()),
            None,
            Some("https://site.example/1".to_string()),
            Some("https://site.example/2".to_string()),
        ];
        selection.apply(Click::Row(1, false), &drawn);
        selection.apply(Click::Row(5, true), &drawn);
        assert_eq!(selection.len(), 3);
        selection.apply(Click::Row(4, false), &drawn);
        assert!(
            !selection.contains("https://site.example/1"),
            "unticked under both pages"
        );
        selection.apply(Click::All(true), &drawn);
        assert_eq!(selection.len(), 3);
    }

    #[test]
    fn by_page_lists_each_link_under_every_page_it_was_found_on() {
        let rows = vec![
            row("https://site.example/gone", 404, ContentKind::Html),
            row("https://site.example/img/missing.png", 404, ContentKind::Image),
            row("https://site.example/slow", -2, ContentKind::Html),
        ];
        let texts = LinkTexts::default();
        let retries = RetryState::default();
        let mut index = BrokenIndex::default();
        index.sync(&rows, Some("site.example"), &retries, 0);
        let found_on = FoundOn(HashMap::from([
            (
                "https://site.example/gone".to_string(),
                "https://site.example/b".to_string(),
            ),
            (
                "https://site.example/img/missing.png".to_string(),
                "https://site.example/a".to_string(),
            ),
        ]));
        let mut shared = link("https://dead.example/", Outcome::Broken, None);
        shared.sources = vec![
            "https://site.example/a".to_string(),
            "https://site.example/b/".to_string(),
        ];
        let links = vec![shared, link("https://ok.example/", Outcome::Ok, Some(200))];
        let context = Context {
            rows: &rows,
            entries: &index.entries,
            found_on: Some(&found_on),
            links: &links,
            texts: &texts,
            retries: &retries,
            revision: 0,
            queue_order: false,
        };
        let mut filter = BrokenFilter::default();
        let mut view = BrokenView::default();
        view.sync(&context, &filter, BrokenSort::Where, true);
        let mut pages = PageView::default();
        pages.sync(&view, &context, &filter);
        let drawn = |pages: &PageView| -> Vec<String> {
            pages
                .rows()
                .iter()
                .map(|row| match row {
                    PageRow::Page(at) => format!("# {}", pages.page(*at).unwrap_or("?")),
                    PageRow::Link(row, _) => context
                        .item(*row)
                        .map(|item| item.url().to_string())
                        .unwrap_or_default(),
                })
                .collect()
        };
        // Pages with more broken links first, then by URL; the link without a page last; the
        // working external link is in no broken view.
        assert_eq!(
            drawn(&pages),
            [
                "# https://site.example/a",
                "https://site.example/img/missing.png",
                "https://dead.example/",
                "# https://site.example/b",
                "https://site.example/gone",
                "https://dead.example/",
                "# ?",
                "https://site.example/slow",
            ]
        );
        assert_eq!(pages.pages(), 2);
        assert_eq!(pages.groups[0], (Some("https://site.example/a".to_string()), 2));
        // Unchanged rows: nothing is rebuilt.
        let built = pages.built_for.clone();
        pages.sync(&view, &context, &filter);
        assert_eq!(pages.built_for, built);
        // A filter naming a page lists a link under that page only.
        filter.text = "site.example/b".to_string();
        view.sync(&context, &filter, BrokenSort::Where, true);
        pages.sync(&view, &context, &filter);
        assert_eq!(
            drawn(&pages),
            [
                "# https://site.example/b",
                "https://site.example/gone",
                "https://dead.example/",
            ]
        );
        // Copy list: one line per link and page.
        let groups = vec![(
            Some("https://site.example/b".to_string()),
            vec![ShownRow {
                status: "404".to_string(),
                code: 404,
                place: Where::InternalPage,
                url: "https://site.example/gone".to_string(),
                title: None,
                final_url: None,
                link_text: "Gone".to_string(),
                sources: vec!["https://site.example/b/".to_string()],
                positions: vec![":1:20345".to_string()],
                checked: false,
            }],
        )];
        assert_eq!(
            copy_text_by_page(&groups),
            "Found on\tStatus\tWhere\tURL\tLink text\nhttps://site.example/b:1:20345\t404\tInternal page\thttps://site.example/gone\tGone\n"
        );
    }
}
