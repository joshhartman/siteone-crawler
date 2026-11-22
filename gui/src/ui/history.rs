//! The History page (GUI-SPEC §6.4): the runs of the runs folder, newest first — date, URL,
//! outcome, duration, URLs, overall score — with a text filter and the actions Open, Run again,
//! Open folder and Delete (confirmed, naming the folder size). Damaged runs are listed with the
//! reason and can be deleted. The listing and the folder sizes are read on workers; the list is
//! read again when a run ends.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, Align, Frame, Layout, Margin, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;

use crate::app::{Action, App, Page};
use crate::backend::shell;
use crate::model::history::{self, RunRecord};
use crate::theme;
use crate::ui::run::{outcome_style, recorded_style};
use crate::ui::widgets::modal::{ButtonKind, Dialog};
use crate::ui::widgets::table::{self, HEADER_HEIGHT, contains_ignore_case};
use crate::ui::widgets::{self, Tone, format, progress, score};

const ROW_HEIGHT: f32 = 34.0;

/// A run as listed.
#[derive(Debug, Clone)]
pub struct HistoryRow {
    pub dir: PathBuf,
    pub run_id: String,
    /// The record, or why it cannot be read.
    pub record: Result<RunRecord, String>,
}

/// UI state of the History page.
#[derive(Debug, Default)]
pub struct HistoryUi {
    pub rows: Vec<HistoryRow>,
    /// The listing has been read once.
    pub loaded: bool,
    pub loading: bool,
    /// Read the listing again (a run started or ended, a run was deleted).
    pub stale: bool,
    pub filter: String,
    pub selected: Option<PathBuf>,
    /// Folder sizes, once measured.
    pub sizes: HashMap<PathBuf, u64>,
    listed_dir: Option<PathBuf>,
    generation: u64,
}

impl HistoryUi {
    /// The rows that match the filter.
    pub fn visible(&self) -> Vec<&HistoryRow> {
        let needle = self.filter.trim().to_lowercase();
        self.rows
            .iter()
            .filter(|row| {
                needle.is_empty()
                    || contains_ignore_case(&row.run_id, &needle)
                    || match &row.record {
                        Ok(record) => {
                            contains_ignore_case(&record.url, &needle)
                                || record
                                    .outcome
                                    .is_some_and(|outcome| contains_ignore_case(outcome.label(), &needle))
                        }
                        Err(error) => contains_ignore_case(error, &needle),
                    }
            })
            .collect()
    }
}

/// Reads the listing on a worker, then the folder sizes on another.
pub fn refresh(app: &mut App) {
    let runs_dir = app.settings.runs_dir(&app.paths);
    let ui = &mut app.history_ui;
    ui.loading = true;
    ui.stale = false;
    ui.listed_dir = Some(runs_dir.clone());
    ui.generation += 1;
    let generation = ui.generation;
    app.spawn_task(move || {
        let rows: Vec<HistoryRow> = history::list_runs(&runs_dir)
            .into_iter()
            .map(|entry| HistoryRow {
                run_id: entry.run_id(),
                record: entry.record.map_err(|error| error.to_string()),
                dir: entry.dir,
            })
            .collect();
        Box::new(move |app: &mut App| {
            if app.history_ui.generation != generation {
                return;
            }
            let dirs: Vec<PathBuf> = rows.iter().map(|row| row.dir.clone()).collect();
            let ui = &mut app.history_ui;
            ui.loading = false;
            ui.loaded = true;
            if ui.selected.as_ref().is_some_and(|selected| !dirs.contains(selected)) {
                ui.selected = None;
            }
            ui.rows = rows;
            app.spawn_task(move || {
                let sizes: HashMap<PathBuf, u64> = dirs
                    .into_iter()
                    .map(|dir| {
                        let size = history::dir_size(&dir);
                        (dir, size)
                    })
                    .collect();
                Box::new(move |app: &mut App| {
                    if app.history_ui.generation == generation {
                        app.history_ui.sizes = sizes;
                    }
                })
            });
        })
    });
}

