//! The Link checker tab: every crawled URL that answered 4xx or 5xx or failed without an answer,
//! split into internal pages and internal assets, and the external links as the app's check after
//! the crawl found them (GUI-SPEC §4.7, §6.3), and the links whose scheme is a typo of `http` or
//! `https` (`htp://…`), which the crawler lists in its result and never requests: status "Mistyped
//! scheme", never retried. *Found on* is the page or stylesheet that referenced
//! the URL: the `url` event's `source` as the URL arrives (every status), then — with
//! `--link-details` — every referring page of the result JSON's `links` table; for runs of a
//! crawler before link data, the result JSON's `404` table (404s only).
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
use crate::model::link_data::{CrawlerRef, LinkGraph, Referrer, TargetRefs, text_on};
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

/// The status of a link whose scheme is a typo of `http`/`https` (`htp://…`), never requested.
pub const MISTYPED_SCHEME: &str = "Mistyped scheme";

/// What [`MISTYPED_SCHEME`] means, for its tooltip.
const MISTYPED_SCHEME_TIP: &str =
    "The link's scheme is misspelled (not http or https), so a browser cannot open it. The crawler did not request it.";

/// The host of a URL for "same site": lower-case, without a leading `www.`.
pub fn site_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    Some(host.strip_prefix("www.").map(str::to_string).unwrap_or(host))
}

/// What a broken URL points to, read from how it is referenced, else from the extension of its
/// path (the reported content type is the error page's, `HTML`, for a missing image).
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

/// The kind of a URL from the `sourceAttr` of its known references (in their stored order), else
/// from its extension ([`link_kind`]). The first reference that embeds the URL decides:
/// `<img src>`, `<img srcset>` and `<input src>` an image, `<script src>` a script, `<video src>`,
/// `<audio src>` and `<source src>` the extension's kind but never a page (other), `css url()` a
/// font or an `@import`ed stylesheet by the extension, else an image, `<link href>` the
/// extension's kind unless that is a page. A link (`<a href>`), a script's URL, a redirect, a
/// sitemap, a URL list or an unknown attribute leave it to the extension. Also returns the
/// attribute that decided (`None` when the extension did).
pub fn kind_of<'a>(url: &str, attrs: impl IntoIterator<Item = &'a str>) -> (LinkKind, Option<&'a str>) {
    let by_extension = link_kind(url);
    for attr in attrs {
        let kind = match attr {
            "<img src>" | "<img srcset>" | "<input src>" => LinkKind::Image,
            "<script src>" => LinkKind::Script,
            "<video src>" | "<audio src>" | "<source src>" if by_extension == LinkKind::Page => LinkKind::Other,
            "<video src>" | "<audio src>" | "<source src>" => by_extension,
            "css url()" if matches!(by_extension, LinkKind::Font | LinkKind::Stylesheet) => by_extension,
            "css url()" => LinkKind::Image,
            "<link href>" if by_extension != LinkKind::Page => by_extension,
            _ => continue,
        };
        return (kind, Some(attr));
    }
    (by_extension, None)
}

/// The `sourceAttr` of every known reference to a URL: its `url` event's first, then every kept
/// referrer of the crawler's `links` table.
pub fn reference_attrs<'a>(
    link: Option<&'a CrawlerRef>,
    refs: Option<&'a TargetRefs>,
) -> impl Iterator<Item = &'a str> {
    link.into_iter()
        .chain(refs.into_iter().flat_map(|refs| &refs.refs))
        .map(|reference| &*reference.attr)
}

/// The kind with the reference that decided it ("Image, referenced by <img src>"); `None` when the
/// extension decided.
pub fn referenced_by(kind: LinkKind, attr: Option<&str>) -> Option<String> {
    attr.map(|attr| format!("{}, referenced by {attr}", kind.label()))
}

/// Where a broken URL lives and what it points to ([`kind_of`], with the attribute that decided):
/// External when its host differs from the start URL's `site` host (`www.` aside); otherwise an
/// internal page when it is a page, else an internal asset.
pub fn classify<'a>(
    url: &str,
    attrs: impl IntoIterator<Item = &'a str>,
    site: Option<&str>,
) -> (Where, LinkKind, Option<&'a str>) {
    let (kind, attr) = kind_of(url, attrs);
    let external = matches!((site, site_host(url).as_deref()), (Some(site), Some(host)) if site != host);
    let place = if external {
        Where::External
    } else if kind == LinkKind::Page {
        Where::InternalPage
    } else {
        Where::InternalAsset
    };
    (place, kind, attr)
}

/// The broken URL rows of a run (and the queued ones), collected as rows arrive.
#[derive(Debug, Default)]
pub struct BrokenIndex {
    /// `(URL row, where, kind)` of every broken or queued row, in arrival order.
    pub entries: Vec<(u32, Where, LinkKind)>,
    /// `(where, kind)` of each link with a mistyped scheme, in the order of the run's list.
    pub mistyped: Vec<(Where, LinkKind)>,
    /// Counts of the broken rows (the links with a mistyped scheme included) by [`Where`].
    pub counts: [u64; 3],
    /// A row names the page it was found on: the crawler reports referring pages.
    pub sources_reported: bool,
    scanned: usize,
    site: Option<String>,
    /// Built with the crawler's `links` table (it arrives with the result: every row is then
    /// classified again by all its references).
    graph: bool,
    /// The revisions of the rows and of the retries the index was built for.
    revisions: (u64, u64),
}

