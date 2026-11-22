//! The Tables tab: every analysis table of the result JSON — a list (title, row count) and a
//! virtualized grid with the table's own columns, a row filter, tooltips for cut cells and
//! click-to-copy.

use eframe::egui::{self, Align, Frame, Layout, Margin, Panel, RichText, Role, Sense, Stroke, Ui, WidgetInfo};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;

use super::{PAGE_MARGIN, RunAction, RunView, padded, result_unavailable};
use crate::model::results::ResultTable;
use crate::theme;
use crate::ui::widgets::table::{self, HEADER_HEIGHT, ROW_HEIGHT, contains_ignore_case};
use crate::ui::widgets::{self, format, one_line};

const LIST_WIDTH: f32 = 270.0;

/// UI state of the Tables tab.
#[derive(Debug, Default)]
pub struct TablesUi {
    /// The key of the selected table.
    pub selected: Option<String>,
    pub filter: String,
    /// The rows of the selected table that match the filter, and what they were computed for.
    rows: Vec<u32>,
    rows_for: Option<(String, String)>,
}

impl TablesUi {
    fn sync(&mut self, table: &ResultTable) {
        let key = (table.key.clone(), self.filter.clone());
        if self.rows_for.as_ref() == Some(&key) {
            return;
        }
        let needle = self.filter.trim().to_lowercase();
        self.rows = table
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| needle.is_empty() || row.iter().any(|cell| contains_ignore_case(cell, &needle)))
            .filter_map(|(index, _)| u32::try_from(index).ok())
            .collect();
        self.rows_for = Some(key);
    }

    /// The rows of the selected table shown now (tests).
    pub fn shown_rows(&self) -> &[u32] {
        &self.rows
    }
}

pub fn show(ui: &mut Ui, view: &RunView<'_>, tables: &mut TablesUi, focus: bool, actions: &mut Vec<RunAction>) {
    if result_unavailable(ui, view, icons::ICON_TABLE_CHART, "Tables") {
        return;
    }
    let Some(data) = view.data() else {
        return;
    };
    if data.tables.is_empty() {
        widgets::empty_state(
            ui,
            icons::ICON_TABLE_CHART,
            "No analysis tables",
            "The result file of this run has no tables.",
            None,
        );
        return;
    }
    let p = theme::palette(ui);
    if tables.selected.as_ref().is_none_or(|key| data.table(key).is_none()) {
        tables.selected = data.tables.first().map(|table| table.key.clone());
        tables.filter.clear();
    }
    Panel::left("run-tables-list")
        .exact_size(LIST_WIDTH + f32::from(PAGE_MARGIN))
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new().fill(p.bg).inner_margin(Margin {
            left: PAGE_MARGIN,
            right: 0,
            top: 12,
            bottom: 16,
        }))
        .show(ui, |ui| {
            Frame::new()
                .fill(p.surface)
                .stroke(Stroke::new(1.0, p.border))
                .corner_radius(theme::CARD_RADIUS)
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    egui::ScrollArea::vertical()
                        .id_salt("run-tables-list-scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for table in &data.tables {
                                let selected = tables.selected.as_deref() == Some(table.key.as_str());
                                if list_item(ui, table, selected).clicked() && !selected {
                                    tables.selected = Some(table.key.clone());
                                    tables.filter.clear();
                                }
                            }
                        });
                });
        });
    let Some(table) = tables.selected.as_deref().and_then(|key| data.table(key)) else {
        return;
    };
    egui::CentralPanel::default()
        .frame(Frame::new().fill(p.bg))
        .show(ui, |ui| {
            padded(ui, |ui| {
                tables.sync(table);
                ui.horizontal(|ui| {
                    ui.label(theme::section_title(title(table)).color(p.text));
                    widgets::chip(
                        ui,
                        format::plural(table.rows.len() as u64, "row"),
                        widgets::Tone::Neutral,
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let response = ui
                            .add(
                                egui::TextEdit::singleline(&mut tables.filter)
                                    .hint_text("Filter rows   (Ctrl+F)")
                                    .desired_width(240.0)
                                    .margin(egui::vec2(8.0, 5.0)),
                            )
                            .accessible_name("Filter rows");
                        if focus {
                            response.request_focus();
                        }
                        ui.label(icons::ICON_SEARCH.rich_text().size(18.0).color(p.text_weak));
                        if !tables.filter.trim().is_empty() {
                            ui.label(widgets::weak(
                                ui,
                                format!(
                                    "{} of {}",
                                    format::count(tables.rows.len() as u64),
                                    format::count(table.rows.len() as u64)
                                ),
                            ));
                        }
                    });
                });
                ui.add_space(theme::GRID);
                tables.sync(table);
                if table.columns.is_empty() || table.rows.is_empty() {
                    widgets::empty_state(
                        ui,
                        icons::ICON_TABLE_ROWS,
                        "This table is empty",
                        "The analysis found nothing to list here.",
                        None,
                    );
                    return;
                }
                if tables.rows.is_empty() {
                    if widgets::empty_state(
                        ui,
                        icons::ICON_FILTER_ALT_OFF,
                        "No row matches the filter",
                        "Change the filter, or clear it to see every row.",
                        Some("Clear filter"),
                    ) {
                        tables.filter.clear();
                    }
                    return;
                }
                grid(ui, table, &tables.rows, actions);
                ui.add_space(4.0);
            });
        });
}