/// What the user asked for on the page.
enum HistoryAction {
    Open(PathBuf),
    RunAgain(Box<RunRecord>),
    OpenFolder(PathBuf),
    Delete(PathBuf),
    Refresh,
    Setup,
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let runs_dir = app.settings.runs_dir(&app.paths);
    let ui_state = &app.history_ui;
    if !ui_state.loading && (!ui_state.loaded || ui_state.stale || ui_state.listed_dir.as_ref() != Some(&runs_dir)) {
        refresh(app);
    }
    let focus = std::mem::take(&mut app.focus_search);
    let active_dir = app
        .current_run
        .as_ref()
        .filter(|run| run.is_active())
        .map(|run| run.plan.run_dir.clone());
    let mut actions = Vec::new();
    let p = theme::palette(ui);
    Frame::new()
        .inner_margin(Margin {
            left: 24,
            right: 24,
            top: 20,
            bottom: 16,
        })
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let history = &mut app.history_ui;
            ui.horizontal(|ui| {
                ui.label(theme::page_title("History"));
                if history.loaded {
                    ui.label(widgets::weak(ui, format::plural(history.rows.len() as u64, "run")));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::icon_button(ui, icons::ICON_REFRESH, "Refresh the list").clicked() {
                        actions.push(HistoryAction::Refresh);
                    }
                    let response = ui
                        .add(
                            egui::TextEdit::singleline(&mut history.filter)
                                .hint_text("Filter runs   (Ctrl+F)")
                                .desired_width(260.0)
                                .margin(egui::vec2(8.0, 5.0)),
                        )
                        .accessible_name("Filter runs");
                    if focus {
                        response.request_focus();
                    }
                    ui.label(icons::ICON_SEARCH.rich_text().size(18.0).color(p.text_weak));
                });
            });
            ui.label(widgets::weak(ui, format!("Runs folder: {}", runs_dir.display())).size(12.5));
            ui.add_space(theme::GRID * 1.5);

            if !history.loaded {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    progress::spinner(ui, 28.0, p.accent);
                    ui.add_space(theme::GRID);
                    ui.label(widgets::weak(ui, "Reading the runs folder…"));
                });
                return;
            }
            if history.rows.is_empty() {
                if widgets::empty_state(
                    ui,
                    icons::ICON_HISTORY,
                    "No runs yet",
                    "Every crawl you start is listed here, newest first. Each keeps its reports and exports in a folder of its own.",
                    Some("Go to Setup"),
                ) {
                    actions.push(HistoryAction::Setup);
                }
                return;
            }
            toolbar(ui, history, active_dir.as_deref(), &mut actions);
            ui.add_space(theme::GRID);
            let visible: Vec<HistoryRow> = history.visible().into_iter().cloned().collect();
            if visible.is_empty() {
                if widgets::empty_state(
                    ui,
                    icons::ICON_FILTER_ALT_OFF,
                    "No run matches the filter",
                    "Change the filter, or clear it to see every run.",
                    Some("Clear filter"),
                ) {
                    history.filter.clear();
                }
                return;
            }
            runs_table(ui, history, &visible, active_dir.as_deref(), &mut actions);
        });
    for action in actions {
        match action {
            HistoryAction::Open(dir) => app.open_past_run(dir),
            HistoryAction::RunAgain(record) => app.run_again_record(&record),
            HistoryAction::OpenFolder(dir) => {
                if let Err(error) = shell::open_path(&dir) {
                    app.toasts.error(format!("The folder could not be opened: {error}"));
                }
            }
            HistoryAction::Delete(dir) => request_delete(app, dir),
            HistoryAction::Refresh => refresh(app),
            HistoryAction::Setup => app.page = Page::Setup,
        }
    }
}

fn selected_row(history: &HistoryUi) -> Option<&HistoryRow> {
    let selected = history.selected.as_ref()?;
    history.rows.iter().find(|row| row.dir == *selected)
}

fn toolbar(ui: &mut Ui, history: &HistoryUi, active_dir: Option<&Path>, actions: &mut Vec<HistoryAction>) {
    let p = theme::palette(ui);
    let row = selected_row(history);
    let record = row.and_then(|row| row.record.as_ref().ok());
    let active = row.is_some_and(|row| active_dir == Some(row.dir.as_path()));
    ui.horizontal(|ui| {
        let open = ui
            .add_enabled(
                record.is_some(),
                egui::Button::new(widgets::with_icon(icons::ICON_VISIBILITY, "Open")),
            )
            .accessible_name("Open")
            .on_hover_text("Show the run on the Run page (or double-click it)")
            .on_disabled_hover_text("Select a run that can be read");
        if open.clicked()
            && let Some(row) = row
        {
            actions.push(HistoryAction::Open(row.dir.clone()));
        }
        let again = ui
            .add_enabled(
                record.is_some(),
                egui::Button::new(widgets::with_icon(icons::ICON_REPLAY, "Run again")),
            )
            .accessible_name("Run again")
            .on_hover_text("Load the run's configuration into Setup")
            .on_disabled_hover_text("Select a run that can be read");
        if again.clicked()
            && let Some(record) = record
        {
            actions.push(HistoryAction::RunAgain(Box::new(record.clone())));
        }
        let folder = ui
            .add_enabled(
                row.is_some(),
                egui::Button::new(widgets::with_icon(icons::ICON_FOLDER_OPEN, "Open folder")),
            )
            .accessible_name("Open folder")
            .on_disabled_hover_text("Select a run");
        if folder.clicked()
            && let Some(row) = row
        {
            actions.push(HistoryAction::OpenFolder(row.dir.clone()));
        }
        let delete = ui
            .add_enabled(
                row.is_some() && !active,
                egui::Button::new((
                    icons::ICON_DELETE.rich_text().size(16.0).color(p.danger),
                    RichText::new("Delete…").color(p.danger),
                )),
            )
            .accessible_name("Delete")
            .on_disabled_hover_text(if active {
                "Not while this run is going"
            } else {
                "Select a run"
            });
        if delete.clicked()
            && let Some(row) = row
        {
            actions.push(HistoryAction::Delete(row.dir.clone()));
        }
        if row.is_none() {
            ui.label(widgets::weak(ui, "Select a run for these actions."));
        }
    });
}

