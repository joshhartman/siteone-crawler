//! An underlined tab bar: the label of each tab with an optional count, the selected one
//! underlined in the accent colour, a hairline under the whole bar.

use eframe::egui::{self, FontId, Role, Sense, Stroke, Ui, WidgetInfo};

use super::format;
use crate::theme;

/// The height of the bar.
pub const TAB_HEIGHT: f32 = 38.0;

/// One tab: its value, label and optional count.
pub struct TabSpec<T> {
    pub value: T,
    pub label: &'static str,
    pub count: Option<u64>,
    /// The count is a problem (danger badge) when it is above zero.
    pub alert: bool,
}

/// Draws the tabs; returns whether the selection changed. Each tab is named by its label.
pub fn tab_bar<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, tabs: &[TabSpec<T>]) -> bool {
    let p = theme::palette(ui);
    let mut changed = false;
    // The hairline first: the underline of the selected tab is painted over it.
    let left = ui.cursor().left();
    let y = ui.cursor().top() + TAB_HEIGHT - 0.5;
    let full_width = ui.available_width();
    ui.painter()
        .hline(left..=left + full_width, y, Stroke::new(1.0, p.border));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for tab in tabs {
            let selected = *current == tab.value;
            let font = if selected {
                theme::semibold_font(14.0)
            } else {
                FontId::proportional(14.0)
            };
            let text_color = if selected { p.text } else { p.text_weak };
            let label = ui.painter().layout_no_wrap(tab.label.to_string(), font, text_color);
            let count = tab.count.map(|count| {
                ui.painter()
                    .layout_no_wrap(format::count(count), FontId::proportional(11.5), p.text_weak)
            });
            let badge_width = count.as_ref().map_or(0.0, |galley| galley.size().x + 12.0 + 6.0);
            let size = egui::vec2(label.size().x + badge_width + 24.0, TAB_HEIGHT);
            let (rect, response) = ui.allocate_exact_size(size, Sense::click());
            response.widget_info(|| WidgetInfo::selected(Role::Tab, true, selected, tab.label));
            if ui.is_rect_visible(rect) {
                let painter = ui.painter();
                if response.hovered() && !selected {
                    painter.rect_filled(rect.shrink2(egui::vec2(0.0, 4.0)), theme::RADIUS, p.surface_alt);
                }
                let text_pos = egui::pos2(rect.left() + 12.0, rect.center().y - label.size().y / 2.0);
                let label_width = label.size().x;
                painter.galley(text_pos, label, text_color);
                if let Some(galley) = count {
                    let badge = egui::Rect::from_min_size(
                        egui::pos2(rect.left() + 12.0 + label_width + 6.0, rect.center().y - 9.0),
                        egui::vec2(galley.size().x + 12.0, 18.0),
                    );
                    let alarming = tab.alert && tab.count.is_some_and(|count| count > 0);
                    let (fill, color) = if alarming {
                        (super::Tone::Danger.soft(p), p.danger)
                    } else if selected {
                        (p.accent_soft, p.accent)
                    } else {
                        (p.surface_alt, p.text_weak)
                    };
                    painter.rect_filled(badge, 9.0, fill);
                    painter.galley(badge.center() - galley.size() / 2.0, galley, color);
                }
                if selected {
                    let underline = egui::Rect::from_min_max(
                        egui::pos2(rect.left() + 6.0, rect.bottom() - 2.5),
                        egui::pos2(rect.right() - 6.0, rect.bottom()),
                    );
                    painter.rect_filled(underline, 1.5, p.accent);
                }
                if response.has_focus() {
                    painter.rect_stroke(
                        rect.shrink(2.0),
                        theme::RADIUS,
                        Stroke::new(1.5, p.accent),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            if response.clicked() && !selected {
                *current = tab.value;
                changed = true;
            }
        }
    });
    changed
}
