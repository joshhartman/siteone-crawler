//! The Overview tab: when finished, the quality scores first (overall ring, five categories that
//! expand to their deductions, finding counts that open the Findings tab); then stat tiles, the
//! status-class breakdown, the content types and the issues. Once URLs were retried after the
//! crawl, the tiles and the breakdown count their newest answers and, when a retry changed an
//! answer, a quiet notice says that the crawler's own outputs are older (GUI-SPEC §4.9).

use eframe::egui::{self, Align, Align2, FontId, Layout, Rect, Role, Sense, Stroke, Ui, WidgetInfo};
use egui_material_icons::icons;

use super::{CheckStatus, RunAction, RunUi, RunView, Tab, padded_scroll, severity_style};
use crate::model::link_check::OutcomeCounts;
use crate::model::results::{QualityScores, ResultData, ResultLoad, Score, Severity};
use crate::model::run_state::{RunOutcome, StatusClass, StatusCounts};
use crate::theme::{self, Palette};
use crate::ui::widgets::stat::{Stat, tile_row};
use crate::ui::widgets::{self, Tone, format, one_line, progress, score};

/// From this width on, the breakdowns and the issues sit side by side.
const TWO_COLUMNS_FROM: f32 = 820.0;

pub fn show(ui: &mut Ui, view: &RunView<'_>, run_ui: &mut RunUi, actions: &mut Vec<RunAction>) {
    padded_scroll(ui, "run-overview", |ui| {
        result_notice(ui, view);
        if outputs_notice(ui, view, actions) {
            ui.add_space(theme::GRID * 1.5);
        }
        if let Some(data) = view.data()
            && let Some(scores) = &data.scores
        {
            scores_card(ui, data, scores, run_ui, actions);
            ui.add_space(theme::GRID * 2.0);
        }
        let tiles = tiles(view);
        if let Some(clicked) = tile_row(ui, &tiles)
            && tiles.get(clicked).is_some_and(|tile| tile.link.is_some())
        {
            actions.push(RunAction::ShowBroken(None));
        }
        if !view.live {
            external_links_line(ui, view, actions);
        }
        ui.add_space(theme::GRID * 2.0);
        let two_columns = ui.available_width() >= TWO_COLUMNS_FROM;
        if two_columns {
            ui.scope(|ui| {
                let spacing = ui.spacing().item_spacing.x;
                ui.spacing_mut().item_spacing.x = theme::GRID * 2.0;
                ui.columns(2, |columns| {
                    if let [left, right] = columns {
                        left.spacing_mut().item_spacing.x = spacing;
                        right.spacing_mut().item_spacing.x = spacing;
                        // Status codes beside content types, so neither column is left empty.
                        status_card(left, view, actions);
                        left.add_space(theme::GRID * 2.0);
                        last_output_card(left, view);
                        issues_card(left, view);
                        content_card(right, view);
                    }
                });
            });
        } else {
            status_card(ui, view, actions);
            ui.add_space(theme::GRID * 2.0);
            content_card(ui, view);
            ui.add_space(theme::GRID * 2.0);
            last_output_card(ui, view);
            issues_card(ui, view);
        }
    });
}

