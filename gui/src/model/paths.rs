//! Where the GUI keeps its files, the run directories, and how it finds the crawler binary.
//!
//! - `<config>` = `%APPDATA%\siteone-crawler-gui`: settings, the working configuration, profiles.
//! - `<data>` = `%LOCALAPPDATA%\siteone-crawler-gui`: runs (unless moved in Settings), caches,
//!   the crash log; also the GUI's working directory (GUI-SPEC §2).
//! - `SITEONE_GUI_DATA_DIR` overrides both roots (tests, portable use).

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

/// The directory name under the OS config and data directories.
pub const APP_DIR: &str = "siteone-crawler-gui";

/// Overrides both the config and the data root.
pub const DATA_DIR_ENV: &str = "SITEONE_GUI_DATA_DIR";

/// The empty config file the app passes to the crawler ([`AppPaths::empty_config_file`]).
pub const EMPTY_CONFIG_FILE: &str = "empty-config.conf";

/// The longest host part of a run id.
const MAX_HOST_CHARS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config: PathBuf,
    pub data: PathBuf,
}

impl AppPaths {
    /// The paths of this machine and user. `None` when the OS reports no config or data directory
    /// and no override is set.
    pub fn discover() -> Option<Self> {
        // The GUI changes its working directory to the data folder: a relative override must not
        // be read against it again.
        let override_dir = std::env::var_os(DATA_DIR_ENV)
            .filter(|dir| !dir.is_empty())
            .map(|dir| std::path::absolute(&dir).map_or(dir, PathBuf::into_os_string));
        Self::resolve(override_dir, dirs::config_dir(), dirs::data_local_dir())
    }

    /// Pure form of [`Self::discover`]: an override (if set and not empty) is both roots.
    pub fn resolve(
        override_dir: Option<OsString>,
        config_base: Option<PathBuf>,
        data_base: Option<PathBuf>,
    ) -> Option<Self> {
        if let Some(root) = override_dir.filter(|dir| !dir.is_empty()) {
            return Some(Self::at(root));
        }
        Some(Self {
            config: config_base?.join(APP_DIR),
            data: data_base?.join(APP_DIR),
        })
    }

    /// Both roots in one directory.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config: root.clone(),
            data: root,
        }
    }

    /// Creates the directories the GUI writes into.
    pub fn ensure(&self) -> io::Result<()> {
        for dir in [
            self.config.clone(),
            self.data.clone(),
            self.profiles_dir(),
            self.http_cache_dir(),
            self.ai_cache_dir(),
        ] {
            fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    pub fn session_file(&self) -> PathBuf {
        self.config.join("session.json")
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.config.join("profiles")
    }

    /// `<data>/runs`; Settings may name another directory.
    pub fn default_runs_dir(&self) -> PathBuf {
        self.data.join("runs")
    }

    pub fn http_cache_dir(&self) -> PathBuf {
        self.data.join("cache").join("http")
    }

    pub fn ai_cache_dir(&self) -> PathBuf {
        self.data.join("cache").join("ai")
    }

    pub fn crash_log(&self) -> PathBuf {
        self.data.join("crash.log")
    }

    /// `<data>/empty-config.conf`, an empty file the app names in `--config-file` of every crawler
    /// it starts and of its own validation, so that a CLI config file of the user
    /// (`~/.siteone-crawler.conf`, the CLI's system file) never changes what Setup shows (GUI-SPEC
    /// §4.2). Written empty when it is missing or not empty; a file that cannot be written leaves
    /// it to the crawler's error to say so.
    pub fn empty_config_file(&self) -> PathBuf {
        let file = self.data.join(EMPTY_CONFIG_FILE);
        if !fs::metadata(&file).is_ok_and(|meta| meta.is_file() && meta.len() == 0) {
            let _ = fs::write(&file, b"");
        }
        file
    }
}

/// The run id before collision handling: `YYYYMMDD-HHMMSS-<host>` (GUI-SPEC §4.2).
pub fn run_id_base(now: &DateTime<Local>, url: &str) -> String {
    format!("{}-{}", now.format("%Y%m%d-%H%M%S"), sanitize_host(url))
}

/// The host of `url` as a safe file-name part: lowercase `a-z 0-9 . -`, anything else `-`, at most
/// 60 characters; `run` when there is none.
pub fn sanitize_host(url: &str) -> String {
    let host = url::Url::parse(url.trim())
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
        .unwrap_or_default();
    let mut safe = String::new();
    for c in host.to_lowercase().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
            c
        } else {
            '-'
        };
        safe.push(c);
    }
    let safe: String = safe.trim_matches(['-', '.']).chars().take(MAX_HOST_CHARS).collect();
    let safe = safe.trim_end_matches(['-', '.']).to_string();
    if safe.is_empty() { "run".to_string() } else { safe }
}