impl BrokenIndex {
    /// Brings the index up to date with `rows` and the links with a mistyped scheme (they arrive
    /// with the result); `revision` is the rows' (a retry changes rows in place).
    pub fn sync(
        &mut self,
        rows: &[UrlRow],
        site: Option<&str>,
        graph: Option<&LinkGraph>,
        mistyped: &[(String, CrawlerRef)],
        retries: &RetryState,
        revision: u64,
    ) {
        let revisions = (revision, retries.revision);
        if self.site.as_deref() != site
            || self.graph != graph.is_some()
            || rows.len() < self.scanned
            || self.revisions != revisions
            || self.mistyped.len() != mistyped.len()
        {
            *self = Self {
                site: site.map(str::to_string),
                graph: graph.is_some(),
                revisions,
                ..Self::default()
            };
        }
        for (index, row) in rows.iter().enumerate().skip(self.scanned) {
            self.sources_reported |= row.link.is_some();
            let broken = is_broken(row.status);
            if (broken || retries.is_queued(&row.url))
                && let Ok(index) = u32::try_from(index)
            {
                let refs = graph.and_then(|graph| graph.target(&row.url));
                let (place, kind, _) = classify(&row.url, reference_attrs(row.link.as_deref(), refs), site);
                if broken && let Some(count) = self.counts.get_mut(place.slot()) {
                    *count += 1;
                }
                self.entries.push((index, place, kind));
            }
        }
        self.scanned = rows.len();
        if self.mistyped.is_empty() {
            for (url, reference) in mistyped {
                let refs = graph.and_then(|graph| graph.target(url));
                let (place, kind, _) = classify(url, reference_attrs(Some(reference), refs), site);
                if let Some(count) = self.counts.get_mut(place.slot()) {
                    *count += 1;
                }
                self.mistyped.push((place, kind));
            }
        }
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

/// A row of the tab: a broken crawled URL (position in [`BrokenIndex::entries`]), a checked
/// external link (position in the check's results) or a link with a mistyped scheme (position in
/// the run's list of them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowRef {
    Crawled(u32),
    Link(u32),
    Mistyped(u32),
}

impl RowRef {
    /// Whether the row can be retried: a link with a mistyped scheme cannot be requested.
    pub fn retryable(self) -> bool {
        !matches!(self, RowRef::Mistyped(_))
    }
}

/// What a row shows.
enum Item<'a> {
    Crawled {
        row: &'a UrlRow,
        place: Where,
        kind: LinkKind,
        /// The page of the result JSON's `404` table (crawlers before link data).
        found_on: Option<&'a str>,
        /// Every kept referrer from the crawler's `links` table (`--link-details`).
        refs: Option<&'a TargetRefs>,
    },
    /// A checked external link, with its referrers from the crawler's `links` table.
    Link(&'a LinkResult, Option<&'a TargetRefs>),
    /// A link whose scheme is a typo of `http`/`https`, never requested: the URL as written, the
    /// page and attribute of the crawler's `skipped` table, and every kept referrer from its
    /// `links` table (`--link-details`).
    Mistyped {
        url: &'a str,
        reference: &'a CrawlerRef,
        place: Where,
        kind: LinkKind,
        refs: Option<&'a TargetRefs>,
    },
}

impl<'a> Item<'a> {
    fn url(&self) -> &'a str {
        match self {
            Item::Crawled { row, .. } => &row.url,
            Item::Link(link, _) => &link.url,
            Item::Mistyped { url, .. } => url,
        }
    }

    fn place(&self) -> Where {
        match self {
            Item::Crawled { place, .. } | Item::Mistyped { place, .. } => *place,
            Item::Link(..) => Where::External,
        }
    }

    fn kind(&self) -> LinkKind {
        match self {
            Item::Crawled { kind, .. } | Item::Mistyped { kind, .. } => *kind,
            Item::Link(..) => self.kind_and_attr().0,
        }
    }

    /// The kind and the reference attribute that decided it (`None` when the extension did).
    fn kind_and_attr(&self) -> (LinkKind, Option<&'a str>) {
        match self {
            Item::Crawled { row, refs, .. } => kind_of(&row.url, reference_attrs(row.link.as_deref(), *refs)),
            Item::Link(link, refs) => kind_of(&link.url, reference_attrs(None, *refs)),
            Item::Mistyped {
                url, reference, refs, ..
            } => kind_of(url, reference_attrs(Some(*reference), *refs)),
        }
    }

    /// Broken first, then refused, then OK.
    fn outcome(&self) -> Outcome {
        match self {
            Item::Crawled { row, .. } if is_broken(row.status) => Outcome::Broken,
            Item::Crawled { .. } => Outcome::Ok,
            Item::Link(link, _) => link.outcome,
            Item::Mistyped { .. } => Outcome::Broken,
        }
    }

    /// The HTTP status, or a negative number without an answer.
    fn status(&self) -> i64 {
        match self {
            Item::Crawled { row, .. } => i64::from(row.status),
            Item::Link(link, _) => link.status.map_or(-1, i64::from),
            Item::Mistyped { .. } => -1,
        }
    }

    fn class(&self) -> StatusClass {
        StatusClass::of(self.status())
    }

    fn status_text(&self) -> String {
        match self {
            Item::Crawled { row, .. } => status_text(row.status),
            Item::Link(link, _) => link.status_label(),
            Item::Mistyped { .. } => MISTYPED_SCHEME.to_string(),
        }
    }

    /// What the status chip's tooltip explains: why a link got no answer.
    fn status_tip(&self) -> Option<&'a str> {
        match self {
            Item::Link(link, _) => link.error.as_deref(),
            Item::Mistyped { .. } => Some(MISTYPED_SCHEME_TIP),
            Item::Crawled { .. } => None,
        }
    }

    fn tone(&self) -> Tone {
        match self {
            Item::Crawled { row, .. } => super::urls::status_tone(row.status),
            Item::Link(link, _) => match (link.outcome, link.status) {
                (Outcome::Refused, _) => Tone::Warning,
                (_, Some(status)) => super::urls::status_tone(i32::from(status)),
                (_, None) => Tone::Danger,
            },
            Item::Mistyped { .. } => Tone::Danger,
        }
    }

    fn title(&self) -> &str {
        match self {
            Item::Link(link, _) => link.title.as_deref().unwrap_or(""),
            Item::Crawled { .. } | Item::Mistyped { .. } => "",
        }
    }

    /// Every page it was found on that is known, with what the crawler reported about each
    /// reference: for a crawled URL every kept referrer of the `links` table, else the page of its
    /// `url` event, else the page of the `404` table; for an external link the check's pages.
    fn referrers(&self) -> Vec<Referrer<'a>> {
        match self {
            Item::Crawled { refs: Some(refs), .. } | Item::Mistyped { refs: Some(refs), .. } => {
                refs.refs.iter().map(Referrer::of).collect()
            }
            Item::Mistyped { reference, .. } => vec![Referrer::of(reference)],
            Item::Crawled { row, found_on, .. } => match row.link.as_deref() {
                Some(link) => vec![Referrer::of(link)],
                None => found_on.iter().map(|page| Referrer::page(page)).collect(),
            },
            Item::Link(link, refs) => link
                .sources
                .iter()
                .map(|page| Referrer {
                    page,
                    crawler: refs.and_then(|refs| refs.on(page)),
                })
                .collect(),
        }
    }

    /// The first page it was found on.
    fn found_on(&self) -> &'a str {
        match self {
            Item::Crawled { refs: Some(refs), .. } | Item::Mistyped { refs: Some(refs), .. } => {
                refs.refs.first().map_or("", |r| &*r.source)
            }
            Item::Mistyped { reference, .. } => &reference.source,
            Item::Crawled { row, found_on, .. } => row
                .link
                .as_deref()
                .map(|link| &*link.source)
                .or(*found_on)
                .unwrap_or(""),
            Item::Link(link, _) => link.sources.first().map_or("", String::as_str),
        }
    }

    /// Every page it was found on that is known.
    fn sources(&self) -> Vec<&'a str> {
        self.referrers().iter().map(|referrer| referrer.page).collect()
    }

    /// How many pages reference it in all: also those beyond the known ones (the crawler keeps 50
    /// per URL).
    fn total(&self) -> usize {
        let known = self.sources().len();
        match self {
            Item::Crawled { refs: Some(refs), .. }
            | Item::Mistyped { refs: Some(refs), .. }
            | Item::Link(_, Some(refs)) => (refs.in_links as usize).max(known),
            Item::Crawled { .. } | Item::Mistyped { .. } | Item::Link(_, None) => known,
        }
    }

    /// The link text on the page(s) it was found on.
    fn link_text<'t>(&self, texts: &'t LinkTexts) -> TextLookup<'t>
    where
        'a: 't,
    {
        text_on(texts, self.url(), &self.referrers())
    }

    /// The referrer of `page` (a trailing `/` aside); the page alone when it is not one of them.
    fn referrer_on<'p>(&self, page: &'p str) -> Referrer<'p>
    where
        'a: 'p,
    {
        let key = page.trim().trim_end_matches('/');
        self.referrers()
            .into_iter()
            .find(|referrer| referrer.page.trim().trim_end_matches('/') == key)
            .unwrap_or(Referrer::page(page))
    }

    fn in_scope(&self, scope: Scope, retries: &RetryState) -> bool {
        let flagged = self.outcome() != Outcome::Ok;
        let crawled_in = |wanted: Where| matches!(self, Item::Crawled { place, .. } | Item::Mistyped { place, .. } if *place == wanted);
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
    built_for: Option<(BrokenFilter, BrokenSort, bool, u8, usize)>,
    /// The revisions of the rows and the retries the rows were built for.
    built_revisions: (u64, u64),
    pub rebuilds: u64,
}