/// One line about the external link check under the tiles (the tiles stay about crawled URLs),
/// with a link to the Link checker tab.
fn external_links_line(ui: &mut Ui, view: &RunView<'_>, actions: &mut Vec<RunAction>) {
    let links = &view.links;
    let found = links.list.map_or(0, |list| list.links.len());
    let (tone, text, link, action) = match &links.status {
        CheckStatus::Running { done, total } => (
            Tone::Accent,
            format!(
                "Checking external links: {} of {}",
                format::count(*done as u64),
                format::count(*total as u64)
            ),
            "Show external links",
            RunAction::ShowExternal,
        ),
        CheckStatus::Waiting { .. } => (
            Tone::Neutral,
            format!("{} will be checked next", format::plural(found as u64, "external link")),
            "Show broken links",
            RunAction::ShowTab(Tab::Broken),
        ),
        CheckStatus::Done => {
            let counts = OutcomeCounts::of(links.results);
            if counts.total() == 0 {
                return;
            }
            let tone = if counts.broken > 0 {
                Tone::Danger
            } else if counts.refused > 0 {
                Tone::Warning
            } else {
                Tone::Success
            };
            (tone, counts.summary(), "Show external links", RunAction::ShowExternal)
        }
        CheckStatus::Failed(_) => (
            Tone::Warning,
            "The external links could not be checked".to_string(),
            "Details",
            RunAction::ShowTab(Tab::Broken),
        ),
        CheckStatus::Stopped | CheckStatus::NotRequested if links.unchecked() > 0 => (
            Tone::Neutral,
            format!(
                "{} not checked",
                format::plural(links.unchecked() as u64, "external link")
            ),
            "Check in Link checker",
            RunAction::ShowTab(Tab::Broken),
        ),
        _ => return,
    };
    let p = theme::palette(ui);
    ui.add_space(theme::GRID);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let color = match tone {
            Tone::Neutral => p.text_weak,
            other => other.color(p),
        };
        ui.label(icons::ICON_LINK.rich_text().size(17.0).color(color));
        ui.label(egui::RichText::new(text).color(p.text));
        if widgets::link_button(ui, link).clicked() {
            actions.push(action);
        }
    });
}

/// Once a retry gave a crawled URL another answer: the crawler's outputs show the crawl as it ran.
/// One quiet line with *Run again*; returns whether it was shown. The Files tab shows it too.
pub(super) fn outputs_notice(ui: &mut Ui, view: &RunView<'_>, actions: &mut Vec<RunAction>) -> bool {
    if view.live || !view.retry.outputs_older() {
        return false;
    }
    let p = theme::palette(ui);
    let text =
        "Retries changed answers after the crawl. The HTML report, the scores and the tables show the crawl as it ran.";
    let tip = if view.retry.cache_on {
        "Load this run's configuration into Setup to crawl the site again. With the HTTP cache on, the new crawl takes the answers the cache still holds (within its lifetime) and requests the rest; failed requests, 429 and 5xx answers are always requested again."
    } else {
        "Load this run's configuration into Setup to crawl the site again."
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(icons::ICON_INFO.rich_text().size(16.0).color(p.text_weak));
        ui.label(widgets::weak(ui, text));
        if widgets::link_button(ui, "Run again").on_hover_text(tip).clicked() {
            actions.push(RunAction::RunAgain);
        }
    });
    true
}

/// Loading, a missing or an unreadable result file.
fn result_notice(ui: &mut Ui, view: &RunView<'_>) {
    if view.live {
        return;
    }
    let p = theme::palette(ui);
    if view.result_loading {
        ui.horizontal(|ui| {
            progress::spinner(ui, 16.0, p.accent);
            ui.label(widgets::weak(ui, "Reading the results…"));
        });
        ui.add_space(theme::GRID * 1.5);
        return;
    }
    match view.result {
        Some(ResultLoad::Unreadable(error)) => {
            widgets::banner(
                ui,
                Tone::Warning,
                &format!("{error}. Scores and findings are not available; the HTML report may still have them."),
                |_| {},
            );
            ui.add_space(theme::GRID * 2.0);
        }
        Some(ResultLoad::Missing)
            if matches!(
                view.outcome,
                RunOutcome::Finished | RunOutcome::Stopped | RunOutcome::QualityGateFailed
            ) =>
        {
            widgets::banner(
                ui,
                Tone::Info,
                "The run wrote no result file, so scores and findings are not available.",
                |_| {},
            );
            ui.add_space(theme::GRID * 2.0);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Scores
// ---------------------------------------------------------------------------------------------

const RING: f32 = 128.0;
const RING_BLOCK: f32 = 176.0;
const CATEGORY_HEIGHT: f32 = 88.0;

fn scores_card(
    ui: &mut Ui,
    data: &ResultData,
    scores: &QualityScores,
    run_ui: &mut RunUi,
    actions: &mut Vec<RunAction>,
) {
    let p = theme::palette(ui);
    widgets::card(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = theme::GRID * 2.0;
            ui.allocate_ui_with_layout(
                egui::vec2(RING_BLOCK, RING + 52.0),
                Layout::top_down(Align::Center),
                |ui| {
                    ui.set_width(RING_BLOCK);
                    let overall = &scores.overall;
                    let name = format!(
                        "Overall score {} of 10, {}",
                        score::score_text(overall.score),
                        overall.label
                    );
                    score::score_ring(ui, RING, overall.score, &name);
                    ui.add_space(4.0);
                    ui.label(theme::semibold("Overall score").size(14.0).color(p.text));
                    if !overall.label.is_empty() {
                        widgets::chip(ui, &overall.label, score::band_tone(overall.score));
                    }
                },
            );
            ui.vertical(|ui| {
                categories(ui, &scores.categories, run_ui);
                ui.add_space(theme::GRID * 1.5);
                severity_counts(ui, data, actions);
            });
        });
        if let Some(code) = run_ui.expanded_score.clone()
            && let Some(category) = scores.categories.iter().find(|category| category.code == code)
        {
            ui.add_space(theme::GRID * 1.5);
            widgets::hairline(ui);
            ui.add_space(theme::GRID * 1.5);
            deductions(ui, category);
        }
    });
}

