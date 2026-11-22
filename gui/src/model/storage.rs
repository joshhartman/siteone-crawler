//! JSON files of the GUI: atomic writes, a `schemaVersion` in every file, and recovery from a
//! file that cannot be read (GUI-SPEC §5.6).
//!
//! - A write goes to a temporary file in the same directory, is flushed to disk and then renamed
//!   over the target, so a crash never leaves a half-written file.
//! - Every file is a JSON object with `"schemaVersion"`.
//! - [`load_or_recover`] renames a file that fails to parse (or was written by a newer version) to
//!   `<name>.corrupt-<timestamp>` and returns the defaults with a notice for a toast.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The key of the schema version in every file.
pub const SCHEMA_VERSION_KEY: &str = "schemaVersion";

/// Larger files are not read (they are not ours or are damaged).
pub const MAX_JSON_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    TooLarge(u64),
    Parse(String),
    /// Written by a newer version of the GUI.
    NewerVersion {
        found: u64,
        supported: u32,
    },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(error) => write!(f, "cannot read the file: {error}"),
            LoadError::TooLarge(bytes) => write!(f, "the file is too large ({bytes} bytes)"),
            LoadError::Parse(error) => write!(f, "the file is damaged: {error}"),
            LoadError::NewerVersion { found, supported } => write!(
                f,
                "the file was written by a newer version (schema {found}, this version reads up to {supported})"
            ),
        }
    }
}

impl std::error::Error for LoadError {}

/// Writes `value` (a struct, serialized as a JSON object) with `schemaVersion` atomically.
pub fn write_json<T: Serialize>(path: &Path, schema_version: u32, value: &T) -> io::Result<()> {
    write_atomic(path, &json_bytes(schema_version, value)?)
}

/// Like [`write_json`], but the directory of `path` must exist: a folder deleted in the meantime
/// (a run deleted from History) is not created again — the write fails with `NotFound`.
pub fn write_json_in_existing_dir<T: Serialize>(path: &Path, schema_version: u32, value: &T) -> io::Result<()> {
    write_atomic_in(path, &json_bytes(schema_version, value)?, false)
}

fn json_bytes<T: Serialize>(schema_version: u32, value: &T) -> io::Result<Vec<u8>> {
    let mut json = serde_json::to_value(value).map_err(io::Error::other)?;
    let object = json
        .as_object_mut()
        .ok_or_else(|| io::Error::other("only JSON objects are stored"))?;
    object.insert(SCHEMA_VERSION_KEY.to_string(), schema_version.into());
    let mut bytes = serde_json::to_vec_pretty(&json).map_err(io::Error::other)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Writes `bytes` to `path` through a temporary file in the same directory and a rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_in(path, bytes, true)
}

/// Like [`write_atomic`], but the directory of `path` must exist (see
/// [`write_json_in_existing_dir`]).
pub fn write_atomic_in_existing_dir(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_in(path, bytes, false)
}

fn write_atomic_in(path: &Path, bytes: &[u8], create_dir: bool) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if create_dir {
        fs::create_dir_all(dir)?;
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path names no file"))?
        .to_string_lossy();
    let temp = dir.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Reads a file written by [`write_json`]. `Ok(None)` when it does not exist.
pub fn read_json<T: DeserializeOwned>(path: &Path, schema_version: u32) -> Result<Option<T>, LoadError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(LoadError::Io(error)),
    };
    let size = file.metadata().map_err(LoadError::Io)?.len();
    if size > MAX_JSON_BYTES {
        return Err(LoadError::TooLarge(size));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(MAX_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(LoadError::Io)?;
    let mut json: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| LoadError::Parse(error.to_string()))?;
    let object = json
        .as_object_mut()
        .ok_or_else(|| LoadError::Parse("not a JSON object".to_string()))?;
    let found = object
        .remove(SCHEMA_VERSION_KEY)
        .and_then(|version| version.as_u64())
        .ok_or_else(|| LoadError::Parse(format!("no {SCHEMA_VERSION_KEY}")))?;
    if found > u64::from(schema_version) {
        return Err(LoadError::NewerVersion {
            found,
            supported: schema_version,
        });
    }
    serde_json::from_value(json)
        .map(Some)
        .map_err(|error| LoadError::Parse(error.to_string()))
}

