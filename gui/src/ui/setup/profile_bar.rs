//! The profile bar: select a profile, Save, Save as…, and a menu with Rename, Delete, Import config
//! file…, Paste command line…, Export config file…, Reset to defaults. A dot marks unsaved changes.

use eframe::egui::{self, RichText, Ui};
use egui_material_icons::icons;

use super::ops;
use crate::app::App;
use crate::theme;
use crate::ui::widgets;

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = theme::palette(ui);
    let mut chosen: Option<Option<String>> = None;
    ui.horizontal(|ui| {
        let label = ui.label(widgets::weak(ui, "Profile"));
        let selected = app
            .profile_name()
            .map(str::to_string)
            .unwrap_or_else(|| "No profile".to_string());
        let current = app.session.profile.clone();
        egui::ComboBox::from_id_salt("profile-select")
            .width(240.0)
            .selected_text(selected)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(current.is_none(), RichText::new("No profile").italics())
                    .on_hover_text("Keep the current values without a profile")
                    .clicked()
                {
                    chosen = Some(None);
                }
                if !app.profiles.is_empty() {
                    ui.separator();
                }
                for item in &app.profiles {
                    let is_current = current.as_deref() == Some(item.slug.as_str());
                    match &item.error {
                        None => {
                            if ui.selectable_label(is_current, &item.name).clicked() {
                                chosen = Some(Some(item.slug.clone()));
                            }
                        }
                        Some(error) => {
                            ui.add_enabled(
                                false,
                                egui::Button::selectable(false, format!("{} (damaged)", item.name)),
                            )
                            .on_disabled_hover_text(format!("This profile cannot be read: {error}"));
                        }
                    }
                }
            })
            .response
            .labelled_by(label.id);

        let has_profile = app.session.profile.is_some();
        let modified = app.is_modified();
        let save_enabled = !has_profile || modified;
        let save = ui
            .add_enabled(
                save_enabled,
                egui::Button::new(widgets::with_icon(icons::ICON_SAVE, "Save")),
            )
            .accessible_name("Save")
            .on_hover_text(if has_profile {
                "Save the changes into this profile"
            } else {
                "Save the configuration as a new profile"
            })
            .on_disabled_hover_text("No unsaved changes");
        if save.clicked() {
            ops::save(app);
        }
        let save_as = ui.add(egui::Button::new("Save as…")).accessible_name("Save as");
        if save_as.clicked() {
            ops::open_save_as(app);
        }
        // As high as the text buttons beside it: the glyph is no taller than their text, and the
        // minimum size gives the height of "Save as…".
        let more = egui::Button::new(icons::ICON_MORE_HORIZ.rich_text().size(14.0))
            .min_size(egui::vec2(save_as.rect.height() + 4.0, save_as.rect.height()));
        let (menu_button, _) = egui::containers::menu::MenuButton::from_button(more).ui(ui, |ui| {
            ui.set_min_width(220.0);
            if ui
                .add_enabled(has_profile, egui::Button::new("Rename…"))
                .on_disabled_hover_text("Select a profile first")
                .clicked()
            {
                ops::open_rename(app);
            }
            if ui
                .add_enabled(has_profile, egui::Button::new("Delete…"))
                .on_disabled_hover_text("Select a profile first")
                .clicked()
            {
                ops::open_delete(app);
            }
            ui.separator();
            if ui.button("Import config file…").clicked() {
                ops::start_import_config_file(app);
            }
            if ui.button("Paste command line…").clicked() {
                ops::start_paste_command_line(app);
            }
            if ui.button("Export config file…").clicked() {
                ops::open_export(app);
            }
            ui.separator();
            if ui.button("Reset to defaults…").clicked() {
                ops::open_reset_all(app);
            }
        });
        let _ = menu_button
            .accessible_name("More profile actions")
            .on_hover_text("More profile actions");

        if modified {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(widgets::weak(
                    ui,
                    if has_profile {
                        "Unsaved changes"
                    } else {
                        "Not saved in a profile"
                    },
                ));
                let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, p.warning);
            });
        }
    });
    if let Some(slug) = chosen {
        ops::select_profile(app, slug);
    }
}
