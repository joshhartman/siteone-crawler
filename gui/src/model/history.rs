//! The history of runs: one directory per run under `<runs>`, described by its `run.json`
//! (meta, configuration snapshot without secrets, redacted argv, summary numbers).
//!
//! `run.json` is written when the run starts and updated when it ends, so every run directory the
//! GUI created has one; directories without it are not runs and are neither listed nor deleted.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use siteone_crawler::utils::redact_url_userinfo;

use super::config::{CrawlConfig, Value};
use super::run_state::RunOutcome;
use super::storage::{self, LoadError};

pub const SCHEMA_VERSION: u32 = 1;

/// The file that marks a run directory.
pub const RUN_FILE: &str = "run.json";

/// How a run ended, as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordedOutcome {
    Finished,
    Stopped,
    QualityGateFailed,
    Failed,
    NoPagesCrawled,
    Crashed,
    Aborted,
}

impl RecordedOutcome {
    /// The stored form of an outcome; `None` for a run that is still going or was not recorded.
    pub fn from_outcome(outcome: &RunOutcome) -> Option<Self> {
        Some(match outcome {
            RunOutcome::Finished => Self::Finished,
            RunOutcome::Stopped => Self::Stopped,
            RunOutcome::QualityGateFailed => Self::QualityGateFailed,
            RunOutcome::Failed { .. } => Self::Failed,
            RunOutcome::NoPagesCrawled => Self::NoPagesCrawled,
            RunOutcome::Crashed { .. } => Self::Crashed,
            RunOutcome::Aborted => Self::Aborted,
            RunOutcome::Running | RunOutcome::Stopping | RunOutcome::ConfigError { .. } => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Finished => "Finished",
            Self::Stopped => "Stopped",
            Self::QualityGateFailed => "Quality gate failed",
            Self::Failed => "Failed",
            Self::NoPagesCrawled => "No pages crawled",
            Self::Crashed => "Crashed",
            Self::Aborted => "Aborted",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunSummary {
    pub urls: u64,
    pub errors: u64,
    pub total_bytes: u64,
    pub overall_score: Option<f64>,
    pub overall_label: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunRecord {
    pub run_id: String,
    pub url: String,
    /// RFC 3339.
    pub started_at: String,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    /// `None` while the run is going (or when the GUI died during it).
    pub outcome: Option<RecordedOutcome>,
    pub exit_code: Option<i32>,
    /// The error of a failed run.
    pub error: Option<String>,
    pub crawler_version: Option<String>,
    pub profile: Option<String>,
    /// The child's argv, redacted (see `run_plan::redact_argv`).
    pub argv: Vec<String>,
    /// The configuration, without secrets.
    pub config: CrawlConfig,
    pub summary: RunSummary,
}

impl RunRecord {
    /// Writes `run.json` into the run folder, which must exist: a run deleted from History while
    /// its record was still being written is not brought back (the write fails with `NotFound`).
    pub fn write(&self, run_dir: &Path) -> io::Result<()> {
        storage::write_json_in_existing_dir(&run_dir.join(RUN_FILE), SCHEMA_VERSION, self)
    }

    pub fn read(run_dir: &Path) -> Result<RunRecord, LoadError> {
        storage::read_json(&run_dir.join(RUN_FILE), SCHEMA_VERSION)?
            .ok_or_else(|| LoadError::Io(io::Error::new(io::ErrorKind::NotFound, "run.json is missing")))
    }
}

/// Options whose values may carry credentials in a URL (`user:password@`); the record keeps them
/// without the userinfo, like the redacted argv.
const URL_OPTIONS: &[&str] = &["--proxy", "--upload-to", "--ai-endpoint"];

/// Header values often are credentials (`Cookie`, `Authorization`): the record leaves them out.
pub const HEADER_OPTION: &str = "--header";

/// The configuration as stored in a run record: secrets are never in it (they live in the
/// credential store); in addition the userinfo of the URL and of URL options is removed and the
/// `--header` values are left out.
pub fn config_snapshot(config: &CrawlConfig) -> CrawlConfig {
    let mut snapshot = config.clone();
    snapshot.url = redact_url_userinfo(&config.url);
    snapshot.values.remove(HEADER_OPTION);
    for name in URL_OPTIONS {
        if let Some(value) = snapshot.values.get_mut(*name) {
            match value {
                Value::Text(text) => *text = redact_url_userinfo(text),
                Value::List(items) => items.iter_mut().for_each(|item| *item = redact_url_userinfo(item)),
                Value::Bool(_) => {}
            }
        }
    }
    snapshot
}

/// A run directory as listed: its record, or why it cannot be read.
#[derive(Debug)]
pub struct HistoryEntry {
    pub dir: PathBuf,
    pub record: Result<RunRecord, LoadError>,
}

impl HistoryEntry {
    pub fn run_id(&self) -> String {
        self.dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

/// The runs under `runs_dir`, newest first (run ids start with the start time).
pub fn list_runs(runs_dir: &Path) -> Vec<HistoryEntry> {
    let Ok(entries) = fs::read_dir(runs_dir) else {
        return Vec::new();
    };
    let mut runs: Vec<HistoryEntry> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|dir| dir.join(RUN_FILE).is_file())
        .map(|dir| HistoryEntry {
            record: RunRecord::read(&dir),
            dir,
        })
        .collect();
    runs.sort_by_key(|entry| std::cmp::Reverse(entry.run_id()));
    runs
}

#[derive(Debug)]
pub enum DeleteError {
    /// The directory is not a direct child of the runs folder, is a link, or has no `run.json`.
    NotARun(PathBuf),
    Io(io::Error),
}

impl std::fmt::Display for DeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeleteError::NotARun(dir) => write!(f, "{} is not a run directory of the runs folder", dir.display()),
            DeleteError::Io(error) => write!(f, "the run could not be deleted: {error}"),
        }
    }
}

impl std::error::Error for DeleteError {}

/// Deletes a run directory — only one that lies directly inside `runs_dir`, is a real directory
/// (no link) and contains `run.json` (GUI-SPEC §5.6).
pub fn delete_run(runs_dir: &Path, run_dir: &Path) -> Result<(), DeleteError> {
    let not_a_run = || DeleteError::NotARun(run_dir.to_path_buf());
    let metadata = fs::symlink_metadata(run_dir).map_err(|_| not_a_run())?;
    if !metadata.is_dir() {
        return Err(not_a_run());
    }
    let runs = fs::canonicalize(runs_dir).map_err(|_| not_a_run())?;
    let run = fs::canonicalize(run_dir).map_err(|_| not_a_run())?;
    if run.parent() != Some(runs.as_path()) || !run.join(RUN_FILE).is_file() {
        return Err(not_a_run());
    }
    fs::remove_dir_all(&run).map_err(DeleteError::Io)
}

/// The size of a directory tree in bytes; links are not followed, unreadable entries are skipped.
/// Call it on a worker thread.
pub fn dir_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| dir_size(&entry.path()))
        .fold(0, u64::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(run_id: &str) -> RunRecord {
        RunRecord {
            run_id: run_id.to_string(),
            url: "https://example.com/".to_string(),
            started_at: "2026-11-18T09:05:07+01:00".to_string(),
            outcome: Some(RecordedOutcome::Finished),
            summary: RunSummary {
                urls: 12,
                overall_score: Some(8.3),
                ..RunSummary::default()
            },
            ..RunRecord::default()
        }
    }

    #[test]
    fn runs_are_listed_newest_first_with_damaged_ones_included() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runs = tmp.path();
        for id in ["20261117-100000-a.cz", "20261118-090000-b.cz"] {
            let dir = runs.join(id);
            fs::create_dir(&dir).expect("dir");
            record(id).write(&dir).expect("written");
        }
        let damaged = runs.join("20261118-120000-c.cz");
        fs::create_dir(&damaged).expect("dir");
        fs::write(damaged.join(RUN_FILE), "{").expect("written");
        fs::create_dir(runs.join("not-a-run")).expect("dir");

        let listed = list_runs(runs);
        let ids: Vec<String> = listed.iter().map(HistoryEntry::run_id).collect();
        assert_eq!(
            ids,
            ["20261118-120000-c.cz", "20261118-090000-b.cz", "20261117-100000-a.cz"]
        );
        assert!(listed[0].record.is_err());
        assert_eq!(
            listed[1].record.as_ref().expect("record"),
            &record("20261118-090000-b.cz")
        );
    }

    #[test]
    fn delete_only_removes_run_directories_inside_the_runs_folder() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runs = tmp.path().join("runs");
        let run = runs.join("20261118-090000-b.cz");
        fs::create_dir_all(&run).expect("dir");
        record("x").write(&run).expect("written");
        fs::write(run.join("report.html"), "<html>").expect("written");

        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).expect("dir");
        fs::write(outside.join(RUN_FILE), "{}").expect("written");
        assert!(matches!(delete_run(&runs, &outside), Err(DeleteError::NotARun(_))));
        assert!(matches!(delete_run(&runs, &runs), Err(DeleteError::NotARun(_))));
        assert!(matches!(
            delete_run(&runs, &runs.join("..").join("outside")),
            Err(DeleteError::NotARun(_))
        ));
        let no_record = runs.join("no-record");
        fs::create_dir_all(&no_record).expect("dir");
        assert!(matches!(delete_run(&runs, &no_record), Err(DeleteError::NotARun(_))));
        let nested = run.join("nested");
        fs::create_dir_all(&nested).expect("dir");
        fs::write(nested.join(RUN_FILE), "{}").expect("written");
        assert!(matches!(delete_run(&runs, &nested), Err(DeleteError::NotARun(_))));

