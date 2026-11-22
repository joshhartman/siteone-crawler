//! The command preview (GUI-SPEC §6.2): the equivalent command line, live; Copy as PowerShell /
//! Bash / config file; the number of changed options; Reset all. Secrets are masked unless shown.

use eframe::egui::{self, Align, Frame, Layout, Margin, RichText, ScrollArea, Ui};
use egui_material_icons::icons;

use super::ops;
use crate::app::App;
use crate::model::config::{self, Shell};
use crate::theme;
use crate::ui::widgets;

/// Shown next to the command while "Check external links" is on: the command does not do it.
const EXTERNAL_NOTE: &str = "+ external links checked by the app";
const EXTERNAL_NOTE_TOOLTIP: &str = "Check external links is a setting of this app, not a crawler option: after the crawl the app requests each external link once. It is not part of the command.";

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = theme::palette(ui);
    let reveal = app.setup.reveal_secrets;
    let argv = app.run_config().to_preview_argv(&app.schema, &app.secrets, reveal);
    let line = config::command_line(config::PROGRAM_NAME, &argv, Shell::PowerShell);
    let changed = app.session.config.changed_count(&app.schema, &app.secrets);
    let has_secrets = !app.secrets.is_empty();
    let checks_links = app.session.config.check_external_links;

    ui.horizontal(|ui| {
        let open = app.setup.preview_open;
        let chevron = if open {
            icons::ICON_EXPAND_MORE
        } else {
            icons::ICON_CHEVRON_RIGHT
        };
        let toggle = ui
            .add(
                egui::Button::new((
                    chevron.rich_text().size(18.0).color(p.text_weak),
                    RichText::new("Command").color(p.text),
                ))
                .frame(false),
            )
            .accessible_name("Command preview")
            .on_hover_text(if open {
                "Hide the full command"
            } else {
                "Show the full command"
            });
        if toggle.clicked() {
            app.setup.preview_open = !open;
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let reset = ui
                .add_enabled(changed > 0, egui::Button::new("Reset all"))
                .accessible_name("Reset all")
                .on_disabled_hover_text("Every option is at its default");
            if reset.clicked() {
                ops::open_reset_all(app);
            }
            let mut copied = None;
            let (copy, _) = egui::containers::menu::MenuButton::new(widgets::with_icon(
                icons::ICON_CONTENT_COPY,
                "Copy",
            ))
            .ui(ui, |ui| {
                ui.set_min_width(200.0);
                if ui.button("Copy as PowerShell").clicked() {
                    copied = Some(("PowerShell command", ops::command_text(app, Shell::PowerShell, reveal)));
                }
                if ui.button("Copy as Bash").clicked() {
                    copied = Some(("Bash command", ops::command_text(app, Shell::Posix, reveal)));
                }
                if ui.button("Copy as config file").clicked() {
                    copied = Some(("Config file", ops::export_text(app, reveal).text));
                }
                if has_secrets {
                    ui.separator();
                    ui.label(widgets::weak(
                        ui,
                        if reveal {
                            "Passwords are included."
                        } else {
                            "Passwords are left out or masked."
                        },
                    ));
                }
            });
            let _ = copy
                .accessible_name("Copy")
                .on_hover_text("Copy the command or a config file");
            if let Some((what, text)) = copied {
                ui.ctx().copy_text(text);
                app.toasts.success(format!("{what} copied to the clipboard."));
            }
            if has_secrets {
                let (glyph, tip) = if reveal {
                    (icons::ICON_VISIBILITY_OFF, "Hide passwords in the command")
                } else {
                    (icons::ICON_VISIBILITY, "Show passwords in the command")
                };
                if widgets::icon_button(ui, glyph, tip).clicked() {
                    app.setup.reveal_secrets = !reveal;
                }
            }
            let count = match changed {
                0 => "No changes".to_string(),
                1 => "1 changed".to_string(),
                n => format!("{n} changed"),
            };
            ui.label(widgets::weak(ui, count));
            if checks_links {
                ui.label(widgets::weak(ui, EXTERNAL_NOTE))
                    .on_hover_text(EXTERNAL_NOTE_TOOLTIP);
            }
            ui.add_space(theme::GRID);
            if !open {
                // One line, cut at the right edge.
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(theme::mono(&line).color(p.text_weak))
                            .truncate()
                            .selectable(false),
                    )
                    .on_hover_text("Open the command preview to see it all");
                });
            }
        });
    });

    if app.setup.preview_open {
        ui.add_space(6.0);
        Frame::new()
            .fill(p.surface_alt)
            .corner_radius(theme::RADIUS)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ScrollArea::vertical()
                    .id_salt("command-preview")
                    .max_height(110.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(theme::mono(&line).color(p.text))
                                .selectable(true)
                                .wrap(),
                        );
                    });
            });
    }
}
