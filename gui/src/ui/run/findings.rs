//! The Findings tab: the summary items of the result JSON, grouped by severity, with severity
//! toggles and a text filter.

use eframe::egui::{self, FontId, Ui};
use egui_material_icons::icons;

use super::{RunView, padded_scroll, result_unavailable, severity_style};
use crate::model::results::Severity;
use crate::theme;
use crate::ui::widgets::table::contains_ignore_case;
use crate::ui::widgets::{self, format};

/// The severities in display order.
pub const ORDER: [Severity; 6] = [
    Severity::Critical,
    Severity::Warning,
    Severity::Notice,
    Severity::Info,
    Severity::Ok,
    Severity::Other,
];

fn slot(severity: Severity) -> usize {
    ORDER
        .iter()
        .position(|item| *item == severity)
        .unwrap_or(ORDER.len() - 1)
}

/// UI state of the Findings tab.
#[derive(Debug)]
pub struct FindingsUi {
    /// Shown severities, by [`ORDER`] position.
    pub shown: [bool; 6],
    pub text: String,
}

impl Default for FindingsUi {
    fn default() -> Self {
        Self {
            shown: [true; 6],
            text: String::new(),
        }
    }
}

impl FindingsUi {
    /// Shows only `severity` (the Overview's finding counts).
    pub fn only(&mut self, severity: Severity) {
        self.shown = [false; 6];
        if let Some(shown) = self.shown.get_mut(slot(severity)) {
            *shown = true;
        }
        self.text.clear();
    }
}

pub fn show(ui: &mut Ui, view: &RunView<'_>, findings: &mut FindingsUi, focus: bool) {
    if result_unavailable(ui, view, icons::ICON_CHECKLIST, "Findings") {
        return;
    }
    let Some(data) = view.data() else {
        return;
    };
    let p = theme::palette(ui);
    padded_scroll(ui, "run-findings", |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            ui.label(icons::ICON_SEARCH.rich_text().size(18.0).color(p.text_weak));
            let response = ui
                .add(
                    egui::TextEdit::singleline(&mut findings.text)
                        .hint_text("Filter findings   (Ctrl+F)")
                        .desired_width(260.0)
                        .margin(egui::vec2(8.0, 5.0)),
                )
                .accessible_name("Filter findings");
            if focus {
                response.request_focus();
            }
            ui.add_space(6.0);
            for (index, severity) in ORDER.iter().enumerate() {
                let count = data.count(*severity);
                if count == 0 && *severity == Severity::Other {
                    continue;
                }
                let (tone, _) = severity_style(*severity);
                if let Some(shown) = findings.shown.get_mut(index) {
                    widgets::filter_chip(ui, shown, severity.label(), Some(count as u64), tone.color(p));
                }
            }
        });
        ui.add_space(theme::GRID * 1.5);
        if data.summary.is_empty() {
            widgets::empty_state(
                ui,
                icons::ICON_CHECKLIST,
                "No findings",
                "The analysis reported no summary items for this run.",
                None,
            );
            return;
        }
        let needle = findings.text.trim().to_lowercase();
        let mut any = false;
        for (index, severity) in ORDER.iter().enumerate() {
            if !findings.shown.get(index).copied().unwrap_or(true) {
                continue;
            }
            let items: Vec<_> = data
                .summary
                .iter()
                .filter(|item| item.severity == *severity)
                .filter(|item| {
                    contains_ignore_case(&item.text, &needle) || contains_ignore_case(&item.apl_code, &needle)
                })
                .collect();
            if items.is_empty() {
                continue;
            }
            any = true;
            let (tone, icon) = severity_style(*severity);
            ui.horizontal(|ui| {
                ui.label(icon.rich_text().size(18.0).color(tone.color(p)));
                ui.label(theme::section_title(severity.label()).color(p.text));
                widgets::chip(ui, format::count(items.len() as u64), tone);
            });
            ui.add_space(4.0);
            widgets::card(ui, |ui| {
                for (row, item) in items.iter().enumerate() {
                    if row > 0 {
                        ui.add_space(6.0);
                        widgets::hairline(ui);
                        ui.add_space(6.0);
                    }
                    ui.horizontal_top(|ui| {
                        ui.label(icon.rich_text().size(16.0).color(tone.color(p)));
                        // Room for the crawler's finding code in full (the longest are ~32 characters).
                        let code_width = if item.apl_code.is_empty() { 0.0 } else { 260.0 };
                        let width = (ui.available_width() - code_width).max(120.0);
                        ui.allocate_ui(egui::vec2(width, 0.0), |ui| {
                            ui.set_width(width);
                            let job = widgets::highlighted(
                                &item.text,
                                &findings.text,
                                FontId::proportional(14.0),
                                p.text,
                                p.highlight,
                            );
                            ui.add(egui::Label::new(job).wrap());
                        });
                        if !item.apl_code.is_empty() {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                ui.add(
                                    egui::Label::new(theme::mono(&item.apl_code).size(11.5).color(p.text_weak))
                                        .truncate(),
                                );
                            });
                        }
                    });
                }
            });
            ui.add_space(theme::GRID * 2.0);
        }
        if !any
            && widgets::empty_state(
                ui,
                icons::ICON_FILTER_ALT_OFF,
                "No finding matches the filter",
                "Turn on more severities or change the text.",
                Some("Show all findings"),
            )
        {
            *findings = FindingsUi::default();
        }
    });
}
