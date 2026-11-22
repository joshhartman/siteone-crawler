//! The control of one option, chosen from its schema entry (GUI-SPEC §5.1): toggle, numeric field
//! (range-checked), dropdown, check chips, text with a format hint, path with Browse…, a row
//! editor for lists, a password field with reveal for secrets. Each control edits exactly one CLI
//! option and is named for screen readers by the label the caller passes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use eframe::egui::{self, Response, TextEdit, Ui};
use egui_material_icons::icons;
use siteone_crawler::options::option_type::OptionType;

use crate::app::{FileDialogs, FileRequest};
use crate::model::config::{CrawlConfig, Value};
use crate::model::schema::{OptionSchema, OptionSpec, PathRole};
use crate::model::secrets::SecretValues;
use crate::theme;
use crate::ui::widgets::{self, Tone};

/// Editor state that is not part of the configuration.
#[derive(Debug, Default)]
pub struct EditorState {
    /// List rows being edited that contain blank rows (the configuration drops blank items), with
    /// the configured value they were based on — a draft is discarded when that value changes.
    drafts: HashMap<String, (Vec<String>, Value)>,
    /// Secret fields whose value is shown.
    revealed: HashSet<String>,
}

impl EditorState {
    pub fn clear(&mut self) {
        self.drafts.clear();
    }
}

/// What the controls edit.
pub struct Editing<'a> {
    pub schema: &'a OptionSchema,
    pub config: &'a mut CrawlConfig,
    pub secrets: &'a mut SecretValues,
    pub state: &'a mut EditorState,
    pub dialogs: &'a dyn FileDialogs,
}

impl Editing<'_> {
    pub fn value(&self, name: &str) -> Value {
        self.config.get(self.schema, name).unwrap_or(Value::Text(String::new()))
    }

    pub fn text(&self, name: &str) -> String {
        match self.value(name) {
            Value::Text(text) => text,
            Value::Bool(value) => value.to_string(),
            Value::List(items) => items.join(", "),
        }
    }

    pub fn flag(&self, name: &str) -> bool {
        matches!(self.value(name), Value::Bool(true))
    }

    pub fn is_changed(&self, spec: &OptionSpec) -> bool {
        if spec.secret {
            self.secrets.get(&spec.name).is_some()
        } else {
            self.config.is_changed(self.schema, &spec.name)
        }
    }

    pub fn set(&mut self, name: &str, value: Value) {
        // The schema decides what is settable; a refusal here is a programming error, not input.
        let _ = self.config.set(self.schema, name, value);
    }

    pub fn reset(&mut self, spec: &OptionSpec) {
        if spec.secret {
            self.secrets.remove(&spec.name);
        } else {
            self.config.reset(self.schema, &spec.name);
        }
        self.state.drafts.remove(&spec.name);
    }
}

/// A format hint for text fields.
pub fn hint(spec: &OptionSpec) -> String {
    match spec.path_role {
        Some(PathRole::Dir) => return "path to a folder".to_string(),
        Some(PathRole::InputFile | PathRole::OutputFile) => return "path to a file".to_string(),
        None => {}
    }
    let typed = match spec.kind {
        OptionType::SizeMG => "e.g. 512M or 1.5G",
        OptionType::Email => "name@example.com",
        OptionType::Url => "https://…",
        OptionType::Regex => "regular expression, e.g. /blog/",
        OptionType::HostAndPort => "host:port",
        OptionType::ReplaceContent => "foo -> bar   or   /regex/i -> bar",
        OptionType::Resolve => "domain:port:ip",
        OptionType::HttpHeader => "Name: value",
        OptionType::File => "path to a file",
        OptionType::Dir => "path to a folder",
        _ => "",
    };
    if !typed.is_empty() {
        return typed.to_string();
    }
    match &spec.default {
        Some(default) if !default.is_empty() => format!("default: {default}"),
        _ => "not set".to_string(),
    }
}

/// The problem with a numeric text, if any.
pub fn number_problem(spec: &OptionSpec, text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let value = match spec.kind {
        OptionType::Int => match text.parse::<i64>() {
            Ok(value) => value as f64,
            Err(_) => return Some("Enter a whole number.".to_string()),
        },
        OptionType::Float => match text.parse::<f64>() {
            Ok(value) if value.is_finite() => value,
            _ => return Some("Enter a number, e.g. 2.5.".to_string()),
        },
        _ => return None,
    };
    match spec.range {
        Some((min, max)) if value < min || value > max => Some(format!("Allowed: {min} to {max}.")),
        _ => None,
    }
}

/// The control of `spec`, named `label`, `width` wide (text fields).
pub fn control(ui: &mut Ui, edit: &mut Editing<'_>, spec: &OptionSpec, label: &str, width: f32) -> Response {
    if spec.secret {
        return secret_field(ui, edit, spec, label, width);
    }
    if spec.kind == OptionType::Bool && !spec.is_list {
        let mut on = edit.flag(&spec.name);
        let response = widgets::toggle(ui, &mut on, label);
        if response.changed() {
            edit.set(&spec.name, Value::Bool(on));
        }
        return response;
    }
    if let Some(choices) = spec.choices.clone() {
        return if choices.multiple {
            choice_chips(ui, edit, spec, &choices.values, label)
        } else {
            choice_dropdown(ui, edit, spec, &choices.values, label, width)
        };
    }
    if spec.is_list {
        return list_editor(ui, edit, spec, label, width);
    }
    match spec.kind {
        OptionType::Int | OptionType::Float => number_field(ui, edit, spec, label),
        _ => text_field(ui, edit, spec, label, width),
    }
}

