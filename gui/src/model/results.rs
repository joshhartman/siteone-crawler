//! A typed view of the result JSON (`--output-json-file`, `docs/JSON-OUTPUT.md`): quality scores,
//! stats, summary findings, analysis tables and the run's `options`. Fields the GUI does not use
//! (the per-URL `results`, …) are skipped while reading, so memory stays small for large runs.
//!
//! Table columns keep the order of the file. The crawler may not depend on `serde_json`'s
//! `preserve_order` (GUI-SPEC §2), so maps are read with a visitor into `Vec<(key, value)>`.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader};
use std::marker::PhantomData;
use std::path::Path;

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};

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
    pub tables: Vec<ResultTable>,
    /// The run's options as the crawler applied them (`options`: `maxVisitedUrls`, `ignoreRegex`,
    /// …), kept as JSON so that a value of an unexpected type never makes the file unreadable.
    pub options: Option<serde_json::Value>,
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
    tables: Option<OrderedMap<RawTable>>,
    options: Option<serde_json::Value>,
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
        let tables = raw
            .tables
            .map(|tables| {
                tables
                    .0
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
