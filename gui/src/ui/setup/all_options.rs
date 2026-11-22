//! The All options view (GUI-SPEC §6.2): the groups with their changed counts on the left, the
//! options of the selected group on the right — name, control, default, the CLI's help text, a
//! reset button and a changed marker. The search filters across all groups by name and
//! description (results grouped, matches highlighted); "Only changed" lists the overrides.

use eframe::egui::{
    self, Align, CentralPanel, Frame, Layout, Margin, Panel, RichText, Role, ScrollArea, Sense, Stroke, Ui, WidgetInfo,
};
use egui_material_icons::icons;

use super::PAGE_MARGIN;
use super::editor::{self, Editing};
use crate::app::App;
use crate::model::schema::{self, OptionSpec};
use crate::theme;
use crate::ui::widgets::{self, Tone, format};

const GROUP_LIST_WIDTH: f32 = 244.0;

/// Whether `spec` matches the search `query` (name, alternative name or description).
pub fn matches(spec: &OptionSpec, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    spec.name.to_lowercase().contains(&query)
        || spec
            .alt_name
            .as_deref()
            .is_some_and(|alt| alt.to_lowercase().contains(&query))
        || spec.description.to_lowercase().contains(&query)
}

fn is_changed(app: &App, spec: &OptionSpec) -> bool {
    if spec.secret {
        app.secrets.get(&spec.name).is_some()
    } else {
        app.session.config.values.contains_key(&spec.name)
    }
}

/// The options a group lists: editable ones, filtered by the search and "Only changed".
fn listed<'a>(app: &'a App, group: &'a schema::GroupSpec) -> Vec<&'a OptionSpec> {
    let query = app.setup.search.trim();
    app.schema
        .group_options(group)
        .filter(|spec| spec.is_editable())
        .filter(|spec| matches(spec, query))
        .filter(|spec| !app.setup.only_changed || is_changed(app, spec))
        .collect()
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = theme::palette(ui);
    let flat = !app.setup.search.trim().is_empty() || app.setup.only_changed;
    if app.setup.group.is_none() {
        app.setup.group = app.schema.groups().first().map(|group| group.code.clone());
    }

    Panel::left("setup-groups")
        .exact_size(GROUP_LIST_WIDTH + f32::from(PAGE_MARGIN))
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new().fill(p.bg).inner_margin(Margin {
            left: PAGE_MARGIN,
            right: 8,
            top: 4,
            bottom: 12,
        }))
        .show(ui, |ui| group_list(app, ui, flat));

    CentralPanel::default()
        .frame(Frame::new().fill(p.bg).inner_margin(Margin {
            left: 8,
            right: PAGE_MARGIN,
            top: 4,
            bottom: 12,
        }))
        .show(ui, |ui| {
            Frame::new()
                .fill(p.surface)
                .stroke(Stroke::new(1.0, p.border))
                .corner_radius(theme::CARD_RADIUS)
                .inner_margin(Margin::same(0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_height(ui.available_height());
                    option_list(app, ui, flat);
                });
        });
}