fn started_text(record: &RunRecord) -> String {
    chrono::DateTime::parse_from_rfc3339(&record.started_at)
        .map(|time| time.with_timezone(&chrono::Local).format("%d %b %Y, %H:%M").to_string())
        .unwrap_or_else(|_| record.started_at.clone())
}

fn runs_table(
    ui: &mut Ui,
    history: &mut HistoryUi,
    rows: &[HistoryRow],
    active_dir: Option<&Path>,
    actions: &mut Vec<HistoryAction>,
) {
    let p = theme::palette(ui);
    let mut clicked = None;
    Frame::new()
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(theme::CARD_RADIUS)
        .inner_margin(Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            // A click on a row's text selects the row, not the text.
            ui.style_mut().interaction.selectable_labels = false;
            TableBuilder::new(ui)
                .id_salt("history-table")
                .striped(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(150.0))
                .column(Column::remainder().at_least(180.0).clip(true))
                .column(Column::exact(176.0))
                .column(Column::exact(96.0))
                .column(Column::exact(76.0))
                .column(Column::exact(96.0))
                .min_scrolled_height(0.0)
                .auto_shrink([false, false])
                .header(HEADER_HEIGHT, |mut header| {
                    for label in ["Started", "URL", "Outcome", "Duration", "URLs", "Score"] {
                        header.col(|ui| table::header_label(ui, label));
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                        let Some(item) = rows.get(row.index()) else {
                            return;
                        };
                        row.set_selected(history.selected.as_ref() == Some(&item.dir));
                        let active = active_dir == Some(item.dir.as_path());
                        match &item.record {
                            Ok(record) => {
                                row.col(|ui| {
                                    ui.label(RichText::new(started_text(record)).color(p.text));
                                });
                                row.col(|ui| {
                                    table::cell_text(ui, RichText::new(&record.url).color(p.text));
                                });
                                row.col(|ui| {
                                    if active {
                                        let (tone, icon) = outcome_style(&crate::model::run_state::RunOutcome::Running);
                                        widgets::icon_chip(ui, icon, "Running", tone, true);
                                    } else {
                                        match record.outcome {
                                            Some(outcome) => {
                                                let (tone, icon) = recorded_style(outcome);
                                                widgets::icon_chip(ui, icon, outcome.label(), tone, false);
                                            }
                                            None => {
                                                widgets::icon_chip(
                                                    ui,
                                                    icons::ICON_HOURGLASS_DISABLED,
                                                    "Unfinished",
                                                    Tone::Warning,
                                                    false,
                                                )
                                                .on_hover_text("The app closed while this run was going.");
                                            }
                                        }
                                    }
                                });
                                row.col(|ui| {
                                    let text = record.duration_ms.map_or("—".to_string(), format::millis);
                                    ui.label(RichText::new(text).color(p.text));
                                });
                                row.col(|ui| {
                                    ui.label(RichText::new(format::count(record.summary.urls)).color(p.text));
                                });
                                row.col(|ui| match record.summary.overall_score {
                                    Some(value) => {
                                        let tone = score::band_tone(value);
                                        ui.label(icons::ICON_CIRCLE.rich_text().size(10.0).color(tone.color(p)));
                                        ui.label(theme::semibold(score::score_text(value)).color(p.text))
                                            .on_hover_text(record.summary.overall_label.clone().unwrap_or_default());
                                    }
                                    None => {
                                        ui.label(widgets::weak(ui, "—"));
                                    }
                                });
                            }
                            Err(error) => {
                                row.col(|ui| {
                                    table::cell_text(ui, RichText::new(&item.run_id).color(p.text_weak));
                                });
                                row.col(|ui| {
                                    table::cell_text(
                                        ui,
                                        RichText::new(format!("This run folder is damaged: {error}")).color(p.danger),
                                    );
                                });
                                row.col(|ui| {
                                    widgets::icon_chip(ui, icons::ICON_BROKEN_IMAGE, "Damaged", Tone::Danger, false);
                                });
                                for _ in 0..3 {
                                    row.col(|ui| {
                                        ui.label(widgets::weak(ui, "—"));
                                    });
                                }
                            }
                        }
                        let response = row.response();
                        if response.clicked() {
                            clicked = Some((item.dir.clone(), false));
                        }
                        if response.double_clicked() && item.record.is_ok() {
                            clicked = Some((item.dir.clone(), true));
                        }
                        response.context_menu(|ui| {
                            if item.record.is_ok()
                                && widgets::button(ui, Some(icons::ICON_VISIBILITY), "Open").clicked()
                            {
                                actions.push(HistoryAction::Open(item.dir.clone()));
                                ui.close();
                            }
                            if let Ok(record) = &item.record
                                && widgets::button(ui, Some(icons::ICON_REPLAY), "Run again").clicked()
                            {
                                actions.push(HistoryAction::RunAgain(Box::new(record.clone())));
                                ui.close();
                            }
                            if widgets::button(ui, Some(icons::ICON_FOLDER_OPEN), "Open folder").clicked() {
                                actions.push(HistoryAction::OpenFolder(item.dir.clone()));
                                ui.close();
                            }
                            if !active && widgets::danger_button(ui, "Delete…").clicked() {
                                actions.push(HistoryAction::Delete(item.dir.clone()));
                                ui.close();
                            }
                        });
                    });
                });
        });
    if let Some((dir, open)) = clicked {
        history.selected = Some(dir.clone());
        if open {
            actions.push(HistoryAction::Open(dir));
        }
    }
}