fn title(table: &ResultTable) -> &str {
    if table.title.is_empty() {
        &table.key
    } else {
        &table.title
    }
}

fn list_item(ui: &mut Ui, table: &ResultTable, selected: bool) -> egui::Response {
    let p = theme::palette(ui);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 30.0), Sense::click());
    let name = title(table).to_string();
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, &name));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, theme::RADIUS, p.accent_soft);
        } else if response.hovered() {
            painter.rect_filled(rect, theme::RADIUS, p.surface_alt);
        }
        let count = ui.painter().layout_no_wrap(
            format::count(table.rows.len() as u64),
            egui::FontId::proportional(12.0),
            p.text_weak,
        );
        let count_width = count.size().x;
        painter.galley(
            egui::pos2(rect.right() - 8.0 - count_width, rect.center().y - count.size().y / 2.0),
            count,
            p.text_weak,
        );
        let color = if selected { p.accent } else { p.text };
        let font = if selected {
            theme::semibold_font(13.5)
        } else {
            egui::FontId::proportional(13.5)
        };
        let label = one_line(ui, &name, font, color, rect.width() - count_width - 28.0);
        painter.galley(
            egui::pos2(rect.left() + 8.0, rect.center().y - label.size().y / 2.0),
            label,
            color,
        );
    }
    response
}

fn grid(ui: &mut Ui, table: &ResultTable, rows: &[u32], actions: &mut Vec<RunAction>) {
    let p = theme::palette(ui);
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(theme::CARD_RADIUS)
        .inner_margin(Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let last = table.columns.len().saturating_sub(1);
            let wanted: Vec<f32> = table
                .columns
                .iter()
                .map(|column| {
                    let chars = if column.width > 0 {
                        column.width.clamp(6, 60)
                    } else {
                        16
                    } as f32;
                    chars * 7.0 + 16.0
                })
                .collect();
            // Columns share the room: wide ones shrink first, none below 56 points.
            let room = ui.available_width() - 8.0 * wanted.len() as f32;
            let total: f32 = wanted.iter().sum();
            let scale = if total > room && total > 0.0 { room / total } else { 1.0 };
            let mut builder = TableBuilder::new(ui)
                .id_salt(("run-table-grid", table.key.as_str()))
                .striped(true)
                .cell_layout(Layout::left_to_right(Align::Center))
                .min_scrolled_height(0.0)
                .auto_shrink([false, false]);
            for (index, wanted) in wanted.iter().enumerate() {
                let width = (wanted * scale).max(56.0);
                builder = builder.column(if index == last {
                    Column::remainder().at_least(width.min(160.0)).clip(true)
                } else {
                    Column::initial(width).at_least(56.0).clip(true).resizable(true)
                });
            }
            builder
                .header(HEADER_HEIGHT, |mut header| {
                    for column in &table.columns {
                        header.col(|ui| table::header_label(ui, &column.name));
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                        let Some(cells) = rows.get(row.index()).and_then(|index| table.rows.get(*index as usize))
                        else {
                            return;
                        };
                        for (column, cell) in table.columns.iter().zip(cells) {
                            row.col(|ui| {
                                let response = table::cell_text_clickable(ui, RichText::new(cell).color(p.text));
                                if response.clicked() && !cell.is_empty() {
                                    actions.push(RunAction::Copy {
                                        text: cell.clone(),
                                        what: format!("the {} value", column.name),
                                    });
                                }
                            });
                        }
                    });
                });
        });
}