fn group_list(app: &mut App, ui: &mut Ui, flat: bool) {
    let p = theme::palette(ui);
    let searching = !app.setup.search.trim().is_empty();
    ScrollArea::vertical()
        .id_salt("setup-group-list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let mut select = None;
            for group in app.schema.groups() {
                let editable = app
                    .schema
                    .group_options(group)
                    .filter(|spec| spec.is_editable())
                    .count();
                if editable == 0 {
                    continue;
                }
                let changed = app
                    .schema
                    .group_options(group)
                    .filter(|spec| spec.is_editable() && is_changed(app, spec))
                    .count();
                let shown = listed(app, group).len();
                let dimmed = flat && shown == 0;
                let selected = !flat && app.setup.group.as_deref() == Some(group.code.as_str());
                let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), Sense::click());
                let label = &group.name;
                response.widget_info(|| WidgetInfo::selected(Role::Button, !dimmed, selected, label));
                if ui.is_rect_visible(rect) {
                    if selected {
                        ui.painter().rect_filled(rect, theme::RADIUS, p.accent_soft);
                    } else if response.hovered() && !dimmed {
                        ui.painter().rect_filled(rect, theme::RADIUS, p.surface_alt);
                    }
                    let color = if dimmed {
                        p.text_weak.gamma_multiply(0.6)
                    } else if selected {
                        p.accent
                    } else {
                        p.text
                    };
                    let font = if selected {
                        theme::semibold_font(13.5)
                    } else {
                        egui::FontId::proportional(13.5)
                    };
                    let text_rect = rect.shrink2(egui::vec2(10.0, 0.0));
                    let galley = widgets::one_line(ui, label, font, color, text_rect.width() - 32.0);
                    let galley_pos = egui::pos2(text_rect.left(), rect.center().y - galley.size().y / 2.0);
                    ui.painter().galley(galley_pos, galley, color);
                    // The count: matches while searching, otherwise changed options.
                    let count = if searching { shown } else { changed };
                    if count > 0 {
                        let text = format::count(count as u64);
                        let galley = ui.painter().layout_no_wrap(
                            text,
                            egui::FontId::proportional(11.5),
                            if searching { p.text } else { p.on_accent },
                        );
                        let size = egui::vec2((galley.size().x + 12.0).max(20.0), 18.0);
                        let badge = egui::Rect::from_center_size(
                            egui::pos2(text_rect.right() - size.x / 2.0, rect.center().y),
                            size,
                        );
                        let fill = if searching { p.surface_alt } else { p.accent_fill };
                        ui.painter().rect_filled(badge, 9.0, fill);
                        ui.painter()
                            .galley(badge.center() - galley.size() / 2.0, galley, p.text);
                    }
                }
                let response = if changed > 0 {
                    response.on_hover_text(format!("{changed} changed"))
                } else {
                    response
                };
                if response.clicked() && !dimmed {
                    select = Some(group.code.clone());
                }
            }
            if let Some(code) = select {
                if flat {
                    app.setup.scroll_to_group = Some(code);
                } else {
                    app.setup.group = Some(code);
                }
            }
        });
}

fn option_list(app: &mut App, ui: &mut Ui, flat: bool) {
    let p = theme::palette(ui);
    let query = app.setup.search.trim().to_string();

    // Header: title, count, Only changed.
    Frame::new().inner_margin(Margin::symmetric(16, 10)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let title = if !query.is_empty() {
                format!("Results for “{query}”")
            } else if app.setup.only_changed {
                "Changed options".to_string()
            } else {
                app.setup
                    .group
                    .as_deref()
                    .and_then(|code| app.schema.groups().iter().find(|group| group.code == code))
                    .map(|group| group.name.clone())
                    .unwrap_or_default()
            };
            ui.label(theme::section_title(title));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                widgets::toggle_labeled(ui, &mut app.setup.only_changed, "Only changed");
            });
        });
    });
    widgets::hairline(ui);

    let groups: Vec<(String, String, Vec<OptionSpec>)> = app
        .schema
        .groups()
        .iter()
        .filter(|group| flat || app.setup.group.as_deref() == Some(group.code.as_str()))
        .map(|group| {
            let options = listed(app, group).into_iter().cloned().collect::<Vec<_>>();
            (group.code.clone(), group.name.clone(), options)
        })
        .filter(|(_, _, options)| !options.is_empty() || !flat)
        .collect();
    let total: usize = groups.iter().map(|(_, _, options)| options.len()).sum();

    let scroll_id = if flat {
        "setup-options-flat".to_string()
    } else {
        format!("setup-options-{}", app.setup.group.clone().unwrap_or_default())
    };
    ScrollArea::vertical()
        .id_salt(scroll_id)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::new().inner_margin(Margin::symmetric(16, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if total == 0 {
                    let (title, text) = if !query.is_empty() {
                        (
                            format!("No option matches “{query}”"),
                            "Search looks in option names and their descriptions. Try a shorter or different word.",
                        )
                    } else if app.setup.only_changed {
                        (
                            "No option is changed".to_string(),
                            "Every option is at its default. Changed options are listed here.",
                        )
                    } else {
                        ("This group has no editable options".to_string(), "")
                    };
                    widgets::empty_state(ui, icons::ICON_SEARCH, &title, text, None);
                    return;
                }
                if flat {
                    ui.label(widgets::weak(ui, format::plural(total as u64, "option")));
                    ui.add_space(4.0);
                }
                for (code, name, options) in &groups {
                    if flat {
                        ui.add_space(theme::GRID);
                        let heading = ui.label(theme::semibold(name).size(13.0).color(p.text_weak));
                        if app.setup.scroll_to_group.as_deref() == Some(code.as_str()) {
                            heading.scroll_to_me(Some(Align::TOP));
                            app.setup.scroll_to_group = None;
                        }
                        ui.add_space(2.0);
                    }
                    if code == siteone_crawler::options::core_options::GROUP_AI_SETTINGS
                        && !app.session.config.ai_enabled
                    {
                        let turn_on = widgets::banner(
                            ui,
                            Tone::Info,
                            "AI analysis is off: these settings are kept but not used.",
                            |ui| {
                                ui.add(egui::Button::new("Turn AI on"))
                                    .accessible_name("Turn AI on")
                                    .clicked()
                            },
                        );
                        if turn_on {
                            app.session.config.ai_enabled = true;
                        }
                        ui.add_space(theme::GRID);
                    }
                    for (index, spec) in options.iter().enumerate() {
                        if index > 0 {
                            widgets::hairline(ui);
                        }
                        option_row(app, ui, spec, &query);
                    }
                }
                ui.add_space(theme::GRID * 2.0);
                managed_note(ui);
            });
        });
}

