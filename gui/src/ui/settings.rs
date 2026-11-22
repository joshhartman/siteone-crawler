//! The Settings page (GUI-SPEC §6.5): the crawler binary, storage (runs folder, sizes, clearing the
//! caches), appearance (theme, UI scale) and About.

use std::path::Path;

use eframe::egui::{self, Frame, Margin, RichText, ScrollArea, Ui};
use egui_material_icons::icons;

use crate::app::{Action, App, CrawlerStatus, FileRequest, confirm};
use crate::backend::shell;
use crate::model::history;
use crate::model::paths::CrawlerSource;
use crate::model::settings::{MAX_UI_SCALE, MIN_UI_SCALE, Theme};
use crate::theme;
use crate::ui::widgets::modal::ButtonKind;
use crate::ui::widgets::{self, Tone, format};

const MAX_WIDTH: f32 = 860.0;

/// UI state of the Settings page.
#[derive(Debug, Default)]
pub struct SettingsUi {
    /// Sizes of the runs folder and the caches, once computed.
    pub sizes: Option<StorageSizes>,
    pub computing: bool,
    pub clearing: bool,
    /// The UI scale while its slider is dragged (applied on release).
    scale_draft: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageSizes {
    pub runs: u64,
    pub caches: u64,
}

pub fn show(app: &mut App, ui: &mut Ui) {
    if app.settings_ui.sizes.is_none() && !app.settings_ui.computing {
        compute_sizes(app);
    }
    ScrollArea::vertical()
        .id_salt("settings")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::new().inner_margin(Margin::symmetric(24, 20)).show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(MAX_WIDTH));
                ui.label(theme::page_title("Settings"));
                ui.add_space(theme::GRID * 2.0);
                crawler_card(app, ui);
                ui.add_space(theme::GRID * 2.0);
                storage_card(app, ui);
                ui.add_space(theme::GRID * 2.0);
                appearance_card(app, ui);
                ui.add_space(theme::GRID * 2.0);
                about_card(ui);
            });
        });
}

fn crawler_card(app: &mut App, ui: &mut Ui) {
    let p = theme::palette(ui);
    widgets::card(ui, |ui| {
        widgets::section_heading(
            ui,
            "Crawler",
            Some(
                "The siteone-crawler program the app runs. Searched in this order: the path below, next to this app, PATH.",
            ),
        );
        ui.add_space(theme::GRID * 1.5);
        match app.crawler.clone() {
            CrawlerStatus::Detecting => {
                ui.label(widgets::weak(ui, "Looking for the crawler…"));
            }
            CrawlerStatus::Missing => {
                widgets::inline_message(
                    ui,
                    Tone::Danger,
                    "Not found. Choose siteone-crawler.exe with Browse…, or put it next to this app.",
                );
            }
            CrawlerStatus::Found { path, source, version } => {
                ui.horizontal(|ui| {
                    ui.label(icons::ICON_CHECK_CIRCLE.rich_text().size(18.0).color(p.success));
                    widgets::selectable_mono(ui, &path.display().to_string());
                });
                ui.horizontal_wrapped(|ui| {
                    let from = match source {
                        Some(CrawlerSource::Settings) => "chosen in Settings",
                        Some(CrawlerSource::NextToGui) => "found next to this app",
                        Some(CrawlerSource::Path) => "found on PATH",
                        None => "given",
                    };
                    ui.label(widgets::weak(ui, from));
                    ui.label(widgets::weak(ui, "·"));
                    match &version {
                        Ok(version) => ui.label(RichText::new(format!("version {version}")).color(p.text)),
                        Err(error) => ui.label(widgets::weak(ui, format!("version unknown ({error})"))),
                    };
                });
                if let Some(configured) = &app.settings.crawler_path
                    && source != Some(CrawlerSource::Settings)
                    && source.is_some()
                {
                    widgets::inline_message(
                        ui,
                        Tone::Warning,
                        &format!(
                            "The chosen file {} was not found; using the one above.",
                            configured.display()
                        ),
                    );
                }
                if let Some(found) = app.crawler.version_mismatch() {
                    ui.add_space(4.0);
                    widgets::inline_message(
                        ui,
                        Tone::Warning,
                        &format!(
                            "This crawler ({found}) differs from the version this app was built with ({}). The option list may differ; crawls still start.",
                            siteone_crawler::version::CODE
                        ),
                    );
                }
            }
        }
        ui.add_space(theme::GRID * 1.5);
        ui.horizontal(|ui| {
            if widgets::button(ui, Some(icons::ICON_FOLDER_OPEN), "Browse…").clicked() {
                app.locate_crawler();
            }
            if widgets::button(ui, Some(icons::ICON_REFRESH), "Detect").clicked() {
                app.detect_crawler();
            }
            if app.settings.crawler_path.is_some() && widgets::link_button(ui, "Use automatic detection").clicked() {
                app.settings.crawler_path = None;
                app.detect_crawler();
            }
        });
    });
}

