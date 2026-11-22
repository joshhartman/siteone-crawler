//! The Files tab: the artifacts the run announced, with Open and Show in folder. Whether each file
//! still exists is checked on a worker when the list changes; missing files are marked. Below them
//! the file the app writes itself, the link report (GUI-SPEC §4.10): Open and Show in folder write
//! it from the run's state first.

use std::path::PathBuf;

use eframe::egui::{self, Align, Layout, Ui};
use egui_material_icons::{MaterialIcon, icons};

use super::{RunAction, RunView, padded_scroll};
use crate::app::App;
use crate::theme;
use crate::ui::widgets::table::middle_truncate;
use crate::ui::widgets::{self, Tone};

/// UI state of the Files tab.
#[derive(Debug, Default)]
pub struct FilesUi {
    /// What the existence check was made for: (run folder, number of artifacts).
    checked_for: Option<(PathBuf, usize)>,
    /// Per artifact: `Some(exists)`, `None` for a URL or while unknown.
    pub exists: Vec<Option<bool>>,
}

fn icon(kind: &str) -> MaterialIcon {
    match kind {
        "html" => icons::ICON_WEB,
        "json" => icons::ICON_DATA_OBJECT,
        "text" => icons::ICON_DESCRIPTION,
        "offline" => icons::ICON_FOLDER_ZIP,
        "markdown" => icons::ICON_ARTICLE,
        "online" => icons::ICON_CLOUD,
        kind if kind.contains("sitemap") => icons::ICON_ACCOUNT_TREE,
        kind if kind.starts_with("ai") => icons::ICON_AUTO_AWESOME,
        kind if kind.contains("screenshot") || kind.contains("animation") => icons::ICON_IMAGE,
        _ => icons::ICON_DRAFT,
    }
}

pub fn show(ui: &mut Ui, view: &RunView<'_>, files: &mut FilesUi, actions: &mut Vec<RunAction>) {
    let artifacts = &view.state.artifacts;
    let key = (view.run_dir.to_path_buf(), artifacts.len());
    if files.checked_for.as_ref() != Some(&key) {
        files.checked_for = Some(key);
        files.exists = vec![None; artifacts.len()];
        actions.push(RunAction::CheckFiles {
            run_dir: view.run_dir.to_path_buf(),
            paths: artifacts.iter().map(|artifact| artifact.path.clone()).collect(),
        });
    }
    if artifacts.is_empty() && view.live {
        widgets::empty_state(
            ui,
            icons::ICON_FOLDER_OPEN,
            if view.live {
                "No files yet"
            } else {
                "This run wrote no files"
            },
            if view.live {
                "The reports and exports appear here when the crawler writes them, mostly at the end of the run."
            } else {
                "The crawler announced no output file. The run folder still has the console log and the events."
            },
            None,
        );
        return;
    }
    let p = theme::palette(ui);
    padded_scroll(ui, "run-files", |ui| {
        if super::overview::outputs_notice(ui, view, actions) {
            ui.add_space(theme::GRID * 1.5);
        }
        if artifacts.is_empty() {
            ui.label(widgets::weak(
                ui,
                "The crawler announced no output file. The run folder still has the console log and the events.",
            ));
        } else {
            artifacts_card(ui, view, files, actions);
        }
        if !view.live {
            ui.add_space(theme::GRID * 2.0);
            ui.label(theme::section_title("Written by the app"));
            ui.add_space(theme::GRID);
            widgets::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(icons::ICON_SUMMARIZE.rich_text().size(22.0).color(p.accent));
                    let text_width = (ui.available_width() - 260.0).max(160.0);
                    ui.allocate_ui_with_layout(egui::vec2(text_width, 40.0), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(text_width);
                        ui.label(theme::semibold("Link report").color(p.text)).on_hover_text(
                            "Broken, refused and working links with their link texts, written from this run's state",
                        );
                        let path = view
                            .run_dir
                            .join(crate::model::link_report::REPORT_FILE)
                            .display()
                            .to_string();
                        let shown = middle_truncate(ui, &path, &egui::FontId::monospace(12.0), text_width);
                        let response = ui.label(theme::mono(&shown).size(12.0).color(p.text_weak));
                        if shown != path {
                            response.on_hover_text(path);
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let reveal = ui
                            .add(egui::Button::new(widgets::with_icon(
                                icons::ICON_FOLDER_OPEN,
                                "Show in folder",
                            )))
                            .accessible_name("Show Link report in folder")
                            .on_hover_text("Write the link report and show it selected in Explorer");
                        if reveal.clicked() {
                            actions.push(RunAction::OpenLinkReport(view.run_dir.to_path_buf(), true));
                        }
                        let open = ui
                            .add(egui::Button::new(widgets::with_icon(icons::ICON_OPEN_IN_NEW, "Open")))
                            .accessible_name("Open Link report")
                            .on_hover_text("Write the link report and open it in the browser");
                        if open.clicked() {
                            actions.push(RunAction::OpenLinkReport(view.run_dir.to_path_buf(), false));
                        }
                    });
                });
            });
        }
    });
}

