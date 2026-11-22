//! The URL bar: the URL field (focused on launch), Start crawl, and the messages under them — the
//! URL's shape after the field was left, the CLI's validation message after a failed Start, and a
//! link to the running crawl.

use eframe::egui::{self, Align2, Frame, Key, Margin, RichText, Sense, Stroke, TextEdit, Ui};
use egui_material_icons::icons;

use crate::app::{App, Page};
use crate::model::config::{check_url, complete_url};
use crate::theme;
use crate::ui::widgets::{self, Tone};

const START_WIDTH: f32 = 150.0;
const HEIGHT: f32 = 40.0;
/// How much lower than centred the field's row starts (twice the shift of its content).
const LOWERED: i8 = 6;

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = theme::palette(ui);
    let mut start = false;
    ui.horizontal(|ui| {
        let field_width = (ui.available_width() - START_WIDTH - theme::GRID).max(200.0);
        let focused_before = ui.memory(|memory| memory.focused()) == Some(egui::Id::unique("url-field"));
        let stroke = if focused_before { p.accent } else { p.border_strong };
        let response = Frame::new()
            .fill(p.field)
            .stroke(Stroke::new(if focused_before { 1.5 } else { 1.0 }, stroke))
            .corner_radius(theme::CARD_RADIUS)
            // The glyph and the text sit 3 px lower than centred (see below): the row is 6 px lower
            // at the top.
            .inner_margin(Margin {
                left: 10,
                right: 10,
                top: LOWERED,
                bottom: 0,
            })
            .show(ui, |ui| {
                let row_height = HEIGHT - 2.0 - f32::from(LOWERED);
                ui.set_width(field_width - 22.0);
                ui.set_height(row_height);
                ui.horizontal_centered(|ui| {
                    // Painted, not a label, so that the glyph sits on the baseline of the URL text: the
                    // 5 px lift puts the bottom of the glyph's ink on that baseline (measured). With
                    // the row lowered, the room above the glyph equals the room below the baseline.
                    let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(20.0, row_height), Sense::hover());
                    ui.painter().text(
                        icon_rect.center() - egui::vec2(0.0, 5.0),
                        Align2::CENTER_CENTER,
                        icons::ICON_PUBLIC.codepoint,
                        egui::FontId::new(18.0, icons::ICON_PUBLIC.font_family()),
                        p.text_weak,
                    );
                    ui.add(
                        TextEdit::singleline(&mut app.session.config.url)
                            .id(egui::Id::unique("url-field"))
                            .hint_text("https://example.com")
                            .font(egui::FontId::proportional(16.0))
                            .frame(egui::Frame::NONE)
                            .desired_width(f32::INFINITY),
                    )
                    .accessible_name("Website URL")
                })
                .inner
            })
            .inner;
        if app.setup.focus_url {
            app.setup.focus_url = false;
            response.request_focus();
        }
        if response.changed() {
            app.setup.start_error = None;
        }
        if response.lost_focus() {
            let completed = complete_url(&app.session.config.url);
            if completed != app.session.config.url {
                app.session.config.url = completed;
            }
            if !app.session.config.url.trim().is_empty() {
                app.setup.url_touched = true;
            }
            if ui.input(|input| input.key_pressed(Key::Enter)) {
                start = true;
            }
        }

        let blocker = app.start_blocker();
        let button = widgets::primary_button_sized(
            ui,
            blocker.is_none(),
            Some(icons::ICON_PLAY_ARROW),
            "Start crawl",
            egui::vec2(START_WIDTH, HEIGHT),
        );
        let button = match blocker {
            Some(reason) => button.on_disabled_hover_text(reason),
            None => button.on_hover_text("Validate the settings and start (Enter, Ctrl+Enter)"),
        };
        if button.clicked() {
            start = true;
        }
    });

    let url = app.session.config.url.trim().to_string();
    if app.has_active_run() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("A crawl is running.").color(p.text_weak));
            if widgets::link_button(ui, "Show the running crawl").clicked() {
                app.page = Page::Run;
            }
        });
    } else if let Some(error) = app.setup.start_error.clone() {
        ui.add_space(theme::GRID);
        let dismiss = widgets::banner(ui, Tone::Danger, &error, |ui| {
            widgets::icon_button(ui, icons::ICON_CLOSE, "Dismiss the message").clicked()
        });
        if dismiss {
            app.setup.start_error = None;
        }
    } else if app.setup.url_touched
        && !url.is_empty()
        && let Err(message) = check_url(&url)
    {
        ui.add_space(4.0);
        widgets::inline_message(ui, Tone::Danger, &message);
    }

    if start && app.start_blocker().is_none() {
        app.start_crawl();
    }
}
