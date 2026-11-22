//! A typed view of the result JSON (`--output-json-file`, `docs/JSON-OUTPUT.md`): quality scores,
//! stats, summary findings, analysis tables and the run's `options` — and, with `--link-details`,
//! the `links` table and the link facts of the per-URL `results` rows, read into a compact
//! [`LinkGraph`]. Fields the GUI does not use (the rest of each `results` row, …) are skipped while
//! reading, so memory stays small for large runs.
//!
//! Table columns keep the order of the file. The crawler may not depend on `serde_json`'s
//! `preserve_order` (GUI-SPEC §2), so maps are read with a visitor into `Vec<(key, value)>`.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader};
use std::marker::PhantomData;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::link_data::{LinkGraph, LinkGraphBuilder, LinkRow, UrlFacts};

/// A JSON object read in file order.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderedMap<T>(pub Vec<(String, T)>);

impl<T> Default for OrderedMap<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for OrderedMap<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OrderedVisitor<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for OrderedVisitor<T> {
            type Value = OrderedMap<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some(entry) = map.next_entry::<String, T>()? {
                    entries.push(entry);
                }
                Ok(OrderedMap(entries))
            }
        }

        deserializer.deserialize_map(OrderedVisitor(PhantomData))
    }
}

/// A table cell: the crawler writes strings; any other JSON value is shown as text.
#[derive(Debug, Clone, PartialEq)]
struct Cell(String);