struct Context<'a> {
    rows: &'a [UrlRow],
    entries: &'a [(u32, Where, LinkKind)],
    found_on: Option<&'a FoundOn>,
    /// The crawler's `links` table (`--link-details`), once the result is loaded.
    graph: Option<&'a LinkGraph>,
    links: &'a [LinkResult],
    /// The links with a mistyped scheme, and their place and kind ([`BrokenIndex::mistyped`]).
    mistyped: &'a [(String, CrawlerRef)],
    mistyped_places: &'a [(Where, LinkKind)],
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
                    refs: self.graph.and_then(|graph| graph.target(&row.url)),
                })
            }
            RowRef::Link(position) => self
                .links
                .get(position as usize)
                .map(|link| Item::Link(link, self.graph.and_then(|graph| graph.target(&link.url)))),
            RowRef::Mistyped(position) => {
                let (url, reference) = self.mistyped.get(position as usize)?;
                let (place, kind) = self.mistyped_places.get(position as usize)?;
                Some(Item::Mistyped {
                    url,
                    reference,
                    place: *place,
                    kind: *kind,
                    refs: self.graph.and_then(|graph| graph.target(url)),
                })
            }
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
        RowRef::Mistyped(position) => (2, position),
    }
}

impl BrokenView {
    pub fn rows(&self) -> &[RowRef] {
        &self.rows
    }

    /// How many rows can be retried ([`RowRef::retryable`]).
    pub fn retryable(&self) -> usize {
        self.rows.iter().filter(|row| row.retryable()).count()
    }