fn number_field(ui: &mut Ui, edit: &mut Editing<'_>, spec: &OptionSpec, label: &str) -> Response {
    let mut text = edit.text(&spec.name);
    let response = ui
        .add(
            TextEdit::singleline(&mut text)
                .desired_width(120.0)
                .hint_text(hint(spec))
                .margin(egui::vec2(8.0, 5.0)),
        )
        .accessible_name(label);
    if response.changed() {
        if text.trim().is_empty() {
            // An empty number means "the default".
            edit.reset(spec);
        } else {
            edit.set(&spec.name, Value::Text(text.clone()));
        }
    }
    if let Some(problem) = number_problem(spec, &edit.text(&spec.name)) {
        widgets::inline_message(ui, Tone::Danger, &problem);
    }
    response
}

fn text_field(ui: &mut Ui, edit: &mut Editing<'_>, spec: &OptionSpec, label: &str, width: f32) -> Response {
    let mut text = edit.text(&spec.name);
    let browse = spec.path_role.is_some();
    ui.horizontal(|ui| {
        let field_width = if browse { (width - 92.0).max(120.0) } else { width };
        let response = ui
            .add(
                TextEdit::singleline(&mut text)
                    .desired_width(field_width)
                    .hint_text(hint(spec))
                    .margin(egui::vec2(8.0, 5.0)),
            )
            .accessible_name(label);
        if response.changed() {
            edit.set(&spec.name, Value::Text(text.clone()));
        }
        if let Some(role) = spec.path_role
            && ui
                .add(egui::Button::new("Browse…"))
                .accessible_name(format!("Browse for {label}"))
                .clicked()
            && let Some(path) = browse_path(edit.dialogs, role, label, &text)
        {
            edit.set(&spec.name, Value::Text(path.display().to_string()));
        }
        response
    })
    .inner
}

/// Opens the dialog of a path role, starting where the current value points.
pub fn browse_path(dialogs: &dyn FileDialogs, role: PathRole, label: &str, current: &str) -> Option<PathBuf> {
    let current = Path::new(current.trim());
    let directory = if current.is_dir() {
        Some(current.to_path_buf())
    } else {
        current.parent().map(Path::to_path_buf)
    };
    let mut request = FileRequest::new(label).in_directory(directory);
    if let Some(name) = current.file_name().filter(|_| role == PathRole::OutputFile) {
        request = request.file_name(name.to_string_lossy());
    }
    match role {
        PathRole::Dir => dialogs.pick_folder(&request),
        PathRole::OutputFile => dialogs.save_file(&request),
        PathRole::InputFile => dialogs.open_file(&request),
    }
}

fn secret_field(ui: &mut Ui, edit: &mut Editing<'_>, spec: &OptionSpec, label: &str, width: f32) -> Response {
    let mut text = edit.secrets.get(&spec.name).unwrap_or_default().to_string();
    let revealed = edit.state.revealed.contains(&spec.name);
    ui.horizontal(|ui| {
        let response = ui
            .add(
                TextEdit::singleline(&mut text)
                    .password(!revealed)
                    .desired_width((width - 36.0).max(120.0))
                    .hint_text(match spec.name.as_str() {
                        "--http-auth" => "user:password",
                        "--ai-api-key" => "stored in Windows Credential Manager",
                        _ => "password",
                    })
                    .margin(egui::vec2(8.0, 5.0)),
            )
            .accessible_name(label);
        if response.changed() {
            edit.secrets.set(&spec.name, &text);
        }
        let (glyph, tip) = if revealed {
            (icons::ICON_VISIBILITY_OFF, format!("Hide {label}"))
        } else {
            (icons::ICON_VISIBILITY, format!("Show {label}"))
        };
        if widgets::icon_button(ui, glyph, &tip).clicked() {
            if revealed {
                edit.state.revealed.remove(&spec.name);
            } else {
                edit.state.revealed.insert(spec.name.clone());
            }
        }
        response
    })
    .inner
}