/// Asks before deleting a run folder, naming its size (measured on a worker when not known yet).
pub fn request_delete(app: &mut App, dir: PathBuf) {
    match app.history_ui.sizes.get(&dir).copied() {
        Some(size) => confirm_delete(app, dir, size),
        None => app.spawn_task(move || {
            let size = history::dir_size(&dir);
            Box::new(move |app: &mut App| confirm_delete(app, dir, size))
        }),
    }
}

fn confirm_delete(app: &mut App, dir: PathBuf, size: u64) {
    let name = dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| dir.display().to_string());
    app.open_dialog(
        Dialog::new("Delete this run?")
            .tone(Tone::Warning)
            .message(format!(
                "The folder {name} ({}) and everything in it — reports, exports, console log — are deleted. This cannot be undone.",
                format::bytes(size)
            ))
            .button("Delete run", ButtonKind::Danger, Action::DeleteRun(dir))
            .cancel("Cancel"),
    );
}

/// Deletes a run folder (confirmed) on a worker; only a run folder of the runs folder.
pub fn delete(app: &mut App, dir: PathBuf) {
    if app
        .current_run
        .as_ref()
        .is_some_and(|run| run.is_active() && run.plan.run_dir == dir)
    {
        app.toasts.warning("A run cannot be deleted while it is going.");
        return;
    }
    // A check of the run's external links ends with it (nothing more is written into the folder).
    app.discard_link_check(&dir);
    app.discard_text_job(&dir);
    app.discard_retry(&dir);
    let runs_dir = app.settings.runs_dir(&app.paths);
    app.spawn_task(move || {
        let result = history::delete_run(&runs_dir, &dir);
        Box::new(move |app: &mut App| {
            match result {
                Ok(()) => {
                    app.toasts.success("The run was deleted.");
                    if app.history_ui.selected.as_ref() == Some(&dir) {
                        app.history_ui.selected = None;
                    }
                    if app.past_run.as_ref().is_some_and(|past| past.run_dir == dir) {
                        app.show_current_run();
                        app.page = Page::History;
                    }
                    if app.current_run.as_ref().is_some_and(|run| run.plan.run_dir == dir) {
                        app.current_run = None;
                    }
                }
                Err(error) => app.error_dialog(
                    "The run could not be deleted",
                    "Close programs that may use files of the run (a browser showing its report, Explorer) and try again.",
                    Some(error.to_string()),
                ),
            }
            refresh(app);
        })
    });
}