fn categories(ui: &mut Ui, categories: &[Score], run_ui: &mut RunUi) {
    if categories.is_empty() {
        return;
    }
    let gap = theme::GRID;
    let width = ui.available_width();
    let per_row = if (width - gap * 4.0) / 5.0 >= 140.0 { 5 } else { 3 }.min(categories.len());
    let card_width = (width - gap * (per_row as f32 - 1.0)) / per_row as f32;
    for (index, row) in categories.chunks(per_row).enumerate() {
        if index > 0 {
            ui.add_space(gap - ui.spacing().item_spacing.y);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for category in row {
                let expanded = run_ui.expanded_score.as_deref() == Some(category.code.as_str());
                if category_card(ui, card_width, category, expanded).clicked() {
                    run_ui.expanded_score = if expanded { None } else { Some(category.code.clone()) };
                }
            }
        });
    }
}

fn category_card(ui: &mut Ui, width: f32, category: &Score, expanded: bool) -> egui::Response {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, CATEGORY_HEIGHT), Sense::click());
    let name = if category.name.is_empty() {
        category.code.clone()
    } else {
        category.name.clone()
    };
    let accessible = format!(
        "{name}: {} of 10, {}",
        score::score_text(category.score),
        category.label
    );
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, expanded, &accessible));
    let response = response.on_hover_text(if expanded {
        "Hide the deductions"
    } else {
        "Show what lowered this score"
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let tone = score::band_tone(category.score);
    let color = tone.color(p);
    let (fill, stroke) = if expanded {
        (p.accent_soft, Stroke::new(1.5, p.accent))
    } else if response.hovered() {
        (p.surface, Stroke::new(1.0, p.border_strong))
    } else {
        (p.surface, Stroke::new(1.0, p.border))
    };
    let painter = ui.painter();
    painter.rect(rect, theme::CARD_RADIUS, fill, stroke, egui::StrokeKind::Inside);
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            theme::CARD_RADIUS + 2,
            Stroke::new(1.5, p.accent),
            egui::StrokeKind::Outside,
        );
    }
    let inner = rect.shrink2(egui::vec2(12.0, 10.0));
    let chevron = if expanded {
        icons::ICON_EXPAND_LESS
    } else {
        icons::ICON_EXPAND_MORE
    };
    painter.text(
        inner.right_top() + egui::vec2(2.0, -1.0),
        Align2::RIGHT_TOP,
        chevron.codepoint,
        FontId::new(18.0, chevron.font_family()),
        p.text_weak,
    );
    let title = one_line(ui, &name, theme::semibold_font(13.0), p.text, inner.width() - 20.0);
    painter.galley(inner.left_top(), title, p.text);
    let value = ui
        .painter()
        .layout_no_wrap(score::score_text(category.score), theme::semibold_font(24.0), p.text);
    let value_width = value.size().x;
    let value_pos = inner.left_top() + egui::vec2(0.0, 20.0);
    painter.galley(value_pos, value, p.text);
    painter.text(
        value_pos + egui::vec2(value_width + 3.0, 22.0),
        Align2::LEFT_BOTTOM,
        "/10",
        FontId::proportional(12.0),
        p.text_weak,
    );
    if !category.label.is_empty() {
        let band = one_line(
            ui,
            &category.label,
            theme::semibold_font(12.0),
            color,
            (inner.width() - value_width - 34.0).max(20.0),
        );
        painter.galley(
            egui::pos2(inner.right() - band.size().x, value_pos.y + 7.0),
            band,
            color,
        );
    }
    let bar = Rect::from_min_size(
        egui::pos2(inner.left(), inner.bottom() - 5.0),
        egui::vec2(inner.width(), 5.0),
    );
    painter.rect_filled(bar, 2.5, progress::track(p));
    let fraction = (category.score / 10.0).clamp(0.0, 1.0) as f32;
    if fraction > 0.0 {
        let filled = Rect::from_min_size(bar.min, egui::vec2(bar.width() * fraction, bar.height()));
        painter.rect_filled(filled, 2.5, color);
    }
    response
}