/// The artifacts the crawler announced.
fn artifacts_card(ui: &mut Ui, view: &RunView<'_>, files: &FilesUi, actions: &mut Vec<RunAction>) {
    let p = theme::palette(ui);
    let artifacts = &view.state.artifacts;
    widgets::card(ui, |ui| {
        for (index, artifact) in artifacts.iter().enumerate() {
            if index > 0 {
                ui.add_space(6.0);
                widgets::hairline(ui);
                ui.add_space(6.0);
            }
            let exists = files.exists.get(index).copied().flatten();
            let missing = exists == Some(false);
            let label = if artifact.label.is_empty() {
                artifact.kind.as_str()
            } else {
                artifact.label.as_str()
            };
            ui.horizontal(|ui| {
                ui.label(icon(&artifact.kind).rich_text().size(22.0).color(if missing {
                    p.text_weak
                } else {
                    p.accent
                }));
                let buttons_width = 260.0;
                let text_width = (ui.available_width() - buttons_width).max(160.0);
                ui.allocate_ui_with_layout(egui::vec2(text_width, 40.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(text_width);
                    ui.horizontal(|ui| {
                        ui.label(theme::semibold(label).color(if missing { p.text_weak } else { p.text }));
                        if missing {
                            widgets::chip(ui, "Missing", Tone::Danger)
                                .on_hover_text("The file is no longer where the crawler wrote it (moved or deleted).");
                        }
                    });
                    let path = artifact
                        .path
                        .as_ref()
                        .map_or_else(|| artifact.raw.clone(), |path| path.display().to_string());
                    let font = egui::FontId::monospace(12.0);
                    let shown = middle_truncate(ui, &path, &font, text_width);
                    let response = ui.label(theme::mono(&shown).size(12.0).color(p.text_weak));
                    if shown != path {
                        response.on_hover_text(path);
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| match &artifact.path {
                    Some(path) => {
                        let reveal = ui
                            .add_enabled(
                                !missing,
                                egui::Button::new(widgets::with_icon(icons::ICON_FOLDER_OPEN, "Show in folder")),
                            )
                            .accessible_name(format!("Show {label} in folder"))
                            .on_hover_text("Show the file selected in Explorer");
                        if reveal.clicked() {
                            actions.push(RunAction::RevealPath(path.clone()));
                        }
                        let open = ui
                            .add_enabled(
                                !missing,
                                egui::Button::new(widgets::with_icon(icons::ICON_OPEN_IN_NEW, "Open")),
                            )
                            .accessible_name(format!("Open {label}"))
                            .on_hover_text("Open with the default program");
                        if open.clicked() {
                            actions.push(RunAction::OpenPath(path.clone()));
                        }
                    }
                    None => {
                        let open = ui
                            .add(egui::Button::new(widgets::with_icon(icons::ICON_OPEN_IN_NEW, "Open")))
                            .accessible_name(format!("Open {label}"))
                            .on_hover_text("Open in the browser");
                        if open.clicked() {
                            actions.push(RunAction::OpenUrl(artifact.raw.clone()));
                        }
                    }
                });
            });
        }
    });
}

/// Checks on a worker which artifact files exist.
pub fn check(app: &mut App, run_dir: PathBuf, paths: Vec<Option<PathBuf>>) {
    app.spawn_task(move || {
        let exists: Vec<Option<bool>> = paths
            .iter()
            .map(|path| path.as_ref().map(|path| path.exists()))
            .collect();
        Box::new(move |app: &mut App| {
            let files = &mut app.run_ui.files;
            if files
                .checked_for
                .as_ref()
                .is_some_and(|(dir, count)| *dir == run_dir && *count == exists.len())
            {
                files.exists = exists;
            }
        })
    });
}