fn storage_card(app: &mut App, ui: &mut Ui) {
    let runs_dir = app.settings.runs_dir(&app.paths);
    widgets::card(ui, |ui| {
        widgets::section_heading(
            ui,
            "Storage",
            Some("Every run keeps its results in a folder of its own."),
        );
        ui.add_space(theme::GRID * 1.5);
        let label = ui.label(theme::semibold("Runs folder"));
        widgets::selectable_mono(ui, &runs_dir.display().to_string()).labelled_by(label.id);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if widgets::button(ui, Some(icons::ICON_FOLDER), "Change…").clicked() {
                let request = FileRequest::new("Choose the runs folder").in_directory(Some(runs_dir.clone()));
                if let Some(dir) = app.dialogs.pick_folder(&request) {
                    app.settings.runs_dir = Some(dir);
                    app.settings_ui.sizes = None;
                }
            }
            if widgets::button(ui, Some(icons::ICON_OPEN_IN_NEW), "Open").clicked() {
                open_folder(app, &runs_dir);
            }
            if app.settings.runs_dir.is_some() && widgets::link_button(ui, "Use the default folder").clicked() {
                app.settings.runs_dir = None;
                app.settings_ui.sizes = None;
            }
        });
        ui.add_space(theme::GRID * 1.5);
        widgets::hairline(ui);
        ui.add_space(theme::GRID * 1.5);
        ui.horizontal(|ui| {
            let text = match app.settings_ui.sizes {
                Some(sizes) => format!(
                    "Runs: {}   ·   Caches: {}",
                    format::bytes(sizes.runs),
                    format::bytes(sizes.caches)
                ),
                None => "Measuring…".to_string(),
            };
            ui.label(text);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let running = app.has_active_run();
                let clear = ui
                    .add_enabled(
                        !running && !app.settings_ui.clearing,
                        egui::Button::new(widgets::with_icon(icons::ICON_DELETE, "Clear caches…")),
                    )
                    .accessible_name("Clear caches")
                    .on_hover_text("Delete the shared HTTP and AI caches; runs are kept")
                    .on_disabled_hover_text(if running {
                        "Not while a crawl is running"
                    } else {
                        "Clearing…"
                    });
                if clear.clicked() {
                    app.open_dialog(confirm(
                        "Clear the caches?",
                        "The shared HTTP and AI caches are deleted. The next crawls download every page again and repeat AI requests (which may cost money). Runs and their reports are kept.",
                        "Clear caches",
                        ButtonKind::Danger,
                        Action::ClearCaches,
                    ));
                }
            });
        });
    });
}

