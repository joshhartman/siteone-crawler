//! The navigation rail: Setup, Run (with a dot while a crawl is active), History, Settings
//! (GUI-SPEC §6.1). `Ctrl+1…4` switch pages too.

use eframe::egui::{self, Align2, FontId, Role, Sense, Stroke, Ui, WidgetInfo};
use egui_material_icons::{MaterialIcon, icons};

use crate::app::{App, Page};
use crate::theme;

fn icon(page: Page) -> MaterialIcon {
    match page {
        Page::Setup => icons::ICON_TUNE,
        Page::Run => icons::ICON_MONITORING,
        Page::History => icons::ICON_HISTORY,
        Page::Settings => icons::ICON_SETTINGS,
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = theme::palette(ui);
    ui.add_space(14.0);
    ui.vertical_centered(|ui| {
        ui.label(icons::ICON_TRAVEL_EXPLORE.rich_text().size(26.0).color(p.accent))
            .on_hover_text("SiteOne Crawler");
    });
    ui.add_space(14.0);
    let active_run = app.has_active_run();
    for (index, page) in Page::ALL.into_iter().enumerate() {
        let selected = app.page == page;
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 60.0), Sense::click());
        let label = page.label();
        let shortcut = format!("{label} (Ctrl+{})", index + 1);
        response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, label));
        let response = response.on_hover_text(&shortcut);
        if ui.is_rect_visible(rect) {
            let pill =
                egui::Rect::from_center_size(egui::pos2(rect.center().x, rect.top() + 20.0), egui::vec2(52.0, 30.0));
            if selected {
                ui.painter().rect_filled(pill, 15.0, p.accent_soft);
            } else if response.hovered() {
                ui.painter().rect_filled(pill, 15.0, p.border.gamma_multiply(0.6));
            }
            if response.has_focus() {
                ui.painter().rect_stroke(
                    pill.expand(2.0),
                    17.0,
                    Stroke::new(1.5, p.accent),
                    egui::StrokeKind::Outside,
                );
            }
            let color = if selected { p.accent } else { p.text_weak };
            let glyph = icon(page);
            ui.painter().text(
                pill.center(),
                Align2::CENTER_CENTER,
                glyph.codepoint,
                FontId::new(22.0, glyph.font_family()),
                color,
            );
            let text_color = if selected { p.text } else { p.text_weak };
            let font = if selected {
                theme::semibold_font(12.0)
            } else {
                FontId::proportional(12.0)
            };
            ui.painter().text(
                egui::pos2(rect.center().x, rect.top() + 46.0),
                Align2::CENTER_CENTER,
                label,
                font,
                text_color,
            );
            if page == Page::Run && active_run {
                ui.painter()
                    .circle_filled(egui::pos2(pill.right() - 9.0, pill.top() + 7.0), 4.5, p.success);
                ui.painter().circle_stroke(
                    egui::pos2(pill.right() - 9.0, pill.top() + 7.0),
                    4.5,
                    Stroke::new(1.5, p.surface_alt),
                );
            }
        }
        if response.clicked() {
            app.page = page;
        }
        ui.add_space(4.0);
    }
}