impl<'de> Deserialize<'de> for Cell {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Cell(match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::String(text) => text,
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        }))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Deduction {
    pub points: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Score {
    /// `overall`, `performance`, `seo`, `security`, `accessibility`, `best-practices`.
    pub code: String,
    pub name: String,
    /// 0.0–10.0.
    pub score: f64,
    /// The crawler's band label, e.g. `Excellent`, `Good`, `Fair`.
    pub label: String,
    pub weight: f64,
    pub deductions: Vec<Deduction>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QualityScores {
    pub overall: Score,
    pub categories: Vec<Score>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Stats {
    pub total_urls: u64,
    pub total_size: u64,
    pub total_size_formatted: String,
    /// Seconds.
    pub total_execution_time: f64,
    pub total_requests_times: f64,
    pub total_requests_times_avg: f64,
    pub total_requests_times_min: f64,
    pub total_requests_times_max: f64,
    pub total_requests_times_p90: Option<f64>,
    /// Status code → count, in file order.
    pub count_by_status: OrderedMap<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Critical,
    Warning,
    Notice,
    Ok,
    Info,
    /// A status this version does not know.
    Other,
}

impl Severity {
    pub fn parse(status: &str) -> Self {
        match status {
            "CRITICAL" => Self::Critical,
            "WARNING" => Self::Warning,
            "NOTICE" => Self::Notice,
            "OK" => Self::Ok,
            "INFO" => Self::Info,
            _ => Self::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "Critical",
            Self::Warning => "Warning",
            Self::Notice => "Notice",
            Self::Ok => "OK",
            Self::Info => "Info",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SummaryItem {
    pub apl_code: String,
    pub severity: Severity,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub key: String,
    pub name: String,
    pub width: i64,
}

/// An analysis table: its own columns in file order, rows as strings aligned to the columns.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultTable {
    /// The key in `tables`, e.g. `404`, `seo`.
    pub key: String,
    pub apl_code: String,
    pub title: String,
    pub position: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResultData {
    pub scores: Option<QualityScores>,
    pub stats: Option<Stats>,
    pub summary: Vec<SummaryItem>,
    /// The analysis tables — not the `links` table, which is read into [`Self::links`].
    pub tables: Vec<ResultTable>,
    /// The run's options as the crawler applied them (`options`: `maxVisitedUrls`, `ignoreRegex`,
    /// …), kept as JSON so that a value of an unexpected type never makes the file unreadable.
    pub options: Option<serde_json::Value>,
    /// With `--link-details`: the `links` table (every referring page of every URL) and the link
    /// facts of the `results` rows (depth, in and out links). Shared: the run's links keep it too.
    pub links: Option<Arc<LinkGraph>>,
}

impl ResultData {
    /// An option of the run (`options.<key>`), e.g. `maxDepth`.
    pub fn option(&self, key: &str) -> Option<&serde_json::Value> {
        self.options.as_ref()?.get(key)
    }

    pub fn count(&self, severity: Severity) -> usize {
        self.summary.iter().filter(|item| item.severity == severity).count()
    }

    pub fn table(&self, key: &str) -> Option<&ResultTable> {
        self.tables.iter().find(|table| table.key == key)
    }
}

/// What loading the result file gave: data, or a state the Run page explains.
#[derive(Debug, Clone, PartialEq)]
pub enum ResultLoad {
    Loaded(Box<ResultData>),
    /// The run wrote no result file (e.g. it was stopped before the analysis).
    Missing,
    Unreadable(String),
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawResult {
    quality_scores: Option<QualityScores>,
    stats: Option<Stats>,
    summary: Option<RawSummary>,
    tables: Option<RawTables>,
    options: Option<serde_json::Value>,
    /// Only the link facts of each row are read (`--link-details`); the rest is skipped.
    results: RawUrlFactsList,
}

/// `tables`, in file order; the `links` table (`--link-details`, possibly tens of thousands of
/// rows) is read into typed rows instead of a generic table.
#[derive(Default)]
struct RawTables {
    tables: Vec<(String, RawTable)>,
    /// The `links` table, built into the graph row by row as it is read.
    links: Option<LinkGraphBuilder>,
}

impl<'de> Deserialize<'de> for RawTables {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TablesVisitor;

        impl<'de> Visitor<'de> for TablesVisitor {
            type Value = RawTables;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object of tables")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut tables = RawTables::default();
                while let Some(key) = map.next_key::<String>()? {
                    if key == LINKS_TABLE {
                        tables.links = Some(map.next_value::<RawLinksTable>()?.0);
                    } else {
                        let table = map.next_value::<RawTable>()?;
                        tables.tables.push((key, table));
                    }
                }
                Ok(tables)
            }
        }

        deserializer.deserialize_map(TablesVisitor)
    }
}

/// The key of the `--link-details` table in `tables`.
pub const LINKS_TABLE: &str = "links";

/// The `links` table: its `rows` go into the graph one by one (the other keys are skipped).
struct RawLinksTable(LinkGraphBuilder);

impl<'de> Deserialize<'de> for RawLinksTable {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TableVisitor;

        impl<'de> Visitor<'de> for TableVisitor {
            type Value = RawLinksTable;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("the links table")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut builder = LinkGraphBuilder::default();
                while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                    if key == "rows" {
                        map.next_value_seed(RowsSeed(&mut builder))?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(RawLinksTable(builder))
            }
        }

        deserializer.deserialize_map(TableVisitor)
    }
}

/// Reads the rows of the `links` table into the graph as they come.
struct RowsSeed<'a>(&'a mut LinkGraphBuilder);

impl<'de> DeserializeSeed<'de> for RowsSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        struct RowsVisitor<'a>(&'a mut LinkGraphBuilder);

        impl<'de> Visitor<'de> for RowsVisitor<'_> {
            type Value = ();

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("the rows of the links table")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
                while let Some(row) = seq.next_element::<RawLinkRow>()? {
                    self.0.add(row.into());
                }
                Ok(())
            }
        }

        deserializer.deserialize_seq(RowsVisitor(self.0))
    }
}

/// A row of the `links` table: string cells (any other JSON value is read as text).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawLinkRow {
    url: Lenient,
    source_url: Lenient,
    source_attr: Lenient,
    link_text: Lenient,
    source_line: Lenient,
    source_column: Lenient,
    in_links: Lenient,
    external: Lenient,
}

impl From<RawLinkRow> for LinkRow {
    fn from(raw: RawLinkRow) -> Self {
        let number = |cell: &Lenient| cell.0.trim().parse::<u32>().ok();
        LinkRow {
            source_line: number(&raw.source_line),
            source_column: number(&raw.source_column),
            in_links: number(&raw.in_links),
            external: matches!(raw.external.0.trim(), "1" | "true"),
            url: raw.url.0,
            source_url: raw.source_url.0,
            source_attr: raw.source_attr.0,
            link_text: raw.link_text.0,
        }
    }
}

/// A value read as text whatever its JSON type (`null`, an object or an array: empty), so that
/// an unexpected value never makes the file unreadable.
#[derive(Default)]
struct Lenient(String);

impl<'de> Deserialize<'de> for Lenient {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct LenientVisitor;

