//! Helpers shared by the UI tests: an [`App`] on a temporary data folder with an in-memory secret
//! store, scripted file dialogs and a fixed crawler; an `egui_kittest` harness around it; and
//! PNG snapshots.
//!
//! Snapshots are "write and check" images, not pixel comparisons: the pixels depend on the GPU
//! adapter and on the system fonts (Segoe UI when present), so a comparison would make
//! `cargo test` fail on another machine for no fault of the code. Each snapshot is rendered,
//! checked to have the window's size and to be no blank canvas, and written to
//! `gui/tests/snapshots/<name>.png` for a person to look at. Without a wgpu adapter the snapshot is
//! skipped with a message.

#![allow(dead_code)]

pub mod runs;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use eframe::egui;
use egui_kittest::Harness;
use siteone_crawler_gui::app::{App, AppDeps, CrawlerProbe, FileDialogs, FileRequest};
use siteone_crawler_gui::model::paths::AppPaths;
use siteone_crawler_gui::model::schema::OptionSchema;
use siteone_crawler_gui::model::secrets::MemoryStore;
use siteone_crawler_gui::model::settings::{Settings, Theme};

pub const LARGE: egui::Vec2 = egui::vec2(1280.0, 820.0);
pub const SMALL: egui::Vec2 = egui::vec2(1000.0, 640.0);

/// File dialogs that answer from a queue (and record what they were asked).
#[derive(Default)]
pub struct ScriptedDialogs {
    pub answers: RefCell<VecDeque<Option<PathBuf>>>,
    pub asked: RefCell<Vec<String>>,
}

impl ScriptedDialogs {
    fn next(&self, request: &FileRequest) -> Option<PathBuf> {
        self.asked.borrow_mut().push(request.title.clone());
        self.answers.borrow_mut().pop_front().flatten()
    }
}

impl FileDialogs for ScriptedDialogs {
    fn open_file(&self, request: &FileRequest) -> Option<PathBuf> {
        self.next(request)
    }

    fn save_file(&self, request: &FileRequest) -> Option<PathBuf> {
        self.next(request)
    }

    fn pick_folder(&self, request: &FileRequest) -> Option<PathBuf> {
        self.next(request)
    }
}

/// A test environment: a temporary data folder that lives as long as this value.
pub struct Env {
    pub dir: tempfile::TempDir,
}

impl Env {
    pub fn new() -> Self {
        Self {
            dir: tempfile::Builder::new()
                .prefix("siteone-gui-ui-")
                .tempdir()
                .expect("temp dir"),
        }
    }

    pub fn paths(&self) -> AppPaths {
        AppPaths::at(self.dir.path())
    }

    /// A crawler path that is "found" (the file does not exist, so starting fails like a spawn
    /// error would).
    pub fn fake_crawler(&self) -> PathBuf {
        self.dir.path().join("bin").join("siteone-crawler.exe")
    }

    /// An app with a found crawler.
    pub fn app(&self) -> App {
        self.app_with(Some(self.fake_crawler()), Vec::new())
    }

    /// An app with the given crawler (or none) and scripted dialog answers.
    pub fn app_with(&self, crawler: Option<PathBuf>, answers: Vec<Option<PathBuf>>) -> App {
        let paths = self.paths();
        paths.ensure().expect("data folder");
        let dialogs = ScriptedDialogs::default();
        dialogs.answers.borrow_mut().extend(answers);
        let mut app = App::new(AppDeps {
            paths,
            schema: OptionSchema::load(),
            settings: Settings::default(),
            notices: Vec::new(),
            secret_store: Box::new(MemoryStore::default()),
            dialogs: Box::new(dialogs),
            crawler_probe: CrawlerProbe::Fixed {
                path: crawler,
                version: Some(siteone_crawler::version::CODE.to_string()),
            },
        });
        // The scripted runs name real sites (crawler.siteone.io): no request may reach them. Tests
        // with a local site turn it on.
        app.fetch_link_texts = false;
        app
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.paths().default_runs_dir()
    }
}

