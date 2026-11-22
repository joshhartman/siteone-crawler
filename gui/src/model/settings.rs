//! App settings (`<config>/settings.json`): crawler path, runs folder, theme, UI scale, window.

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::paths::AppPaths;
use super::storage::{self, Recovered};

pub const SCHEMA_VERSION: u32 = 1;

pub const MIN_UI_SCALE: f32 = 0.8;
pub const MAX_UI_SCALE: f32 = 1.5;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Theme {
    /// Follow the system.
    #[default]
    System,
    Light,
    Dark,
}

/// Window position and size in logical points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// The crawler binary chosen by the user; `None` = detect (GUI-SPEC §4.1).
    pub crawler_path: Option<PathBuf>,
    /// `None` = `<data>/runs`.
    pub runs_dir: Option<PathBuf>,
    pub theme: Theme,
    pub ui_scale: f32,
    pub window: Option<WindowGeometry>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            crawler_path: None,
            runs_dir: None,
            theme: Theme::System,
            ui_scale: 1.0,
            window: None,
        }
    }
}

impl Settings {
    /// Loads the settings; a damaged file is set aside and the defaults are used (with a notice).
    pub fn load(paths: &AppPaths) -> Recovered<Settings> {
        let mut loaded: Recovered<Settings> = storage::load_or_recover(&paths.settings_file(), SCHEMA_VERSION);
        loaded.value = loaded.value.sanitized();
        loaded
    }

    pub fn save(&self, paths: &AppPaths) -> io::Result<()> {
        storage::write_json(&paths.settings_file(), SCHEMA_VERSION, self)
    }

    /// The runs folder in effect.
    pub fn runs_dir(&self, paths: &AppPaths) -> PathBuf {
        self.runs_dir.clone().unwrap_or_else(|| paths.default_runs_dir())
    }

    /// Values in range: the UI scale within 80–150 %, a window of a usable size.
    pub fn sanitized(mut self) -> Self {
        self.ui_scale = if self.ui_scale.is_finite() {
            self.ui_scale.clamp(MIN_UI_SCALE, MAX_UI_SCALE)
        } else {
            1.0
        };
        self.window = self.window.filter(|window| {
            [window.x, window.y, window.width, window.height]
                .iter()
                .all(|value| value.is_finite())
                && window.width >= 200.0
                && window.height >= 150.0
        });
        self.crawler_path = self.crawler_path.filter(|path| !path.as_os_str().is_empty());
        self.runs_dir = self.runs_dir.filter(|path| !path.as_os_str().is_empty());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_unknown_or_missing_fields_are_tolerated() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::at(tmp.path());
        let settings = Settings {
            crawler_path: Some(PathBuf::from(r"C:\Tools\siteone-crawler.exe")),
            theme: Theme::Dark,
            ui_scale: 1.25,
            ..Settings::default()
        };
        settings.save(&paths).expect("saved");
        let loaded = Settings::load(&paths);
        assert_eq!(loaded.value, settings);
        assert_eq!(loaded.notice, None);

        std::fs::write(
            paths.settings_file(),
            r#"{"schemaVersion":1,"theme":"light","futureField":1}"#,
        )
        .expect("written");
        let loaded = Settings::load(&paths);
        assert_eq!(loaded.value.theme, Theme::Light);
        assert_eq!(loaded.value.ui_scale, 1.0);
    }

    #[test]
    fn out_of_range_values_are_corrected() {
        let settings = Settings {
            ui_scale: 9.0,
            window: Some(WindowGeometry {
                x: 0.0,
                y: 0.0,
                width: f32::NAN,
                height: 600.0,
            }),
            runs_dir: Some(PathBuf::new()),
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(settings.ui_scale, MAX_UI_SCALE);
        assert_eq!(settings.window, None);
        assert_eq!(settings.runs_dir, None);
        let paths = AppPaths::at("D:\\gui");
        assert_eq!(settings.runs_dir(&paths), paths.default_runs_dir());
    }
}