fn deductions(ui: &mut Ui, category: &Score) {
    let p = theme::palette(ui);
    let name = if category.name.is_empty() {
        &category.code
    } else {
        &category.name
    };
    ui.label(
        theme::semibold(format!("What lowered “{name}”"))
            .size(14.0)
            .color(p.text),
    );
    ui.add_space(4.0);
    if category.deductions.is_empty() {
        ui.label(widgets::weak(ui, "Nothing — this category has full marks."));
        return;
    }
    for deduction in &category.deductions {
        ui.horizontal_top(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(52.0, 18.0), Sense::hover());
            ui.painter().text(
                rect.right_center(),
                Align2::RIGHT_CENTER,
                format!("−{:.1}", deduction.points.abs()),
                theme::semibold_font(13.0),
                p.danger,
            );
            ui.add(egui::Label::new(egui::RichText::new(&deduction.reason).color(p.text)).wrap());
        });
    }
}

fn severity_counts(ui: &mut Ui, data: &ResultData, actions: &mut Vec<RunAction>) {
    let p = theme::palette(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        ui.label(widgets::weak(ui, "Findings"));
        ui.add_space(4.0);
        for severity in [Severity::Critical, Severity::Warning, Severity::Notice, Severity::Ok] {
            let count = data.count(severity);
            let (tone, icon) = severity_style(severity);
            let color = if count > 0 { tone.color(p) } else { p.text_weak };
            let name = format!("{} findings", severity.label());
            let response = ui
                .add(
                    egui::Button::new((
                        icon.rich_text().size(15.0).color(color),
                        theme::semibold(format::count(count as u64)).color(p.text),
                        egui::RichText::new(severity.label()).color(p.text_weak),
                    ))
                    .corner_radius(13)
                    .min_size(egui::vec2(0.0, 26.0)),
                )
                .accessible_name(&name)
                .on_hover_text(format!("Show the {} findings", severity.label().to_lowercase()));
            if response.clicked() {
                actions.push(RunAction::ShowFindings(severity));
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Tiles and breakdowns
// ---------------------------------------------------------------------------------------------

fn tiles(view: &RunView<'_>) -> Vec<Stat> {
    let state = view.state;
    let stats = view.data().and_then(|data| data.stats.as_ref());
    let mut tiles = Vec::new();
    if view.live {
        tiles.push(
            Stat::new("URLs", format::count(state.done))
                .detail(format!("of {} found so far", format::count(state.total))),
        );
        let rate = state.urls_per_second(std::time::Instant::now());
        tiles.push(Stat::new("URLs per second", format!("{rate:.1}")).detail("over the last 5 s"));
    } else {
        let urls = stats.map_or(state.urls.len() as u64, |stats| {
            stats.total_urls.max(state.urls.len() as u64)
        });
        tiles.push(
            Stat::new("URLs crawled", format::count(urls))
                .detail(format!("{} found", format::count(state.total.max(urls)))),
        );
        let sub = if state.urls.is_empty() {
            String::new()
        } else {
            let per_second = state.urls.len() as f64 / view.elapsed.as_secs_f64().max(0.001);
            format!("{per_second:.1} URLs per second")
        };
        let mut tile = Stat::new("Total time", format::duration(view.elapsed));
        if !sub.is_empty() {
            tile = tile.detail(sub);
        }
        tiles.push(tile);
    }
    let (average, slowest) = match stats {
        Some(stats) if stats.total_urls > 0 => (
            Some(stats.total_requests_times_avg * 1000.0),
            (stats.total_requests_times_max * 1000.0) as u64,
        ),
        _ => (state.avg_time_ms(), state.max_time_ms),
    };
    let mut tile = Stat::new(
        "Average response",
        average.map_or_else(|| "—".to_string(), |ms| format::millis(ms.round() as u64)),
    );
    if average.is_some() {
        tile = tile.detail(format!("slowest {}", format::millis(slowest)));
    }
    tiles.push(tile);
    let bytes = stats.map_or(state.total_bytes, |stats| stats.total_size.max(state.total_bytes));
    tiles.push(
        Stat::new("Downloaded", format::bytes(bytes)).detail(if state.urls.is_empty() {
            "no responses yet".to_string()
        } else {
            format!("{} per URL", format::bytes(bytes / state.urls.len() as u64))
        }),
    );
    let counts = state.status_counts;
    let errors = counts.errors();
    let mut tile = Stat::new("Errors", format::count(errors))
        .detail(error_breakdown(&counts))
        .link("Show the broken links");
    if errors > 0 {
        tile = tile.tone(Tone::Danger);
    } else if !state.urls.is_empty() {
        tile = tile.tone(Tone::Success);
    }
    tiles.push(tile);
    tiles
}

fn error_breakdown(counts: &StatusCounts) -> String {
    let parts: Vec<String> = [
        (counts.client_error, "4xx"),
        (counts.server_error, "5xx"),
        (counts.error, "failed"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, label)| format!("{} {label}", format::count(count)))
    .collect();
    if parts.is_empty() {
        "no 4xx, 5xx or failed requests".to_string()
    } else {
        parts.join(" · ")
    }
}

/// The status classes: label, description, colour, count.
pub fn status_classes(counts: &StatusCounts, p: &Palette) -> [(&'static str, &'static str, egui::Color32, u64); 5] {
    [
        ("2xx", "Success", p.success, counts.success),
        ("3xx", "Redirect", p.info, counts.redirect),
        ("4xx", "Client error", p.warning, counts.client_error),
        ("5xx", "Server error", p.danger, counts.server_error),
        ("Failed", "No response", failed_color(p), counts.error),
    ]
}

/// Transport errors: red like 5xx, a shade darker so the bar tells them apart.
pub fn failed_color(p: &Palette) -> egui::Color32 {
    if p.dark {
        egui::Color32::from_rgb(190, 50, 60)
    } else {
        egui::Color32::from_rgb(150, 20, 30)
    }
}

fn status_card(ui: &mut Ui, view: &RunView<'_>, actions: &mut Vec<RunAction>) {
    let p = theme::palette(ui);
    let counts = view.state.status_counts;
    let classes = status_classes(&counts, p);
    let total: u64 = classes.iter().map(|class| class.3).sum();
    widgets::card(ui, |ui| {
        widgets::section_heading(ui, "Status codes", None);
        ui.add_space(theme::GRID);
        if total == 0 {
            ui.label(widgets::weak(
                ui,
                empty_text(view, "No responses yet.", "No responses."),
            ));
            return;
        }
        // One stacked bar, then a legend that carries the labels (colour is never the only signal).
        let width = ui.available_width();
        let (bar, _) = ui.allocate_exact_size(egui::vec2(width, 12.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(bar, 4.0, progress::track(p));
        let mut x = bar.left();
        let shown: Vec<_> = classes.iter().filter(|class| class.3 > 0).collect();
        for (index, (_, _, color, count)) in shown.iter().enumerate() {
            let segment = bar.width() * *count as f32 / total as f32;
            let right = if index + 1 == shown.len() {
                bar.right()
            } else {
                x + segment
            };
            let rect = Rect::from_x_y_ranges(x..=right.max(x + 2.0), bar.y_range());
            let rounding = egui::CornerRadius {
                nw: if index == 0 { 4 } else { 0 },
                sw: if index == 0 { 4 } else { 0 },
                ne: if index + 1 == shown.len() { 4 } else { 0 },
                se: if index + 1 == shown.len() { 4 } else { 0 },
            };
            painter.rect_filled(rect, rounding, *color);
            x = right;
        }
        ui.add_space(theme::GRID);
        for (slot, (label, description, color, count)) in classes.into_iter().enumerate() {
            let class = match slot {
                2 => Some(StatusClass::ClientError),
                3 => Some(StatusClass::ServerError),
                4 => Some(StatusClass::Error),
                _ => None,
            };
            let clicked = legend_row(
                ui,
                label,
                description,
                color,
                count,
                total,
                class.is_some() && count > 0,
            );
            if clicked && let Some(class) = class {
                actions.push(RunAction::ShowBroken(Some(class)));
            }
        }
    });
}

/// A legend line; `link` makes it a button (to the broken links of that class). Returns whether it
/// was clicked.
fn legend_row(
    ui: &mut Ui,
    label: &str,
    description: &str,
    color: egui::Color32,
    count: u64,
    total: u64,
    link: bool,
) -> bool {
    let p = theme::palette(ui);
    let weak = count == 0;
    let row = ui.horizontal(|ui| {
        let (swatch, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
        if weak {
            ui.painter()
                .rect_stroke(swatch, 3.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
        } else {
            ui.painter().rect_filled(swatch, 3.0, color);
        }
        let text_color = if weak { p.text_weak } else { p.text };
        widgets::fixed_label(ui, 52.0, theme::semibold(label).color(text_color), Align::Min);
        ui.label(widgets::weak(ui, description));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // The arrow column is always reserved, so the numbers line up.
            let (arrow, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
            if link {
                ui.painter().text(
                    arrow.center(),
                    Align2::CENTER_CENTER,
                    icons::ICON_ARROW_FORWARD.codepoint,
                    FontId::new(14.0, icons::ICON_ARROW_FORWARD.font_family()),
                    p.text_weak,
                );
            }
            let percent = if total > 0 {
                count as f64 * 100.0 / total as f64
            } else {
                0.0
            };
            widgets::fixed_label(ui, 52.0, widgets::weak(ui, percent_text(percent)), Align::Max);
            ui.label(egui::RichText::new(format::count(count)).color(text_color));
        });
    });
    if !link {
        return false;
    }
    let response = row.response.interact(Sense::click());
    let name = format!("Show the {label} broken links");
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
    if response.hovered() {
        ui.painter().rect_filled(
            response.rect.expand2(egui::vec2(4.0, 1.0)),
            theme::RADIUS,
            p.surface_alt.gamma_multiply(0.5),
        );
    }
    response
        .on_hover_text(format!("Show the {label} responses in Link checker"))
        .clicked()
}

fn percent_text(percent: f64) -> String {
    if percent > 0.0 && percent < 0.1 {
        "<0.1 %".to_string()
    } else {
        format!("{percent:.1} %")
    }
}

fn content_card(ui: &mut Ui, view: &RunView<'_>) {
    let p = theme::palette(ui);
    let mut kinds: Vec<_> = view
        .state
        .content_counts
        .iter()
        .map(|(kind, count)| (*kind, *count))
        .collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let total: u64 = kinds.iter().map(|(_, count)| count).sum();
    let max = kinds.first().map_or(1, |(_, count)| (*count).max(1));
    widgets::card(ui, |ui| {
        widgets::section_heading(ui, "Content types", None);
        ui.add_space(theme::GRID);
        if kinds.is_empty() {
            ui.label(widgets::weak(
                ui,
                empty_text(view, "No responses yet.", "No responses."),
            ));
            return;
        }
        for (kind, count) in kinds {
            ui.horizontal(|ui| {
                widgets::fixed_label(ui, 76.0, egui::RichText::new(kind.label()).color(p.text), Align::Min);
                let bar_width = (ui.available_width() - 130.0).max(40.0);
                progress::fraction_bar(ui, bar_width, count as f32 / max as f32, p.accent, p);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let percent = count as f64 * 100.0 / total.max(1) as f64;
                    widgets::fixed_label(ui, 52.0, widgets::weak(ui, percent_text(percent)), Align::Max);
                    ui.label(egui::RichText::new(format::count(count)).color(p.text));
                });
            });
        }
    });
}

fn empty_text(view: &RunView<'_>, live: &'static str, ended: &'static str) -> &'static str {
    if view.live { live } else { ended }
}

fn issues_card(ui: &mut Ui, view: &RunView<'_>) {
    let p = theme::palette(ui);
    let issues = &view.state.issues;
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            widgets::section_heading(ui, "Issues", None);
            if !issues.is_empty() {
                widgets::chip(ui, format::count(issues.len() as u64), Tone::Warning);
            }
        });
        ui.add_space(theme::GRID);
        if issues.is_empty() {
            ui.horizontal_top(|ui| {
                ui.label(icons::ICON_CHECK_CIRCLE.rich_text().size(16.0).color(p.success));
                ui.add(
                    egui::Label::new(widgets::weak(
                        ui,
                        if view.live {
                            "None so far. Problems the crawler reports (a skipped AI phase, a failed upload or export) appear here."
                        } else {
                            "No export, upload or AI problems were reported."
                        },
                    ))
                    .wrap(),
                );
            });
            return;
        }
        for (index, issue) in issues.iter().enumerate() {
            if index > 0 {
                ui.add_space(4.0);
                widgets::hairline(ui);
                ui.add_space(4.0);
            }
            ui.horizontal_top(|ui| {
                ui.label(icons::ICON_WARNING.rich_text().size(16.0).color(p.warning));
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let label = if issue.label.is_empty() { "Issue" } else { &issue.label };
                        ui.label(theme::semibold(label).color(p.text));
                        if !issue.kind.is_empty() {
                            widgets::chip(ui, &issue.kind, Tone::Neutral);
                        }
                    });
                    if !issue.detail.is_empty() {
                        ui.add(egui::Label::new(widgets::weak(ui, &issue.detail)).wrap());
                    }
                });
            });
        }
    });
}

/// The last console lines, for a run that ended badly (GUI-SPEC §4.5).
fn last_output_card(ui: &mut Ui, view: &RunView<'_>) {
    if !matches!(
        view.outcome,
        RunOutcome::Crashed { .. } | RunOutcome::Aborted | RunOutcome::Failed { .. } | RunOutcome::NoPagesCrawled
    ) {
        return;
    }
    let p = theme::palette(ui);
    let lines = view.state.console.lines();
    widgets::card(ui, |ui| {
        widgets::section_heading(ui, "Last console output", None);
        ui.add_space(theme::GRID);
        let shown: Vec<String> = lines
            .iter()
            .rev()
            .map(|line| line.text())
            .filter(|text| !text.trim().is_empty())
            .take(8)
            .collect();
        if shown.is_empty() {
            ui.label(widgets::weak(ui, "The crawler printed nothing."));
            return;
        }
        egui::Frame::new()
            .fill(p.surface_alt)
            .corner_radius(theme::RADIUS)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for line in shown.iter().rev() {
                    ui.add(egui::Label::new(theme::mono(line).size(12.0).color(p.text)).truncate());
                }
            });
    });
    ui.add_space(theme::GRID * 2.0);
}