    fn sync(&mut self, context: &Context<'_>, filter: &BrokenFilter, sort: BrokenSort, ascending: bool) {
        // How many of the result's joins have arrived (the `404` table, the crawler's `links`
        // table, which may classify rows again, and the links with a mistyped scheme; each arrives
        // once, maybe in different frames).
        let joined = u8::from(context.found_on.is_some())
            + u8::from(context.graph.is_some())
            + u8::from(!context.mistyped_places.is_empty());
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
        let matching = |range: std::ops::Range<usize>, row: fn(u32) -> RowRef| {
            range
                .filter_map(|position| u32::try_from(position).ok())
                .map(row)
                .filter(|row| context.matches(*row, filter, &needle))
                .collect::<Vec<_>>()
        };
        let crawled = |range| matching(range, RowRef::Crawled);
        let checked = |range| matching(range, RowRef::Link);
        if !current || total < self.scanned || links < self.scanned_links {
            self.rebuilds += 1;
            self.rows = crawled(0..total);
            self.rows.extend(checked(0..links));
            // They arrive with the result, all at once: only a rebuild adds them.
            self.rows
                .extend(matching(0..context.mistyped_places.len(), RowRef::Mistyped));
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
        graph: view.links.graph,
        links: view.links.results,
        mistyped: view.links.mistyped,
        mistyped_places: &index.mistyped,
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
    /// Every page it was found on that is known (empty when unknown).
    pub sources: Vec<String>,
    /// For each of `sources`, where it references the URL (`:1929`, `:1:20345`), or empty.
    pub positions: Vec<String>,
    /// Pages that reference it beyond `sources` (the crawler keeps 50 per URL).
    pub more: usize,
    /// A checked external link (not a crawled URL).
    pub checked: bool,
}

impl ShownRow {
    /// The first page it was found on.
    pub fn found_on(&self) -> Option<&str> {
        self.sources.first().map(String::as_str)
    }

    /// Every page it was found on, each with where it references the URL (`…/style.css:1929`),
    /// then "and N more pages" for those beyond the known ones.
    pub fn found_on_with_positions(&self) -> Vec<String> {
        let mut pages: Vec<String> = self
            .sources
            .iter()
            .enumerate()
            .map(|(at, source)| format!("{source}{}", self.positions.get(at).map_or("", String::as_str)))
            .collect();
        if self.more > 0 {
            pages.push(more_pages(self.more));
        }
        pages
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

/// What the shown rows are read from: the run's URL rows, its checked links, the link texts the app
/// read, the crawler's `links` table and the retries.
#[derive(Clone, Copy)]
pub struct ShownFrom<'a> {
    pub urls: &'a [UrlRow],
    pub links: &'a [LinkResult],
    pub texts: &'a LinkTexts,
    pub graph: Option<&'a LinkGraph>,
    /// The links with a mistyped scheme.
    pub mistyped: &'a [(String, CrawlerRef)],
    pub retries: &'a RetryState,
}

/// "and 312 more pages".
pub fn more_pages(more: usize) -> String {
    let noun = if more == 1 { "page" } else { "pages" };
    format!("and {} more {noun}", format::count(more as u64))
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
        self.index.sync(
            &view.state.urls,
            site.as_deref(),
            view.links.graph,
            view.links.mistyped,
            retries,
            view.state.revision,
        );
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
            graph: view.links.graph,
            links,
            mistyped: view.links.mistyped,
            mistyped_places: &self.index.mistyped,
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

    /// The URL of each row as drawn (`None` for a page heading and a row that cannot be retried),
    /// for a click on a checkbox.
    fn shown_urls(&self, context: &Context<'_>) -> Vec<Option<String>> {
        let url = |row: RowRef| {
            context
                .item(row)
                .filter(|_| row.retryable())
                .map(|item| item.url().to_string())
        };
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
    pub fn shown(&self, rows: ShownFrom<'_>) -> Vec<ShownRow> {
        let texts = rows.texts;
        let context = self.context_from(&rows);
        self.view
            .rows()
            .iter()
            .filter_map(|row| context.item(*row))
            .map(|item| shown_row(&item, item.link_text(texts), texts))
            .collect()
    }

    fn context_from<'a>(&'a self, rows: &ShownFrom<'a>) -> Context<'a> {
        Context {
            rows: rows.urls,
            entries: &self.index.entries,
            found_on: self.found_on.as_ref(),
            graph: rows.graph,
            links: rows.links,
            mistyped: rows.mistyped,
            mistyped_places: &self.index.mistyped,
            texts: rows.texts,
            retries: rows.retries,
            revision: 0,
            queue_order: false,
        }
    }

    /// The shown rows of the *by page* layout: `(page, its rows)`, `None` for the links whose page
    /// is not known; the link text is the one on that page (after [`Self::sync`]).
    pub fn shown_by_page(&self, rows: ShownFrom<'_>) -> Vec<(Option<String>, Vec<ShownRow>)> {
        let texts = rows.texts;
        let context = self.context_from(&rows);
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
    let referrers = item.referrers();
    ShownRow {
        status: item.status_text(),
        code: item.status(),
        place: item.place(),
        url: item.url().to_string(),
        title: Some(item.title().to_string()).filter(|title| !title.is_empty()),
        final_url: match item {
            Item::Link(link, _) => link.final_url.clone(),
            Item::Crawled { .. } | Item::Mistyped { .. } => None,
        },
        link_text: link_text.text().to_string(),
        positions: referrers
            .iter()
            .map(|referrer| {
                referrer
                    .position(texts, item.url())
                    .map(|position| position.suffix())
                    .unwrap_or_default()
            })
            .collect(),
        sources: referrers.iter().map(|referrer| referrer.page.to_string()).collect(),
        more: item.total().saturating_sub(referrers.len()),
        checked: matches!(item, Item::Link(..)),
    }
}

/// The link text of `item` on `page` (on any page it was found on when the page is not known).
fn page_text<'t>(texts: &'t LinkTexts, item: &Item<'t>, page: Option<&'t str>) -> TextLookup<'t> {
    let Some(page) = page else {
        return item.link_text(texts);
    };
    text_on(texts, item.url(), &[item.referrer_on(page)])
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
            let from = ShownFrom {
                urls: &view.state.urls,
                links: view.links.results,
                texts: view.links.texts,
                graph: view.links.graph,
                mistyped: view.links.mistyped,
                retries: view.retry.state,
            };
            let (text, count) = if by_page {
                let groups = broken.shown_by_page(from);
                (copy_text_by_page(&groups), broken.view.rows().len())
            } else {
                let rows = broken.shown(from);
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
    let unknown = UnknownPage::of(view, result_ready, broken.index.sources_reported);
    let all_external = broken.filter.scope == Scope::AllExternal;
    let scope = broken.filter.scope;
    // Rows are selected for a retry once the crawl has ended.
    let selectable = !view.live;
    let mut click = None;
    let context = broken.context(view);
    let rows = broken.view.rows();
    let retryable = broken.view.retryable();
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
                // Found on gets the largest share: the page and where it references the link.
                builder = builder
                    .column(Column::exact(second))
                    .column(Column::exact(110.0))
                    .column(text_column(share(fixed, 0.31)))
                    .column(text_column(share(fixed, 0.22)))
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
                            if let Some(clicked) = selection::header_checkbox(ui, selection, retryable) {
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
                        let Some(&row_ref) = rows.get(position) else {
                            return;
                        };
                        let Some(item) = context.item(row_ref) else {
                            return;
                        };
                        if selectable {
                            row.col(|ui| {
                                if row_ref.retryable()
                                    && let Some(clicked) = selection::row_checkbox(ui, selection, position, item.url())
                                {
                                    click = Some(clicked);
                                }
                            });
                        }
                        row.col(|ui| {
                            let chip = widgets::chip(ui, item.status_text(), item.tone());
                            if let Some(tip) = item.status_tip() {
                                chip.on_hover_text(tip);
                            }
                            retry::mark(ui, &view.retry, item.url());
                        });
                        if all_external {
                            row.col(|ui| title_cell(ui, &item));
                        } else {
                            row.col(|ui| match scope {
                                Scope::RetryQueue => retry::state_cell(ui, &view.retry, item.url()),
                                _ => kind_cell(ui, &item),
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
                        row.col(|ui| found_on_cell(ui, view, &item, unknown));
                        let response = row.response();
                        if response.double_clicked() {
                            actions.push(RunAction::OpenUrl(item.url().to_string()));
                        }
                        response.context_menu(|ui| {
                            if selectable && row_ref.retryable() {
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
    let retryable = broken.view.retryable();
    let selection = &broken.selection;
    let selected = || selection.in_view_order(&broken.view, &context);
    let unknown = UnknownPage::of(view, result_ready, broken.index.sources_reported);
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
                            if let Some(clicked) = selection::header_checkbox(ui, selection, retryable) {
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
                                row.col(|ui| page_heading(ui, across, page, links, unknown.heading));
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
                                        response.on_hover_text(unknown.tip);
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
                                        if link_row.retryable()
                                            && let Some(clicked) =
                                                selection::row_checkbox(ui, selection, position, item.url())
                                        {
                                            click = Some(clicked);
                                        }
                                    });
                                }
                                row.col(|ui| {
                                    let chip = widgets::chip(ui, item.status_text(), item.tone());
                                    if let Some(tip) = item.status_tip() {
                                        chip.on_hover_text(tip);
                                    }
                                    retry::mark(ui, &view.retry, item.url());
                                });
                                row.col(|ui| kind_cell(ui, &item));
                                row.col(|ui| {
                                    let place = item.place();
                                    let color = if place == Where::External { p.info } else { p.text };
                                    ui.label(RichText::new(place.label()).color(color));
                                });
                                row.col(|ui| url_cell(ui, &item));
                                row.col(|ui| {
                                    let position = page
                                        .and_then(|page| item.referrer_on(page).position(view.links.texts, item.url()));
                                    line_cell(ui, position.as_ref());
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
                                    if selectable && link_row.retryable() {
                                        let in_selection = if selection.contains(item.url()) {
                                            selection.len()
                                        } else {
                                            0
                                        };
                                        retry::menu_entries(
                                            ui,
                                            &view.retry,
                                            item.url(),
                                            &selected,
                                            in_selection,
                                            actions,
                                        );
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
        Item::Link(link, _) => match &link.title {
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
        Item::Mistyped { .. } => {
            ui.label(widgets::weak(ui, "—"));
        }
    }
}

/// The kind; how the URL is referenced in the tooltip when that decided it.
fn kind_cell(ui: &mut Ui, item: &Item<'_>) {
    let p = theme::palette(ui);
    let (kind, attr) = item.kind_and_attr();
    let response = ui.label(RichText::new(kind.label()).color(p.text));
    if let Some(tip) = referenced_by(kind, attr) {
        response.on_hover_text(tip);
    }
}

fn url_cell(ui: &mut Ui, item: &Item<'_>) {
    let p = theme::palette(ui);
    let mut tip = Vec::new();
    if let Item::Link(link, _) = item {
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

/// What the tab says where the page a link was found on is not known.
#[derive(Debug, Clone, Copy)]
struct UnknownPage {
    /// In the *Found on* cell.
    cell: &'static str,
    /// The heading of the last group by page.
    heading: &'static str,
    tip: &'static str,
}

impl UnknownPage {
    /// `sources_reported`: the crawler names the referring page of each URL as it arrives.
    fn of(view: &RunView<'_>, result_ready: bool, sources_reported: bool) -> Self {
        if sources_reported {
            Self {
                cell: "not reported by the crawler",
                heading: "Page not reported by the crawler",
                tip: "The crawler reports no referring page for a start URL (--url, --url-list).",
            }
        } else if view.live {
            Self {
                cell: "available when the crawl finishes",
                heading: "Pages known when the crawl finishes",
                tip: "This crawler reports the page a URL was found on in its result file, for 404 responses only.",
            }
        } else if !result_ready {
            Self {
                cell: "not available",
                heading: "Page not available",
                tip: "This run has no readable result file.",
            }
        } else {
            Self {
                cell: "not reported by the crawler",
                heading: "Page not reported by the crawler",
                tip: "The crawler of this run reports the page a URL was found on only for a 404, and only the first page that links to it.",
            }
        }
    }
}

fn found_on_cell(ui: &mut Ui, view: &RunView<'_>, item: &Item<'_>, unknown: UnknownPage) {
    let referrers = item.referrers();
    let Some(first) = referrers.first() else {
        ui.label(widgets::weak(ui, unknown.cell)).on_hover_text(unknown.tip);
        return;
    };
    let texts = view.links.texts;
    let position = first.position(texts, item.url());
    // With a position, the cell ends with `.css:1929`: the chip would only take its room.
    if is_stylesheet(first.page) && position.is_none() {
        widgets::chip(ui, "CSS", Tone::Accent).on_hover_text("Referenced from a stylesheet (e.g. a background image)");
    }
    let total = item.total();
    if total > 1 {
        // The chip first: the URL is cut at the column's edge. Every known page with where it
        // references the link; the ones beyond (the crawler keeps 50 per URL) counted.
        let mut all: Vec<String> = referrers
            .iter()
            .map(|referrer| match referrer.position(texts, item.url()) {
                Some(position) => format!("{}{}", referrer.page, position.suffix()),
                None => referrer.page.to_string(),
            })
            .collect();
        let more = total.saturating_sub(referrers.len());
        if more > 0 {
            all.push(format!("{} (the crawler lists 50 pages per URL)", more_pages(more)));
        }
        // In a narrow column the chip says only the number: the page needs the room.
        let others = format::count(total as u64 - 1);
        let label = if ui.available_width() < 260.0 {
            format!("+{others}")
        } else {
            format!("+{others} more")
        };
        widgets::chip(ui, label, Tone::Neutral).on_hover_text(all.join("\n"));
    }
    source_cell(ui, first.page, position.as_ref());
}

/// The width of the *Line* column of the by-page layout.
const LINE_WIDTH: f32 = 84.0;

/// Where a page references a link, in words, for a tooltip.
fn position_tip(position: &RefPosition) -> String {
    position.tip()
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
                "No position: the crawler gave none (a reference in a script or a sitemap, a page rendered in Chromium) and the page was not read, or it does not reference this URL now (it changed, or a script builds the link).",
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
    if let Item::Link(link, _) = item
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

    use std::sync::Arc;

    use crate::model::link_check::Failure;
    use crate::model::run_state::ContentKind;

    fn row(url: &str, status: i32, kind: ContentKind) -> UrlRow {
        UrlRow::new(url, status, kind, 1, 1, false)
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
            classify("http://127.0.0.1:8911/img/missing-background.jpg", [], site),
            (Where::InternalAsset, LinkKind::Image, None)
        );
        assert_eq!(
            classify("http://127.0.0.1:8911/missing-page.html", [], site),
            (Where::InternalPage, LinkKind::Page, None)
        );
        assert_eq!(
            classify("http://127.0.0.1:8911/practice_areas/agricultural-law/", [], site),
            (Where::InternalPage, LinkKind::Page, None)
        );
        assert_eq!(
            classify("https://example.com/missing-external-bg.png", [], site),
            (Where::External, LinkKind::Image, None)
        );
        assert_eq!(
            classify("http://dead-domain.invalid/page", [], site),
            (Where::External, LinkKind::Page, None)
        );
        // www. and the bare host are one site; the query and fragment do not count.
        let site = site_host("https://www.example.com/");
        assert_eq!(
            classify("https://example.com/css/site.css?v=3#x", [], site.as_deref()),
            (Where::InternalAsset, LinkKind::Stylesheet, None)
        );
        assert_eq!(link_kind("https://example.com/fonts/a.woff2"), LinkKind::Font);
        assert_eq!(link_kind("https://example.com/docs/terms.pdf"), LinkKind::Document);
        assert_eq!(link_kind("https://example.com/app.mjs"), LinkKind::Script);
        assert_eq!(link_kind("https://example.com/archive.zip"), LinkKind::Other);
        assert_eq!(link_kind("https://example.com/index.php?id=4"), LinkKind::Page);
    }

    #[test]
    fn the_reference_decides_the_kind_before_the_extension() {
        let kind = |url: &str, attr: &'static str| kind_of(url, [attr]);
        let bare = "https://example.com/media/logo";
        // Embedding references decide, whatever the extension says.
        for attr in ["<img src>", "<img srcset>", "<input src>"] {
            assert_eq!(kind(bare, attr), (LinkKind::Image, Some(attr)), "{attr}");
            assert_eq!(kind("https://example.com/x.php", attr), (LinkKind::Image, Some(attr)));
        }
        assert_eq!(kind(bare, "<script src>"), (LinkKind::Script, Some("<script src>")));
        // Media: the extension's kind, but never a page.
        for attr in ["<video src>", "<audio src>", "<source src>"] {
            assert_eq!(kind(bare, attr), (LinkKind::Other, Some(attr)), "{attr}");
            assert_eq!(kind("https://example.com/v.html", attr), (LinkKind::Other, Some(attr)));
            assert_eq!(kind("https://example.com/v.mp4", attr), (LinkKind::Other, Some(attr)));
            assert_eq!(kind("https://example.com/p.webp", attr), (LinkKind::Image, Some(attr)));
        }
        // A stylesheet's url(): a font, an @import, else an image.
        let css = "css url()";
        assert_eq!(kind("https://example.com/f.woff2", css), (LinkKind::Font, Some(css)));
        assert_eq!(
            kind("https://example.com/more.css", css),
            (LinkKind::Stylesheet, Some(css))
        );
        assert_eq!(kind(bare, css), (LinkKind::Image, Some(css)));
        assert_eq!(kind("https://example.com/x.pdf", css), (LinkKind::Image, Some(css)));
        // <link href>: the extension, unless it gives a page (then the extension rule as before).
        let link = "<link href>";
        assert_eq!(
            kind("https://example.com/s.css", link),
            (LinkKind::Stylesheet, Some(link))
        );
        assert_eq!(kind("https://example.com/i.ico", link), (LinkKind::Image, Some(link)));
        assert_eq!(kind(bare, link), (LinkKind::Page, None));
        assert_eq!(
            kind("https://example.com/feed.xml", link),
            (LinkKind::Other, Some(link))
        );
        // Links, scripts' URLs, redirects, sitemaps, URL lists and unknown attributes: the
        // extension.
        for attr in [
            "<a href>",
            "js url",
            "redirect",
            "sitemap",
            "url list",
            "<embed src>",
            "",
        ] {
            assert_eq!(kind(bare, attr), (LinkKind::Page, None), "{attr}");
            assert_eq!(kind("https://example.com/terms.pdf", attr), (LinkKind::Document, None));
        }
        assert_eq!(kind_of(bare, []), (LinkKind::Page, None));
        assert_eq!(
            referenced_by(LinkKind::Image, Some("<img src>")).as_deref(),
            Some("Image, referenced by <img src>")
        );
        assert_eq!(referenced_by(LinkKind::Page, None), None);
    }

    #[test]
    fn an_embedding_reference_wins_over_links_and_the_first_one_decides() {
        let bare = "https://example.com/banner";
        assert_eq!(
            kind_of(bare, ["<a href>", "redirect", "<img src>", "<script src>"]),
            (LinkKind::Image, Some("<img src>"))
        );
        // A <link href> that falls back to the extension does not embed: the next one decides.
        assert_eq!(
            kind_of(bare, ["<link href>", "<script src>"]),
            (LinkKind::Script, Some("<script src>"))
        );
        assert_eq!(
            kind_of("https://example.com/app.css", ["<a href>", "<link href>", "css url()"]),
            (LinkKind::Stylesheet, Some("<link href>"))
        );
        assert_eq!(kind_of(bare, ["<a href>", "sitemap"]), (LinkKind::Page, None));
    }

    #[test]
    fn link_checker_rows_take_their_kind_from_every_known_reference() {
        use crate::model::link_data::LinkRow;
        let reference = |source: &str, attr: &str| CrawlerRef {
            source: Arc::from(source),
            attr: Arc::from(attr),
            text: None,
            line: None,
            column: None,
        };
        let referenced = |url: &str, source: &str, attr: &str| {
            let mut row = row(url, 404, ContentKind::Html);
            row.link = Some(Box::new(reference(source, attr)));
            row
        };
        let rows = vec![
            // (a) An image without an extension, from the `url` event.
            referenced("https://site.example/media/logo", "https://site.example/", "<img src>"),
            // (b) Queued by a link; the `links` table has an <img src> on another page.
            referenced("https://site.example/banner", "https://site.example/a", "<a href>"),
            // (c) No link data: the extension, as before.
            row("https://site.example/gone", 404, ContentKind::Html),
            row("https://site.example/img/x.png", 404, ContentKind::Html),
        ];
        let links_row = |url: &str, source: &str, attr: &str| LinkRow {
            url: url.to_string(),
            source_url: source.to_string(),
            source_attr: attr.to_string(),
            ..LinkRow::default()
        };
        let graph = LinkGraph::build(
            [
                links_row("https://site.example/banner", "https://site.example/a", "<a href>"),
                links_row("https://site.example/banner", "https://site.example/b", "<img src>"),
            ],
            [],
        );
        let retries = RetryState::default();
        let texts = LinkTexts::default();
        let mut index = BrokenIndex::default();
        let shown = |index: &BrokenIndex, graph: Option<&LinkGraph>| -> Vec<(LinkKind, Option<String>)> {
            let context = Context {
                rows: &rows,
                entries: &index.entries,
                found_on: None,
                graph,
                links: &[],
                mistyped: &[],
                mistyped_places: &[],
                texts: &texts,
                retries: &retries,
                revision: 0,
                queue_order: false,
            };
            (0..index.entries.len() as u32)
                .filter_map(|position| context.item(RowRef::Crawled(position)))
                .map(|item| {
                    let (kind, attr) = item.kind_and_attr();
                    assert_eq!(kind, item.kind(), "the cell and the sort agree");
                    (kind, referenced_by(kind, attr))
                })
                .collect()
        };

        // Live: the event's reference only.
        index.sync(&rows, Some("site.example"), None, &[], &retries, 0);
        let places: Vec<(Where, LinkKind)> = index.entries.iter().map(|(_, place, kind)| (*place, *kind)).collect();
        assert_eq!(
            places,
            [
                (Where::InternalAsset, LinkKind::Image),
                (Where::InternalPage, LinkKind::Page),
                (Where::InternalPage, LinkKind::Page),
                (Where::InternalAsset, LinkKind::Image),
            ]
        );
        assert_eq!(index.counts, [2, 2, 0]);
        assert_eq!(
            shown(&index, None),
            [
                (LinkKind::Image, Some("Image, referenced by <img src>".to_string())),
                (LinkKind::Page, None),
                (LinkKind::Page, None),
                (LinkKind::Image, None),
            ]
        );

        // The `links` table arrives with the result: the rows already collected are classified
        // again, and the counts follow.
        index.sync(&rows, Some("site.example"), Some(&graph), &[], &retries, 0);
        assert_eq!(index.entries[1], (1, Where::InternalAsset, LinkKind::Image));
        assert_eq!(index.counts, [1, 3, 0]);
        assert_eq!(
            shown(&index, Some(&graph))[1],
            (LinkKind::Image, Some("Image, referenced by <img src>".to_string()))
        );
        assert_eq!(
            scope_urls(&rows, &index, &retries, 0, Scope::InternalPages),
            ["https://site.example/gone"]
        );
        assert_eq!(scope_urls(&rows, &index, &retries, 0, Scope::InternalAssets).len(), 3);
    }

    #[test]
    fn links_with_a_mistyped_scheme_are_broken_rows_that_cannot_be_retried() {
        use crate::model::link_data::LinkRow;
        use selection::Click;
        let rows = vec![
            row("https://site.example/", 200, ContentKind::Html),
            row("https://site.example/gone", 404, ContentKind::Html),
        ];
        let links = vec![link("https://dead.example/", Outcome::Broken, None)];
        let reference = |source: &str, attr: &str| CrawlerRef {
            source: Arc::from(source),
            attr: Arc::from(attr),
            text: None,
            line: None,
            column: None,
        };
        // As the crawler's `skipped` table gives them: one on the site's host, one on another.
        let mistyped = vec![
            (
                "htp://site.example/about".to_string(),
                reference("https://site.example/contact/", "<a href>"),
            ),
            (
                "htps://partner.example/logo.png".to_string(),
                reference("https://site.example/", "<img src>"),
            ),
        ];
        // The `links` table has every page of the first one; the second one is not in it.
        let referrer = |source: &str, text: &str, line: Option<u32>| LinkRow {
            url: "htp://site.example/about".to_string(),
            source_url: source.to_string(),
            source_attr: "<a href>".to_string(),
            link_text: text.to_string(),
            source_line: line,
            in_links: Some(3),
            ..LinkRow::default()
        };
        let graph = LinkGraph::build(
            [
                referrer("https://site.example/contact/", "About us", Some(12)),
                referrer("https://site.example/team/", "", None),
            ],
            [],
        );
        let texts = LinkTexts::default();
        let retries = RetryState::default();
        let mut broken = BrokenUi::default();
        broken
            .index
            .sync(&rows, Some("site.example"), Some(&graph), &mistyped, &retries, 0);
        assert_eq!(
            broken.index.mistyped,
            [
                (Where::InternalPage, LinkKind::Page),
                (Where::External, LinkKind::Image)
            ]
        );
        assert_eq!(broken.index.counts, [2, 0, 1], "the counts include them");
        let context = Context {
            rows: &rows,
            entries: &broken.index.entries,
            found_on: None,
            graph: Some(&graph),
            links: &links,
            mistyped: &mistyped,
            mistyped_places: &broken.index.mistyped,
            texts: &texts,
            retries: &retries,
            revision: 0,
            queue_order: false,
        };
        let view_of = |scope| {
            let mut view = BrokenView::default();
            let filter = BrokenFilter {
                scope,
                ..BrokenFilter::default()
            };
            view.sync(&context, &filter, BrokenSort::Where, true);
            view
        };
        let shown = |scope| -> Vec<ShownRow> {
            view_of(scope)
                .rows()
                .iter()
                .filter_map(|row| context.item(*row))
                .map(|item| shown_row(&item, item.link_text(&texts), &texts))
                .collect()
        };
        let all = shown(Scope::AllBroken);
        let summary: Vec<(&str, i64, Where, &str)> = all
            .iter()
            .map(|row| (row.status.as_str(), row.code, row.place, row.url.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                ("Mistyped scheme", -1, Where::InternalPage, "htp://site.example/about"),
                ("404", 404, Where::InternalPage, "https://site.example/gone"),
                (
                    "Mistyped scheme",
                    -1,
                    Where::External,
                    "htps://partner.example/logo.png"
                ),
                ("DNS lookup failed", -1, Where::External, "https://dead.example/"),
            ]
        );
        // The tab's badge and the view's count agree with the rows: crawled and mistyped, plus the
        // broken external link of the check.
        assert_eq!(broken.index.total() + 1, all.len() as u64);
        // Found on, link text, position and "+N more" from the `links` table; else the page of the
        // `skipped` table.
        let internal = &all[0];
        assert_eq!(
            internal.sources,
            ["https://site.example/contact/", "https://site.example/team/"]
        );
        assert_eq!(
            (
                internal.link_text.as_str(),
                internal.positions[0].as_str(),
                internal.more
            ),
            ("About us", ":12", 1)
        );
        assert_eq!(all[2].sources, ["https://site.example/"]);
        assert!(!all[2].checked);
        // Where: internal by kind, External with the external links.
        let urls = |scope| -> Vec<String> { shown(scope).into_iter().map(|row| row.url).collect() };
        assert_eq!(
            urls(Scope::InternalPages),
            ["htp://site.example/about", "https://site.example/gone"]
        );
        assert!(urls(Scope::InternalAssets).is_empty());
        assert_eq!(
            urls(Scope::ExternalBroken),
            ["htps://partner.example/logo.png", "https://dead.example/"]
        );
        // The kind by the existing rule, and the status explained in the chip's tooltip.
        let external = context.item(RowRef::Mistyped(1)).expect("row");
        assert_eq!(external.kind_and_attr(), (LinkKind::Image, Some("<img src>")));
        assert_eq!(external.status_tip(), Some(MISTYPED_SCHEME_TIP));
        // Copy list.
        assert!(
            copy_text(&all, false)
                .contains("Mistyped scheme\tExternal\thtps://partner.example/logo.png\t\thttps://site.example/\n")
        );
        // By page: under every page it was found on.
        let mut pages = PageView::default();
        let view = view_of(Scope::AllBroken);
        pages.sync(&view, &context, &BrokenFilter::default());
        let contact = pages
            .groups
            .iter()
            .position(|(page, _)| page.as_deref() == Some("https://site.example/contact/"))
            .expect("a group");
        assert!(
            pages
                .rows()
                .contains(&PageRow::Link(RowRef::Mistyped(0), contact as u32))
        );

        // Not retryable: no checkbox URL, so Select all and the header's count leave them out.
        broken.view = view;
        assert_eq!(broken.view.retryable(), 2);
        let drawn = broken.shown_urls(&context);
        assert_eq!(
            drawn,
            [
                None,
                Some("https://site.example/gone".to_string()),
                None,
                Some("https://dead.example/".to_string()),
            ]
        );
        broken.selection.apply(Click::All(true), &drawn);
        assert_eq!(broken.selection.len(), 2);
        assert!(!RowRef::Mistyped(0).retryable() && RowRef::Crawled(0).retryable() && RowRef::Link(0).retryable());

        // A result without them: the rows and counts of before.
        let mut index = BrokenIndex::default();
        index.sync(&rows, Some("site.example"), Some(&graph), &mistyped, &retries, 0);
        index.sync(&rows, Some("site.example"), Some(&graph), &[], &retries, 0);
        assert_eq!(
            (index.counts, index.mistyped.len(), index.entries.len()),
            ([1, 0, 0], 0, 1)
        );
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
        index.sync(&rows, Some("example.com"), None, &[], &RetryState::default(), 0);
        assert_eq!(index.entries, vec![(1, Where::InternalPage, LinkKind::Page)]);
        rows.push(row("https://ext.org/", -2, ContentKind::Other));
        index.sync(&rows, Some("example.com"), None, &[], &RetryState::default(), 0);
        assert_eq!(index.counts, [1, 0, 1]);
    }

    #[test]
    fn checked_links_join_the_views_broken_first() {
        let no_retries = RetryState::default();
        let rows = vec![row("https://site.example/gone", 404, ContentKind::Html)];
        let texts = LinkTexts::default();
        let mut index = BrokenIndex::default();
        index.sync(&rows, Some("site.example"), None, &[], &RetryState::default(), 0);
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
                graph: None,
                links,
                mistyped: &[],
                mistyped_places: &[],
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
                graph: None,
                links: &links,
                mistyped: &[],
                mistyped_places: &[],
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
            graph: None,
            links: &links,
            mistyped: &[],
            mistyped_places: &[],
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
            graph: None,
            links: &fresh,
            mistyped: &[],
            mistyped_places: &[],
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
            more: 312,
            checked: true,
        }];
        // Each page with where it references the link, when it is known; the pages beyond the
        // crawler's 50 counted.
        assert_eq!(
            copy_text(&rows, false),
            "Status\tWhere\tURL\tLink text\tFound on\nRefused 403\tExternal\thttps://refused.example/\tOur partner\thttps://site.example/a:212 | https://site.example/b | and 312 more pages\n"
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
            graph: None,
            links: &[],
            mistyped: &[],
            mistyped_places: &[],
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
        index.sync(&state.urls, Some("site.example"), None, &[], &retries, state.revision);
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
        index.sync(&state.urls, Some("site.example"), None, &[], &retries, revision);
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
        index.sync(&rows, Some("site.example"), None, &[], &retries, 0);
        let context = Context {
            rows: &rows,
            entries: &index.entries,
            found_on: None,
            graph: None,
            links: &[],
            mistyped: &[],
            mistyped_places: &[],
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
        index.sync(&rows, Some("site.example"), None, &[], &retries, 0);
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
            graph: None,
            links: &links,
            mistyped: &[],
            mistyped_places: &[],
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
                more: 0,
                checked: false,
            }],
        )];
        assert_eq!(
            copy_text_by_page(&groups),
            "Found on\tStatus\tWhere\tURL\tLink text\nhttps://site.example/b:1:20345\t404\tInternal page\thttps://site.example/gone\tGone\n"
        );
    }
}