/// A harness that draws `app` at `size` in `theme`.
///
/// The app's theme setting is set to `theme` (with "System", egui's default theme would apply).
/// The app gets the whole window, as in eframe (the harness itself keeps an 8 px margin).
pub fn harness(mut app: App, size: egui::Vec2, theme: egui::Theme) -> Harness<'static, App> {
    app.settings.theme = match theme {
        egui::Theme::Light => Theme::Light,
        egui::Theme::Dark => Theme::Dark,
    };
    Harness::builder()
        .with_size(size)
        .with_theme(theme)
        .with_max_steps(24)
        .with_context_setup(siteone_crawler_gui::theme::install_fonts)
        .build_ui_state(
            |ui, app: &mut App| {
                let window = ui.ctx().content_rect();
                let mut root = ui.new_child(egui::UiBuilder::new().max_rect(window));
                app.show(&mut root);
            },
            app,
        )
}

/// Runs frames until the UI settles, with background tasks completed in between.
pub fn settle(harness: &mut Harness<'_, App>) {
    harness.run();
    for _ in 0..3 {
        if harness.state().pending_tasks() == 0 {
            break;
        }
        assert!(
            harness.state_mut().wait_for_background(Duration::from_secs(20)),
            "background tasks did not finish"
        );
        harness.run();
    }
}

/// Whether a label (or any node) shows text containing `needle`.
pub fn shows_text(harness: &Harness<'_, App>, needle: &str) -> bool {
    use egui_kittest::kittest::Queryable;
    harness
        .query_all_by(|node| {
            node.value().is_some_and(|value| value.contains(needle))
                || node.label().is_some_and(|label| label.contains(needle))
        })
        .next()
        .is_some()
}

/// Serializes everything that creates or drops a wgpu device: drivers do not all survive several
/// devices being created at once from parallel test threads. Hold it for a harness's whole life.
pub fn gpu_lock() -> MutexGuard<'static, ()> {
    static GPU: Mutex<()> = Mutex::new(());
    GPU.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Call with [`gpu_lock`] held.
fn wgpu_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::panic::catch_unwind(|| {
            let _ = egui_kittest::wgpu::WgpuTestRenderer::new();
        })
        .is_ok()
    })
}

pub fn snapshot_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("snapshots")
}

/// Renders the harness and writes `snapshots/<name>.png` after checking that it is a real image.
/// Call with [`gpu_lock`] held.
pub fn snapshot(harness: &mut Harness<'_, App>, name: &str) {
    // The pointer of the last click would cover what it clicked (and show a hover state).
    harness.input_mut().events.push(egui::Event::PointerGone);
    harness.run_ok();
    if !wgpu_available() {
        eprintln!("snapshot {name} skipped: no wgpu adapter is available");
        return;
    }
    let image = match std::panic::catch_unwind(AssertUnwindSafe(|| harness.render())) {
        Ok(Ok(image)) => image,
        Ok(Err(error)) => {
            eprintln!("snapshot {name} skipped: rendering failed ({error})");
            return;
        }
        Err(_) => {
            eprintln!("snapshot {name} skipped: the renderer panicked");
            return;
        }
    };
    let (width, height) = image.dimensions();
    assert!(
        width >= 1000 && height >= 640,
        "{name}: unexpected size {width}x{height}"
    );
    let mut colours = std::collections::HashSet::new();
    for pixel in image.pixels().step_by(97) {
        colours.insert(pixel.0);
        if colours.len() > 64 {
            break;
        }
    }
    assert!(
        colours.len() > 16,
        "{name}: the image looks blank ({} colours)",
        colours.len()
    );
    let dir = snapshot_dir();
    std::fs::create_dir_all(&dir).expect("snapshot dir");
    image.save(dir.join(format!("{name}.png"))).expect("snapshot written");
}