/// A loaded value, with a message for the user when the file had to be set aside.
#[derive(Debug, Clone, PartialEq)]
pub struct Recovered<T> {
    pub value: T,
    pub notice: Option<String>,
}

/// Reads a file, or returns the defaults: silently when it does not exist; after renaming it to
/// `*.corrupt-<timestamp>` and with a notice when it cannot be read.
pub fn load_or_recover<T: DeserializeOwned + Default>(path: &Path, schema_version: u32) -> Recovered<T> {
    match read_json(path, schema_version) {
        Ok(Some(value)) => Recovered { value, notice: None },
        Ok(None) => Recovered {
            value: T::default(),
            notice: None,
        },
        Err(error) => {
            let kept = match quarantine(path) {
                Ok(kept) => format!("It was kept as {}.", kept.display()),
                Err(rename) => format!("It could not be set aside ({rename})."),
            };
            Recovered {
                value: T::default(),
                notice: Some(format!(
                    "{} could not be loaded: {error}. Defaults are used. {kept}",
                    path.display()
                )),
            }
        }
    }
}

/// Renames a file to `<name>.corrupt-<timestamp>` and returns the new path.
pub fn quarantine(path: &Path) -> io::Result<PathBuf> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path names no file"))?
        .to_string_lossy()
        .to_string();
    let mut target = path.with_file_name(format!("{name}.corrupt-{stamp}"));
    let mut n = 2;
    while target.exists() {
        target = path.with_file_name(format!("{name}.corrupt-{stamp}-{n}"));
        n += 1;
    }
    fs::rename(path, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Sample {
        name: String,
        count: u32,
    }

    #[test]
    fn a_written_file_reads_back_and_carries_its_schema_version() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let path = tmp.path().join("sub dir ž").join("sample.json");
        let sample = Sample {
            name: "a \"b\"".to_string(),
            count: 3,
        };
        write_json(&path, 1, &sample).expect("written");
        let text = fs::read_to_string(&path).expect("text");
        assert!(text.contains("\"schemaVersion\": 1"), "{text}");
        assert_eq!(read_json::<Sample>(&path, 1).expect("read"), Some(sample));
        // No temporary file is left behind.
        let entries = fs::read_dir(path.parent().expect("dir")).expect("list").count();
        assert_eq!(entries, 1);
    }

    #[test]
    fn a_missing_file_gives_the_defaults_without_a_notice() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let loaded: Recovered<Sample> = load_or_recover(&tmp.path().join("none.json"), 1);
        assert_eq!(loaded.value, Sample::default());
        assert_eq!(loaded.notice, None);
    }

    #[test]
    fn a_corrupt_file_is_set_aside_and_the_defaults_are_used() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let path = tmp.path().join("settings.json");
        fs::write(&path, "{ not json").expect("written");
        let loaded: Recovered<Sample> = load_or_recover(&path, 1);
        assert_eq!(loaded.value, Sample::default());
        assert!(
            loaded
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("damaged"))
        );
        assert!(!path.exists());
        let kept: Vec<_> = fs::read_dir(tmp.path())
            .expect("list")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(kept.len(), 1);
        assert!(kept[0].starts_with("settings.json.corrupt-"), "{kept:?}");
    }

    #[test]
    fn wrong_shapes_and_newer_versions_are_errors() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let path = tmp.path().join("x.json");
        fs::write(&path, r#"{"name":"a","count":1}"#).expect("written");
        assert!(matches!(read_json::<Sample>(&path, 1), Err(LoadError::Parse(_))));
        fs::write(&path, r#"{"schemaVersion":9,"name":"a","count":1}"#).expect("written");
        assert!(matches!(
            read_json::<Sample>(&path, 1),
            Err(LoadError::NewerVersion { found: 9, .. })
        ));
        fs::write(&path, r#"{"schemaVersion":1,"name":5}"#).expect("written");
        assert!(matches!(read_json::<Sample>(&path, 1), Err(LoadError::Parse(_))));
        fs::write(&path, "[1,2]").expect("written");
        assert!(matches!(read_json::<Sample>(&path, 1), Err(LoadError::Parse(_))));
    }

    #[test]
    fn an_atomic_write_replaces_an_existing_file() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let path = tmp.path().join("a.json");
        write_atomic(&path, b"one").expect("first");
        write_atomic(&path, b"two").expect("second");
        assert_eq!(fs::read(&path).expect("read"), b"two");
    }
}