        assert!(dir_size(&run) > 6);
        delete_run(&runs, &run).expect("deleted");
        assert!(!run.exists() && outside.exists() && no_record.exists());
    }

    #[test]
    fn a_deleted_run_folder_is_not_created_again_by_its_record() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let run = tmp.path().join("20261118-090000-b.cz");
        let error = record("x").write(&run).expect_err("the folder is gone");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!run.exists());
        assert!(
            fs::read_dir(tmp.path()).expect("dir").next().is_none(),
            "no temp file left"
        );
    }

    #[test]
    fn the_snapshot_leaves_out_credentials() {
        let mut config = CrawlConfig::new("https://user:pw@example.com/");
        config
            .values
            .insert("--proxy".to_string(), Value::Text("http://p:s@proxy:8080".to_string()));
        config
            .values
            .insert(HEADER_OPTION.to_string(), Value::List(vec!["Cookie: a=b".to_string()]));
        config
            .values
            .insert("--workers".to_string(), Value::Text("3".to_string()));
        let snapshot = config_snapshot(&config);
        let json = serde_json::to_string(&snapshot).expect("json");
        for secret in ["pw@", "p:s@", "a=b"] {
            assert!(!json.contains(secret), "{secret} in {json}");
        }
        assert_eq!(snapshot.url, "https://example.com/");
        assert_eq!(snapshot.values.get("--workers"), Some(&Value::Text("3".to_string())));
    }

    #[test]
    fn outcomes_map_to_their_stored_form() {
        assert_eq!(
            RecordedOutcome::from_outcome(&RunOutcome::Failed { error: None }),
            Some(RecordedOutcome::Failed)
        );
        assert_eq!(RecordedOutcome::from_outcome(&RunOutcome::Running), None);
        assert_eq!(
            RecordedOutcome::from_outcome(&RunOutcome::ConfigError { message: None }),
            None
        );
        let json = serde_json::to_string(&RecordedOutcome::QualityGateFailed).expect("json");
        assert_eq!(json, "\"qualityGateFailed\"");
    }
}