fn appearance_card(app: &mut App, ui: &mut Ui) {
    widgets::card(ui, |ui| {
        widgets::section_heading(ui, "Appearance", None);
        ui.add_space(theme::GRID * 1.5);
        widgets::field_row(ui, "Theme", "Follow Windows, or always light or dark", false, |ui| {
            let mut theme = app.settings.theme;
            if widgets::segmented(
                ui,
                &mut theme,
                &[
                    (Theme::System, "System"),
                    (Theme::Light, "Light"),
                    (Theme::Dark, "Dark"),
                ],
            ) {
                app.settings.theme = theme;
            }
        });
        ui.add_space(theme::GRID);
        widgets::field_row(
            ui,
            "UI scale",
            "Size of all text and controls (80–150 %)",
            false,
            |ui| {
                ui.spacing_mut().slider_width = 240.0;
                let mut percent = app.settings_ui.scale_draft.unwrap_or(app.settings.ui_scale) * 100.0;
                let response = ui
                    .add(
                        egui::Slider::new(&mut percent, MIN_UI_SCALE * 100.0..=MAX_UI_SCALE * 100.0)
                            .step_by(5.0)
                            .fixed_decimals(0)
                            .suffix(" %"),
                    )
                    .accessible_name("UI scale");
                let scale = (percent / 100.0).clamp(MIN_UI_SCALE, MAX_UI_SCALE);
                if response.dragged() {
                    app.settings_ui.scale_draft = Some(scale);
                } else if response.drag_stopped() || response.changed() {
                    app.settings_ui.scale_draft = None;
                    app.settings.ui_scale = scale;
                }
            },
        );
    });
}

fn about_card(ui: &mut Ui) {
    widgets::card(ui, |ui| {
        widgets::section_heading(ui, "About", None);
        ui.add_space(theme::GRID);
        egui::Grid::new("about")
            .num_columns(2)
            .spacing([24.0, 6.0])
            .show(ui, |ui| {
                ui.label(widgets::weak(ui, "App version"));
                ui.label(env!("CARGO_PKG_VERSION"));
                ui.end_row();
                ui.label(widgets::weak(ui, "Built with crawler"));
                ui.label(siteone_crawler::version::CODE);
                ui.end_row();
                ui.label(widgets::weak(ui, "Licence"));
                ui.label("MIT");
                ui.end_row();
            });
        ui.add_space(theme::GRID);
        ui.horizontal(|ui| {
            ui.hyperlink_to("Documentation", "https://crawler.siteone.io/");
            ui.label(widgets::weak(ui, "·"));
            ui.hyperlink_to("Source code", "https://github.com/janreges/siteone-crawler");
        });
    });
}

fn open_folder(app: &mut App, dir: &Path) {
    if let Err(error) = std::fs::create_dir_all(dir).and_then(|()| shell::open_path(dir)) {
        app.toasts.error(format!("The folder could not be opened: {error}"));
    }
}

/// Measures the runs folder and the caches on a worker.
pub fn compute_sizes(app: &mut App) {
    app.settings_ui.computing = true;
    let runs = app.settings.runs_dir(&app.paths);
    let caches = [app.paths.http_cache_dir(), app.paths.ai_cache_dir()];
    app.spawn_task(move || {
        let sizes = StorageSizes {
            runs: history::dir_size(&runs),
            caches: caches.iter().map(|dir| history::dir_size(dir)).sum(),
        };
        Box::new(move |app: &mut App| {
            app.settings_ui.computing = false;
            app.settings_ui.sizes = Some(sizes);
        })
    });
}

/// Deletes the contents of the shared caches (confirmed by the user).
pub fn clear_caches(app: &mut App) {
    if app.has_active_run() {
        app.toasts
            .warning("The caches cannot be cleared while a crawl is running.");
        return;
    }
    app.settings_ui.clearing = true;
    let dirs = [app.paths.http_cache_dir(), app.paths.ai_cache_dir()];
    app.spawn_task(move || {
        let mut failures = Vec::new();
        for dir in &dirs {
            if dir.exists()
                && let Err(error) = std::fs::remove_dir_all(dir)
            {
                failures.push(format!("{}: {error}", dir.display()));
            }
            let _ = std::fs::create_dir_all(dir);
        }
        Box::new(move |app: &mut App| {
            app.settings_ui.clearing = false;
            if failures.is_empty() {
                app.toasts.success("The caches were cleared.");
            } else {
                app.error_dialog(
                    "Some cache files could not be deleted",
                    "Close other programs that might use them and try again.",
                    Some(failures.join("\n")),
                );
            }
            compute_sizes(app);
        })
    });
}