        impl<'de> Visitor<'de> for LenientVisitor {
            type Value = Lenient;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_str<E>(self, value: &str) -> Result<Lenient, E> {
                Ok(Lenient(value.to_string()))
            }

            fn visit_string<E>(self, value: String) -> Result<Lenient, E> {
                Ok(Lenient(value))
            }

            fn visit_bool<E>(self, value: bool) -> Result<Lenient, E> {
                Ok(Lenient(value.to_string()))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Lenient, E> {
                Ok(Lenient(value.to_string()))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Lenient, E> {
                Ok(Lenient(value.to_string()))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Lenient, E> {
                Ok(Lenient(value.to_string()))
            }

            fn visit_unit<E>(self) -> Result<Lenient, E> {
                Ok(Lenient::default())
            }

            fn visit_none<E>(self) -> Result<Lenient, E> {
                Ok(Lenient::default())
            }

            fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Lenient, D::Error> {
                Lenient::deserialize(deserializer)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Lenient, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Lenient::default())
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Lenient, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Lenient::default())
            }
        }

        deserializer.deserialize_any(LenientVisitor)
    }
}

/// The link facts of the `results` rows (`--link-details`): `(url, facts)`; any other value of
/// `results` (or of a row) is skipped.
#[derive(Default)]
struct RawUrlFactsList(Vec<(String, UrlFacts)>);

impl<'de> Deserialize<'de> for RawUrlFactsList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ListVisitor;

        impl<'de> Visitor<'de> for ListVisitor {
            type Value = RawUrlFactsList;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("the results rows")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut rows = Vec::new();
                while let Some(row) = seq.next_element::<RawUrlFacts>()? {
                    if let Some(row) = row.0 {
                        rows.push(row);
                    }
                }
                Ok(RawUrlFactsList(rows))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(RawUrlFactsList::default())
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(RawUrlFactsList::default())
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(RawUrlFactsList::default())
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(RawUrlFactsList::default())
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(RawUrlFactsList::default())
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(RawUrlFactsList::default())
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(RawUrlFactsList::default())
            }
        }

        deserializer.deserialize_any(ListVisitor)
    }
}

/// One `results` row: its URL and link facts when it has any.
struct RawUrlFacts(Option<(String, UrlFacts)>);