/// Creates the run directory `<runs>/<base>`, or `<base>-2`, `<base>-3`, … when it exists.
/// Returns the final run id and directory.
pub fn create_run_dir(runs_dir: &Path, base: &str) -> io::Result<(String, PathBuf)> {
    fs::create_dir_all(runs_dir)?;
    for n in 1..=1000 {
        let id = if n == 1 {
            base.to_string()
        } else {
            format!("{base}-{n}")
        };
        let dir = runs_dir.join(&id);
        match fs::create_dir(&dir) {
            Ok(()) => return Ok((id, dir)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("no free run directory for {base}"),
    ))
}

/// `siteone-crawler.exe` on Windows, `siteone-crawler` elsewhere.
pub fn crawler_binary_name() -> String {
    format!("siteone-crawler{}", std::env::consts::EXE_SUFFIX)
}

/// Where the crawler binary was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrawlerSource {
    Settings,
    NextToGui,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrawlerLocation {
    pub path: PathBuf,
    pub source: CrawlerSource,
}

/// Finds the crawler (GUI-SPEC §4.1): the path in Settings → next to the GUI executable → `PATH`.
/// Only existing files count.
pub fn find_crawler(
    configured: Option<&Path>,
    gui_exe: Option<&Path>,
    path_var: Option<&OsStr>,
) -> Option<CrawlerLocation> {
    let found = |path: PathBuf, source| path.is_file().then_some(CrawlerLocation { path, source });
    if let Some(location) = configured.and_then(|path| found(path.to_path_buf(), CrawlerSource::Settings)) {
        return Some(location);
    }
    let name = crawler_binary_name();
    if let Some(location) = gui_exe
        .and_then(Path::parent)
        .and_then(|dir| found(dir.join(&name), CrawlerSource::NextToGui))
    {
        return Some(location);
    }
    std::env::split_paths(path_var?)
        .filter(|dir| !dir.as_os_str().is_empty())
        .find_map(|dir| found(dir.join(&name), CrawlerSource::Path))
}

/// [`find_crawler`] for this process: its executable and `PATH`.
pub fn locate_crawler(configured: Option<&Path>) -> Option<CrawlerLocation> {
    let exe = std::env::current_exe().ok();
    let path_var = std::env::var_os("PATH");
    find_crawler(configured, exe.as_deref(), path_var.as_deref())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn the_override_is_both_roots_and_an_empty_one_is_ignored() {
        let paths = AppPaths::resolve(
            Some("D:\\portable".into()),
            Some("C:\\cfg".into()),
            Some("C:\\data".into()),
        )
        .expect("paths");
        assert_eq!(paths, AppPaths::at("D:\\portable"));
        let paths = AppPaths::resolve(Some("".into()), Some("C:\\cfg".into()), Some("C:\\data".into())).expect("paths");
        assert_eq!(paths.config, PathBuf::from("C:\\cfg").join(APP_DIR));
        assert_eq!(paths.data, PathBuf::from("C:\\data").join(APP_DIR));
        assert_eq!(AppPaths::resolve(None, None, Some("C:\\data".into())), None);
        assert_eq!(paths.http_cache_dir(), paths.data.join("cache").join("http"));
    }

    #[test]
    fn run_ids_carry_time_and_a_safe_host() {
        let now = Local.with_ymd_and_hms(2026, 11, 18, 9, 5, 7).single().expect("time");
        assert_eq!(
            run_id_base(&now, "https://Example.COM:8443/a?b"),
            "20261118-090507-example.com"
        );
        assert_eq!(sanitize_host("http://[::1]:8080/"), "1");
        assert_eq!(sanitize_host("http://127.0.0.1:8888/"), "127.0.0.1");
        assert_eq!(sanitize_host("https://xn--crawl-0ta.cz/"), "xn--crawl-0ta.cz");
        assert_eq!(sanitize_host("not a url"), "run");
        let long = format!("https://{}.com/", "a".repeat(100));
        assert_eq!(sanitize_host(&long).len(), MAX_HOST_CHARS);
    }

    #[test]
    fn colliding_run_directories_get_a_numeric_suffix() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runs = tmp.path().join("runs");
        let (first, first_dir) = create_run_dir(&runs, "20261118-090507-example.com").expect("first");
        let (second, second_dir) = create_run_dir(&runs, "20261118-090507-example.com").expect("second");
        assert_eq!(first, "20261118-090507-example.com");
        assert_eq!(second, "20261118-090507-example.com-2");
        assert!(first_dir.is_dir() && second_dir.is_dir());
    }

    #[test]
    fn the_crawler_is_found_in_the_documented_order() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let name = crawler_binary_name();
        let make = |dir: &str| {
            let dir = tmp.path().join(dir);
            fs::create_dir_all(&dir).expect("dir");
            let file = dir.join(&name);
            fs::write(&file, b"").expect("file");
            file
        };
        let configured = make("configured ž");
        let next_to_gui = make("gui");
        let on_path = make("path");
        let gui_exe = tmp.path().join("gui").join("siteone-crawler-gui.exe");
        let path_var = std::env::join_paths([tmp.path().join("nothing"), tmp.path().join("path")]).expect("PATH");

        let found = find_crawler(Some(&configured), Some(&gui_exe), Some(&path_var)).expect("found");
        assert_eq!((found.path, found.source), (configured, CrawlerSource::Settings));
        let missing = tmp.path().join("missing.exe");
        let found = find_crawler(Some(&missing), Some(&gui_exe), Some(&path_var)).expect("found");
        assert_eq!((found.path, found.source), (next_to_gui, CrawlerSource::NextToGui));
        let found = find_crawler(None, None, Some(&path_var)).expect("found");
        assert_eq!((found.path, found.source), (on_path, CrawlerSource::Path));
        assert_eq!(find_crawler(None, None, None), None);
    }
}
