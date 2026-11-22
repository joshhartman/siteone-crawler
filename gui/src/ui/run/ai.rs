//! The AI tab: usage tiles, the progress of the AI tasks and the request log.

use eframe::egui::{self, Align, Frame, Layout, Margin, RichText, Stroke, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;

use super::{RunView, padded_scroll};
use crate::model::run_state::AiTaskState;
use crate::theme;
use crate::ui::widgets::stat::{Stat, tile_row};
use crate::ui::widgets::table::{self, HEADER_HEIGHT, ROW_HEIGHT};
use crate::ui::widgets::{self, Tone, format, progress};

pub fn show(ui: &mut Ui, view: &RunView<'_>) {
    let p = theme::palette(ui);
    let state = view.state;
    padded_scroll(ui, "run-ai", |ui| {
        match &state.ai_usage {
            Some(usage) => {
                let mut tiles = vec![
                    Stat::new("Calls", format::count(usage.calls))
                        .detail(format!("{} HTTP attempts", format::count(usage.http_attempts))),
                    Stat::new("Input tokens", format::count(usage.input_tokens))
                        .detail(format!("{} cached", format::count(usage.cached_input_tokens))),
                    Stat::new("Output tokens", format::count(usage.output_tokens)),
                    Stat::new("Reasoning tokens", format::count(usage.reasoning_tokens)),
                    Stat::new("Cache hits", format::count(usage.cache_hits)),
                    Stat::new("Retries", format::count(usage.retries)),
                    Stat::new("Network time", format::millis(usage.network_ms)),
                ];
                if usage.retries > 0
                    && let Some(tile) = tiles.get_mut(5)
                {
                    tile.tone = Some(Tone::Warning);
                }
                tile_row(ui, &tiles);
                if !usage.model.is_empty() || !usage.provider.is_empty() {
                    ui.add_space(6.0);
                    ui.label(widgets::weak(
                        ui,
                        format!("{} · {}", usage.provider, usage.model)
                            .trim_matches([' ', '·'])
                            .to_string(),
                    ));
                }
            }
            None => {
                ui.horizontal(|ui| {
                    ui.label(icons::ICON_INFO.rich_text().size(16.0).color(p.text_weak));
                    ui.label(widgets::weak(
                        ui,
                        if view.live {
                            "The usage totals appear when the AI phase ends."
                        } else {
                            "The crawler reported no AI usage totals for this run."
                        },
                    ));
                });
            }
        }
        ui.add_space(theme::GRID * 2.0);
        if !state.ai_tasks.is_empty() {
            widgets::card(ui, |ui| {
                widgets::section_heading(ui, "Tasks", None);
                ui.add_space(theme::GRID);
                for task in &state.ai_tasks {
                    ui.horizontal(|ui| {
                        let label = if task.label.is_empty() { &task.key } else { &task.label };
                        ui.allocate_ui_with_layout(
                            egui::vec2(180.0, 22.0),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.set_width(180.0);
                                ui.add(egui::Label::new(RichText::new(label).color(p.text)).truncate());
                            },
                        );
                        let fraction = if task.total > 0 {
                            task.done as f32 / task.total as f32
                        } else if task.state == AiTaskState::Finished {
                            1.0
                        } else {
                            0.0
                        };
                        let done = task.state == AiTaskState::Finished;
                        let bar = (ui.available_width() - 200.0).max(60.0);
                        progress::fraction_bar(ui, bar, fraction, if done { p.success } else { p.accent }, p);
                        ui.label(
                            RichText::new(format!("{} / {}", format::count(task.done), format::count(task.total)))
                                .color(p.text),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if done {
                                widgets::chip(ui, "Done", Tone::Success);
                            } else {
                                widgets::chip(ui, "Running", Tone::Accent);
                            }
                        });
                    });
                }
            });
            ui.add_space(theme::GRID * 2.0);
        }
        ui.horizontal(|ui| {
            widgets::section_heading(ui, "Requests", None);
            if state.ai_requests_dropped > 0 {
                ui.label(widgets::weak(
                    ui,
                    format!("(the oldest {} are not kept)", format::count(state.ai_requests_dropped)),
                ));
            }
        });
        ui.add_space(4.0);
        if state.ai_requests.is_empty() {
            ui.label(widgets::weak(ui, "No AI request yet."));
            return;
        }
        request_log(ui, view);
    });
}

fn request_log(ui: &mut Ui, view: &RunView<'_>) {
    let p = theme::palette(ui);
    let requests = &view.state.ai_requests;
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(theme::CARD_RADIUS)
        .inner_margin(Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            TableBuilder::new(ui)
                .id_salt("run-ai-log")
                .max_scroll_height(420.0)
                .striped(true)
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(52.0))
                .column(Column::exact(110.0))
                .column(Column::initial(200.0).at_least(80.0).clip(true).resizable(true))
                .column(Column::exact(92.0))
                .column(Column::exact(120.0))
                .column(Column::exact(72.0))
                .column(Column::exact(72.0))
                .column(Column::remainder().at_least(80.0).clip(true))
                .min_scrolled_height(0.0)
                .auto_shrink([false, false])
                .header(HEADER_HEIGHT, |mut header| {
                    for label in [
                        "#",
                        "Task",
                        "Subject",
                        "Outcome",
                        "Tokens in → out",
                        "Time",
                        "Tok/s",
                        "Error",
                    ] {
                        header.col(|ui| table::header_label(ui, label));
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, requests.len(), |mut row| {
                        let Some(request) = requests.get(row.index()) else {
                            return;
                        };
                        row.col(|ui| {
                            ui.label(RichText::new(request.seq.to_string()).color(p.text_weak));
                        });
                        row.col(|ui| {
                            let task = request
                                .label
                                .as_deref()
                                .or(request.task.as_deref())
                                .unwrap_or(&request.category);
                            table::cell_text(ui, RichText::new(task).color(p.text));
                        });
                        row.col(|ui| {
                            table::cell_text(
                                ui,
                                RichText::new(request.subject.as_deref().unwrap_or("—")).color(p.text),
                            );
                        });
                        row.col(|ui| {
                            let (text, tone) = match request.outcome.as_str() {
                                "ok" => ("OK", Tone::Success),
                                "retry" => ("Retry", Tone::Warning),
                                "error" => ("Error", Tone::Danger),
                                "cacheHit" => ("Cache hit", Tone::Info),
                                other => (other, Tone::Neutral),
                            };
                            widgets::chip(ui, text, tone);
                        });
                        row.col(|ui| {
                            let tokens = match (request.input_tokens, request.output_tokens) {
                                (Some(input), Some(output)) => {
                                    format!("{} → {}", format::count(input), format::count(output))
                                }
                                (Some(input), None) => format!("{} → —", format::count(input)),
                                _ => "—".to_string(),
                            };
                            ui.label(RichText::new(tokens).color(p.text));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(request.ms.map_or("—".to_string(), format::millis)).color(p.text));
                        });
                        row.col(|ui| {
                            let rate = request
                                .output_tokens_per_second
                                .map_or("—".to_string(), |rate| format!("{rate:.0}"));
                            ui.label(RichText::new(rate).color(p.text));
                        });
                        row.col(|ui| {
                            if let Some(error) = &request.error {
                                table::cell_text(ui, RichText::new(error).color(p.danger));
                            }
                        });
                    });
                });
        });
}