impl<'de> Deserialize<'de> for RawUrlFacts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RowVisitor;

        impl<'de> Visitor<'de> for RowVisitor {
            type Value = RawUrlFacts;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a results row")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut url = None;
                let mut facts = UrlFacts::default();
                let number = |cell: Lenient| cell.0.trim().parse::<u32>().ok();
                while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                    match key.as_ref() {
                        "url" => url = Some(map.next_value::<Lenient>()?.0),
                        "depth" => facts.depth = number(map.next_value()?),
                        "inLinks" => facts.in_links = number(map.next_value()?),
                        "outLinks" => facts.out_links = number(map.next_value()?),
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(RawUrlFacts(
                    url.filter(|_| facts != UrlFacts::default()).map(|url| (url, facts)),
                ))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(RawUrlFacts(None))
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(RawUrlFacts(None))
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(RawUrlFacts(None))
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(RawUrlFacts(None))
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(RawUrlFacts(None))
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(RawUrlFacts(None))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(RawUrlFacts(None))
            }
        }

        deserializer.deserialize_any(RowVisitor)
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSummary {
    items: Vec<RawItem>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawItem {
    apl_code: String,
    status: String,
    text: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawTable {
    apl_code: String,
    title: String,
    position: String,
    columns: OrderedMap<RawColumn>,
    rows: Vec<OrderedMap<Cell>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawColumn {
    name: String,
    width: i64,
}

impl From<RawResult> for ResultData {
    fn from(raw: RawResult) -> Self {
        let summary = raw
            .summary
            .map(|summary| {
                summary
                    .items
                    .into_iter()
                    .map(|item| SummaryItem {
                        apl_code: item.apl_code,
                        severity: Severity::parse(&item.status),
                        text: item.text,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (tables, link_rows) = raw
            .tables
            .map(|tables| (tables.tables, tables.links))
            .unwrap_or_default();
        let links = (link_rows.is_some() || !raw.results.0.is_empty())
            .then(|| link_rows.unwrap_or_default().finish(raw.results.0))
            .filter(|graph| !graph.is_empty())
            .map(Arc::new);
        let tables = Some(tables)
            .map(|tables| {
                tables
                    .into_iter()
                    .map(|(key, table)| {
                        let columns: Vec<Column> = table
                            .columns
                            .0
                            .into_iter()
                            .map(|(key, column)| Column {
                                name: if column.name.is_empty() {
                                    key.clone()
                                } else {
                                    column.name
                                },
                                key,
                                width: column.width,
                            })
                            .collect();
                        let rows = table
                            .rows
                            .into_iter()
                            .map(|row| {
                                columns
                                    .iter()
                                    .map(|column| {
                                        row.0
                                            .iter()
                                            .find(|(key, _)| *key == column.key)
                                            .map(|(_, cell)| cell.0.clone())
                                            .unwrap_or_default()
                                    })
                                    .collect()
                            })
                            .collect();
                        ResultTable {
                            key,
                            apl_code: table.apl_code,
                            title: table.title,
                            position: table.position,
                            columns,
                            rows,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        ResultData {
            scores: raw.quality_scores,
            stats: raw.stats,
            summary,
            tables,
            options: raw.options,
            links,
        }
    }
}

/// Parses result JSON text.
pub fn parse_result(json: &str) -> Result<ResultData, String> {
    serde_json::from_str::<RawResult>(json)
        .map(ResultData::from)
        .map_err(|error| error.to_string())
}

/// Loads the result file (call it on a worker thread: the file can be large).
pub fn load_result(path: &Path) -> ResultLoad {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return ResultLoad::Missing,
        Err(error) => return ResultLoad::Unreadable(format!("The result file cannot be opened: {error}")),
    };
    match serde_json::from_reader::<_, RawResult>(BufReader::new(file)) {
        Ok(raw) => ResultLoad::Loaded(Box::new(raw.into())),
        Err(error) => ResultLoad::Unreadable(format!("The result file cannot be read: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../docs/OUTPUT-crawler.siteone.io.json");

    #[test]
    fn the_fixture_reads_into_scores_stats_findings_and_tables() {
        let data = parse_result(FIXTURE).expect("parses");
        let scores = data.scores.as_ref().expect("scores");
        assert_eq!(scores.overall.score, 8.3);
        assert_eq!(scores.overall.label, "Good");
        let codes: Vec<&str> = scores.categories.iter().map(|c| c.code.as_str()).collect();
        assert_eq!(
            codes,
            ["performance", "seo", "security", "accessibility", "best-practices"]
        );
        let accessibility = &scores.categories[3];
        assert_eq!((accessibility.score, accessibility.deductions.len()), (5.0, 3));

        let stats = data.stats.as_ref().expect("stats");
        assert_eq!(stats.total_urls, 73);
        assert_eq!(stats.total_size, 21_514_206);
        assert_eq!(stats.total_requests_times_p90, None, "not in this older fixture");
        assert_eq!(
            stats.count_by_status.0,
            vec![("200".to_string(), 69), ("404".to_string(), 1), ("429".to_string(), 3)]
        );

        assert_eq!(data.summary.len(), 36);
        assert_eq!(data.count(Severity::Critical), 2);
        assert_eq!(data.count(Severity::Notice), 4);
        assert_eq!(data.summary[0].apl_code, "skipped");

        assert_eq!(data.tables.len(), 27);
        let not_found = data.table("404").expect("404 table");
        assert_eq!(not_found.title, "404 URLs");
        assert_eq!(not_found.position, "before-url-table");
        let names: Vec<&str> = not_found.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Found at URL", "Status", "URL 404"]);
        assert_eq!(
            not_found.rows,
            vec![vec![
                "https://crawler.siteone.io/features/performance-metrics/".to_string(),
                "404".to_string(),
                "https://crawler.siteone.io/docs/features/content-type-analysis".to_string(),
            ]]
        );
        let seo = data.table("seo").expect("seo table");
        assert_eq!(seo.rows.len(), 51);
        assert!(seo.rows.iter().all(|row| row.len() == seo.columns.len()));
    }

    #[test]
    fn columns_keep_the_file_order() {
        let json = r#"{"tables":{"z-table":{"title":"Z","columns":{"url":{"name":"URL"},"b":{"name":"B"},"a":{}},
            "rows":[{"a":"1","url":"/x","extra":"ignored"},{"b":2,"a":null,"url":true}]},"a-table":{"columns":{}}}}"#;
        let data = parse_result(json).expect("parses");
        let keys: Vec<&str> = data.tables.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["z-table", "a-table"]);
        let table = &data.tables[0];
        let columns: Vec<(&str, &str)> = table
            .columns
            .iter()
            .map(|c| (c.key.as_str(), c.name.as_str()))
            .collect();
        assert_eq!(columns, [("url", "URL"), ("b", "B"), ("a", "a")]);
        assert_eq!(table.rows, vec![vec!["/x", "", "1"], vec!["true", "2", ""]]);
        assert_eq!(data.scores, None);
    }

    #[test]
    fn the_links_table_and_link_facts_are_read_into_the_link_graph() {
        // As the crawler writes it with `--link-details` (string cells; numbers in `results`).
        let json = r#"{"results":[
              {"url":"https://site.example/","status":"200","depth":0,"inLinks":0,"outLinks":3},
              {"url":"https://site.example/gone","status":"404","source":"https://site.example/","depth":1,"inLinks":2,"outLinks":0},
              "not a row", {"url":"https://site.example/old"}],
            "tables":{"links":{"title":"Links","columns":{"url":{"name":"URL"}},"rows":[
              {"url":"https://site.example/gone","sourceUrl":"https://site.example/","sourceAttr":"<a href>","linkText":"Gone","sourceLine":"12","sourceColumn":"9","inLinks":"2","external":"0"},
              {"url":"https://site.example/gone","sourceUrl":"https://site.example/a","sourceAttr":"<a href>","linkText":"","sourceLine":"","sourceColumn":"","inLinks":2,"external":"0"},
              {"url":"https://other.example/","sourceUrl":"https://site.example/a","sourceAttr":"<a href>","linkText":{"odd":1},"sourceLine":null,"sourceColumn":"x","inLinks":"1","external":"1"}]},
              "404":{"columns":{"url":{"name":"URL"}},"rows":[{"url":"https://site.example/gone"}]}}}"#;
        let data = parse_result(json).expect("parses");
        let keys: Vec<&str> = data.tables.iter().map(|table| table.key.as_str()).collect();
        assert_eq!(keys, ["404"], "the links table is not an analysis table");
        let graph = data.links.as_deref().expect("the link graph");
        assert_eq!(graph.rows(), 3);
        let gone = graph.target("https://site.example/gone").expect("target");
        assert_eq!(gone.in_links, 2);
        assert_eq!(
            (gone.refs[0].text.as_deref(), gone.refs[0].line, gone.refs[0].column),
            (Some("Gone"), Some(12), Some(9))
        );
        assert_eq!((gone.refs[1].text.as_deref(), gone.refs[1].line), (None, None));
        let other = graph.target("https://other.example/").expect("external");
        assert!(other.external && other.refs[0].text.is_none() && other.refs[0].line.is_none());
        let facts = graph.facts("https://site.example/gone").expect("facts");
        assert_eq!(
            (facts.depth, facts.in_links, facts.out_links),
            (Some(1), Some(2), Some(0))
        );
        assert!(graph.facts("https://site.example/old").is_none());

        // Older files: no links table, no facts — no graph; an odd `results` never breaks the file.
        assert_eq!(parse_result(FIXTURE).expect("fixture").links, None);
        let odd = parse_result(r#"{"results":{"a":1},"tables":{"links":{"rows":[]}}}"#).expect("parses");
        assert_eq!(odd.links, None);
        assert!(parse_result(r#"{"results":"x"}"#).is_ok());
    }

    #[test]
    fn missing_and_unreadable_files_are_states_not_errors() {
        let tmp = tempfile::tempdir().expect("temp dir");
        assert_eq!(load_result(&tmp.path().join("result.json")), ResultLoad::Missing);
        let broken = tmp.path().join("broken.json");
        std::fs::write(&broken, "{\"stats\":").expect("written");
        assert!(matches!(load_result(&broken), ResultLoad::Unreadable(_)));
        let good = tmp.path().join("good.json");
        std::fs::write(&good, FIXTURE).expect("written");
        assert!(matches!(load_result(&good), ResultLoad::Loaded(data) if data.tables.len() == 27));
    }
}