fn option_row(app: &mut App, ui: &mut Ui, spec: &OptionSpec, query: &str) {
    let p = theme::palette(ui);
    let changed = is_changed(app, spec);
    let top = ui.cursor().top();
    let left = ui.cursor().left();
    ui.add_space(10.0);
    ui.horizontal_top(|ui| {
        let total = ui.available_width();
        let info_width = (total * 0.52).clamp(240.0, 560.0);
        let control_width = (total - info_width - 48.0).max(180.0);

        ui.allocate_ui_with_layout(egui::vec2(info_width, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(info_width);
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let mono = egui::FontId::monospace(13.0);
                let name_color = editor::name_color(ui, changed);
                let name = widgets::highlighted(&spec.name, query, mono.clone(), name_color, p.highlight);
                ui.add(egui::Label::new(name).selectable(true));
                if let Some(alt) = &spec.alt_name {
                    ui.label(widgets::highlighted(alt, query, mono, p.text_weak, p.highlight));
                }
                if spec.secret {
                    ui.label(icons::ICON_LOCK.rich_text().size(14.0).color(p.text_weak))
                        .on_hover_text("Stored in the Windows Credential Manager, never in a file");
                }
            });
            let description = widgets::described(&spec.description, query, 13.0, p);
            ui.add(egui::Label::new(description).wrap());
            ui.label(RichText::new(editor::default_text(spec)).size(12.0).color(p.text_weak));
        });
        ui.add_space(16.0);
        ui.allocate_ui_with_layout(egui::vec2(control_width, 0.0), Layout::top_down(Align::Min), |ui| {
            let App {
                schema,
                session,
                secrets,
                setup,
                dialogs,
                ..
            } = app;
            let mut edit = Editing {
                schema,
                config: &mut session.config,
                secrets,
                state: &mut setup.editor,
                dialogs: &**dialogs,
            };
            // The reset button sits right after the control.
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    editor::control(ui, &mut edit, spec, &spec.name, control_width - 40.0);
                });
                if changed && widgets::icon_button(ui, icons::ICON_UNDO, &format!("Reset {}", spec.name)).clicked() {
                    edit.reset(spec);
                }
            });
        });
    });
    ui.add_space(10.0);
    if changed {
        let bottom = ui.cursor().top();
        let bar = egui::Rect::from_min_max(
            egui::pos2(left - 10.0, top + 10.0),
            egui::pos2(left - 7.0, bottom - 10.0),
        );
        ui.painter().rect_filled(bar, 1.5, p.accent);
    }
}

/// The note about the options the GUI sets itself.
fn managed_note(ui: &mut Ui) {
    let p = theme::palette(ui);
    Frame::new()
        .fill(p.surface_alt)
        .corner_radius(theme::RADIUS)
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.label(icons::ICON_INFO.rich_text().size(16.0).color(p.text_weak));
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "Set by the app for every run, so not listed: {}. The URL and the API key have their own fields; the utility modes (serve, convert, list models) are not crawl settings. While Skip cart and action links is on (Simple view, Scope), the run adds its patterns to --ignore-regex after the ones listed here.",
                            schema::MANAGED.join(", ")
                        ))
                        .size(12.5)
                        .color(p.text_weak),
                    )
                    .wrap(),
                );
            });
        });
}