fn choice_dropdown(
    ui: &mut Ui,
    edit: &mut Editing<'_>,
    spec: &OptionSpec,
    values: &[String],
    label: &str,
    width: f32,
) -> Response {
    let current = edit.text(&spec.name);
    let unset = current.is_empty();
    let shown = if unset { "Not set".to_string() } else { current.clone() };
    let mut chosen: Option<String> = None;
    let response = egui::ComboBox::from_id_salt(("choice", &spec.name))
        .width(width.min(260.0))
        .selected_text(shown)
        .show_ui(ui, |ui| {
            if spec.nullable
                && spec.default.as_deref().unwrap_or_default().is_empty()
                && ui.selectable_label(unset, "Not set").clicked()
            {
                chosen = Some(String::new());
            }
            for value in values {
                let mut text = value.clone();
                if spec.default.as_deref() == Some(value.as_str()) {
                    text.push_str("  (default)");
                }
                if ui.selectable_label(current == *value, text).clicked() {
                    chosen = Some(value.clone());
                }
            }
            if !unset && !values.contains(&current) && ui.selectable_label(true, &current).clicked() {
                chosen = Some(current.clone());
            }
        })
        .response
        .accessible_name(label);
    if let Some(value) = chosen {
        edit.set(&spec.name, Value::Text(value));
    }
    response
}

fn choice_chips(ui: &mut Ui, edit: &mut Editing<'_>, spec: &OptionSpec, values: &[String], label: &str) -> Response {
    // List options keep one item per value; text options join the values with commas.
    let current: Vec<String> = match edit.value(&spec.name) {
        Value::List(items) => items,
        Value::Text(text) => text
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        Value::Bool(_) => Vec::new(),
    };
    let mut next = current.clone();
    let response = ui
        .horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            let mut first: Option<Response> = None;
            for value in values {
                let mut selected = current.contains(value);
                let response = widgets::check_chip(ui, &mut selected, value);
                if response.clicked() {
                    if selected {
                        next.push(value.clone());
                    } else {
                        next.retain(|item| item != value);
                    }
                }
                first.get_or_insert(response);
            }
            // Values the GUI does not know (from an import) stay visible and removable.
            for extra in current.iter().filter(|item| !values.contains(item)) {
                let mut selected = true;
                if widgets::check_chip(ui, &mut selected, extra).clicked() {
                    next.retain(|item| item != extra);
                }
            }
            first
        })
        .inner;
    if next != current {
        // Keep the order of the value set.
        let mut ordered: Vec<String> = values.iter().filter(|value| next.contains(value)).cloned().collect();
        ordered.extend(next.iter().filter(|item| !values.contains(item)).cloned());
        let value = if spec.is_list {
            Value::List(ordered)
        } else {
            Value::Text(ordered.join(","))
        };
        edit.set(&spec.name, value);
    }
    let response = response.unwrap_or_else(|| ui.label(""));
    response.on_hover_text(label)
}

fn list_editor(ui: &mut Ui, edit: &mut Editing<'_>, spec: &OptionSpec, label: &str, width: f32) -> Response {
    let configured = edit.value(&spec.name);
    let base_items = match &configured {
        Value::List(items) => items.clone(),
        _ => Vec::new(),
    };
    let mut rows = match edit.state.drafts.get(&spec.name) {
        Some((draft, base)) if *base == configured => draft.clone(),
        _ => base_items.clone(),
    };
    let mut changed = false;
    let mut remove = None;
    let mut first: Option<Response> = None;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        for (index, row) in rows.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let response = ui
                    .add(
                        TextEdit::singleline(row)
                            .desired_width((width - 36.0).max(120.0))
                            .hint_text(hint(spec))
                            .margin(egui::vec2(8.0, 5.0)),
                    )
                    .accessible_name(format!("{label} {}", index + 1));
                changed |= response.changed();
                first.get_or_insert(response);
                if widgets::icon_button(ui, icons::ICON_CLOSE, &format!("Remove {label} {}", index + 1)).clicked() {
                    remove = Some(index);
                }
            });
        }
        let add = ui
            .add(egui::Button::new(widgets::with_icon(icons::ICON_ADD, "Add")))
            .accessible_name(format!("Add {label}"));
        if add.clicked() {
            rows.push(String::new());
            changed = true;
        }
        if rows.is_empty() {
            first.get_or_insert(add);
        }
    });
    if let Some(index) = remove
        && index < rows.len()
    {
        rows.remove(index);
        changed = true;
    }
    if changed {
        edit.set(&spec.name, Value::List(rows.clone()));
        let now = edit.value(&spec.name);
        if rows.iter().any(|row| row.trim().is_empty()) {
            edit.state.drafts.insert(spec.name.clone(), (rows, now));
        } else {
            edit.state.drafts.remove(&spec.name);
        }
    }
    let response = first.unwrap_or_else(|| ui.label(""));
    response.on_hover_text(label)
}

/// The default of an option for display (`Default: 3`, `Default: off`, `No default`).
pub fn default_text(spec: &OptionSpec) -> String {
    match &spec.default_value {
        Value::Bool(true) => "Default: on".to_string(),
        Value::Bool(false) => "Default: off".to_string(),
        Value::List(items) if items.is_empty() => "Default: none".to_string(),
        Value::List(items) => format!("Default: {}", items.join(", ")),
        Value::Text(text) if text.is_empty() => "No default".to_string(),
        Value::Text(text) => format!("Default: {text}"),
    }
}

/// The colour for an option's name: accent when changed.
pub fn name_color(ui: &Ui, changed: bool) -> egui::Color32 {
    let p = theme::palette(ui);
    if changed { p.accent } else { p.text }
}
