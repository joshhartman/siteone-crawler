//! What the Setup page does to the working configuration: profiles (load, save, save as, rename,
//! delete), presets, imports (config file, pasted command line), export and reset. Every operation
//! that would discard unsaved changes asks first.

use std::path::PathBuf;

use crate::app::{Action, App, Baseline, FileRequest, confirm};
use crate::model::config::{self, CrawlConfig, Imported, Shell};
use crate::model::presets;
use crate::model::profiles::{self, Profile};
use crate::model::secrets::SecretValues;
use crate::ui::widgets::Tone;
use crate::ui::widgets::modal::{ButtonKind, Dialog};

/// Performs a Setup action chosen in a dialog.
pub fn perform(app: &mut App, action: Action, dialog: Option<Dialog<Action>>) {
    match action {
        Action::LoadProfile(slug) => load_profile(app, &slug),
        Action::ApplyPreset(index) => apply_preset(app, index),
        Action::SaveProfileAs => {
            let Some(dialog) = dialog else { return };
            save_as(app, dialog);
        }
        Action::OverwriteProfile(name) => save_named(app, &name),
        Action::RenameProfile => {
            let Some(dialog) = dialog else { return };
            rename(app, dialog);
        }
        Action::DeleteProfile => delete_profile(app),
        Action::ImportConfigFile => import_config_file(app),
        Action::PasteCommandLine => open_paste_dialog(app),
        Action::ImportCommandLine(shell) => {
            let Some(dialog) = dialog else { return };
            import_command_line(app, dialog, shell);
        }
        Action::ExportConfigFile => {
            let include_secrets = dialog.as_ref().is_some_and(Dialog::checked);
            export_config_file(app, include_secrets);
        }
        Action::ResetAll => reset_all(app),
        Action::ShowPage(_)
        | Action::LocateCrawler
        | Action::ClearCaches
        | Action::ForceQuit
        | Action::StopAndClose
        | Action::RunAgain(_)
        | Action::DeleteRun(_) => {}
    }
}

/// Runs `action` now, or after a confirmation when there are unsaved changes.
fn after_confirming_unsaved(app: &mut App, what: &str, action: Action) {
    if app.is_modified() {
        app.open_dialog(confirm(
            "Discard unsaved changes?",
            format!("The current configuration has changes that are not saved in a profile. {what}"),
            "Discard and continue",
            ButtonKind::Danger,
            action,
        ));
    } else {
        perform(app, action, None);
    }
}

// ---------------------------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------------------------

/// The profile combo box chose `slug` (`None` = no profile).
pub fn select_profile(app: &mut App, slug: Option<String>) {
    if slug == app.session.profile {
        return;
    }
    match slug {
        None => {
            // Detach: the values stay, they are just no longer tied to a profile.
            app.session.profile = None;
            app.baseline = None;
        }
        Some(slug) => after_confirming_unsaved(app, "Loading the profile replaces them.", Action::LoadProfile(slug)),
    }
}

fn load_profile(app: &mut App, slug: &str) {
    let dir = app.paths.profiles_dir();
    match profiles::load(&dir, slug, &app.schema) {
        Ok((profile, dropped)) => {
            let (secrets, errors) = SecretValues::load(app.secret_store.as_ref(), slug, app.schema.secret_options());
            let mut config = profile.config.clone();
            if config.url.trim().is_empty() {
                config.url = app.session.config.url.clone();
            }
            app.session.config = config;
            app.session.profile = Some(slug.to_string());
            app.secrets = secrets.clone();
            app.baseline = Some(Baseline {
                config: app.session.config.clone(),
                secrets,
            });
            app.setup.editor.clear();
            app.setup.start_error = None;
            if !dropped.is_empty() {
                app.toasts.warning(format!(
                    "Some values of “{}” no longer apply and were left out: {}.",
                    profile.name,
                    dropped.join("; ")
                ));
            }
            if let Some(error) = errors.first() {
                app.toasts
                    .warning(format!("The passwords of this profile could not be read ({error})."));
            }
        }
        Err(error) => {
            app.toasts.error(format!("The profile could not be loaded: {error}"));
            app.refresh_profiles();
        }
    }
}

/// Save: into the selected profile, or Save as… without one.
pub fn save(app: &mut App) {
    match app.profile_name().map(str::to_string) {
        Some(name) => save_named(app, &name),
        None => open_save_as(app),
    }
}

pub fn open_save_as(app: &mut App) {
    let suggestion = app
        .profile_name()
        .map(|name| format!("{name} copy"))
        .unwrap_or_else(|| suggest_name(&app.session.config));
    app.open_dialog(
        Dialog::new("Save as a profile")
            .message("A profile keeps the URL and every setting. Passwords go to the Windows Credential Manager.")
            .input("Profile name", suggestion, "e.g. Production site")
            .button("Save profile", ButtonKind::Primary, Action::SaveProfileAs)
            .cancel("Cancel"),
    );
}

fn suggest_name(config: &CrawlConfig) -> String {
    url::Url::parse(config.url.trim())
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default()
}

fn save_as(app: &mut App, mut dialog: Dialog<Action>) {
    let name = dialog.input_text();
    if name.is_empty() {
        dialog.error = Some("Enter a name.".to_string());
        app.open_dialog(dialog);
        return;
    }
    let slug = profiles::slugify(&name);
    if profiles::exists(&app.paths.profiles_dir(), &slug) && app.session.profile.as_deref() != Some(slug.as_str()) {
        let existing = app
            .profiles
            .iter()
            .find(|item| item.slug == slug)
            .map(|item| item.name.clone())
            .unwrap_or(slug);
        app.open_dialog(confirm(
            "Replace the profile?",
            format!("A profile named “{existing}” exists. Saving replaces it."),
            "Replace",
            ButtonKind::Danger,
            Action::OverwriteProfile(name),
        ));
        return;
    }
    save_named(app, &name);
}

fn save_named(app: &mut App, name: &str) {
    let profile = Profile {
        name: name.to_string(),
        config: app.session.config.clone(),
    };
    let dir = app.paths.profiles_dir();
    match profiles::save(&dir, &profile, &app.schema) {
        Ok(slug) => {
            let stored = app
                .secrets
                .save(app.secret_store.as_ref(), &slug, app.schema.secret_options());
            if let Err(error) = stored {
                app.toasts
                    .warning(format!("The profile was saved, but its passwords were not: {error}"));
            }
            app.session.profile = Some(slug);
            app.baseline = Some(Baseline {
                config: app.session.config.clone(),
                secrets: app.secrets.clone(),
            });
            app.refresh_profiles();
            app.toasts.success(format!("Profile “{}” saved.", name.trim()));
        }
        Err(error) => app.toasts.error(format!("The profile could not be saved: {error}")),
    }
}

pub fn open_rename(app: &mut App) {
    let Some(name) = app.profile_name().map(str::to_string) else {
        return;
    };
    app.open_dialog(
        Dialog::new("Rename profile")
            .input("New name", name, "Profile name")
            .button("Rename", ButtonKind::Primary, Action::RenameProfile)
            .cancel("Cancel"),
    );
}

fn rename(app: &mut App, mut dialog: Dialog<Action>) {
    let Some(slug) = app.session.profile.clone() else {
        return;
    };
    let name = dialog.input_text();
    if name.is_empty() {
        dialog.error = Some("Enter a name.".to_string());
        app.open_dialog(dialog);
        return;
    }
    let new_slug = profiles::slugify(&name);
    let dir = app.paths.profiles_dir();
    if new_slug != slug && profiles::exists(&dir, &new_slug) {
        dialog.error = Some("Another profile already has this name.".to_string());
        app.open_dialog(dialog);
        return;
    }
    match profiles::rename(&dir, &slug, &name, &app.schema) {
        Ok(new_slug) => {
            if new_slug != slug {
                let options: Vec<String> = app.schema.secret_options().map(str::to_string).collect();
                let options = || options.iter().map(String::as_str);
                let (secrets, _) = SecretValues::load(app.secret_store.as_ref(), &slug, options());
                let moved = secrets
                    .save(app.secret_store.as_ref(), &new_slug, options())
                    .and_then(|()| SecretValues::delete_scope(app.secret_store.as_ref(), &slug, options()));
                if let Err(error) = moved {
                    app.toasts
                        .warning(format!("The passwords of the profile could not be moved: {error}"));
                }
            }
            app.session.profile = Some(new_slug);
            app.refresh_profiles();
            app.toasts.success(format!("Profile renamed to “{name}”."));
        }
        Err(error) => app.toasts.error(format!("The profile could not be renamed: {error}")),
    }
}

pub fn open_delete(app: &mut App) {
    let Some(name) = app.profile_name().map(str::to_string) else {
        return;
    };
    app.open_dialog(confirm(
        "Delete profile?",
        format!(
            "“{name}” and its saved passwords will be deleted. The current form keeps its values without a profile."
        ),
        "Delete",
        ButtonKind::Danger,
        Action::DeleteProfile,
    ));
}

fn delete_profile(app: &mut App) {
    let Some(slug) = app.session.profile.clone() else {
        return;
    };
    match profiles::delete(&app.paths.profiles_dir(), &slug) {
        Ok(()) => {
            if let Err(error) =
                SecretValues::delete_scope(app.secret_store.as_ref(), &slug, app.schema.secret_options())
            {
                app.toasts
                    .warning(format!("The profile's passwords could not be deleted: {error}"));
            }
            app.session.profile = None;
            app.baseline = None;
            app.refresh_profiles();
            app.toasts.success("Profile deleted.");
        }
        Err(error) => app.toasts.error(format!("The profile could not be deleted: {error}")),
    }
}

// ---------------------------------------------------------------------------------------------
// Presets and reset
// ---------------------------------------------------------------------------------------------

/// A preset card was clicked. Asks first when hand-made changes would be lost.
pub fn choose_preset(app: &mut App, index: usize) {
    let Some(preset) = app.presets.get(index) else { return };
    let hand_made = presets::matching(&app.session.config, &app.presets, &app.schema).is_none()
        && app
            .session
            .config
            .values
            .keys()
            .any(|name| !app.schema.get(name).is_some_and(|spec| spec.is_ai()));
    if app.is_modified() && hand_made {
        let name = preset.name;
        app.open_dialog(confirm(
            &format!("Apply “{name}”?"),
            "The preset replaces your changed settings (the URL and the AI settings stay).",
            "Apply preset",
            ButtonKind::Danger,
            Action::ApplyPreset(index),
        ));
    } else {
        apply_preset(app, index);
    }
}

fn apply_preset(app: &mut App, index: usize) {
    let Some(preset) = app.presets.get(index).cloned() else {
        return;
    };
    presets::apply(&mut app.session.config, &preset, &app.schema);
    app.setup.editor.clear();
    app.setup.start_error = None;
}

pub fn open_reset_all(app: &mut App) {
    app.open_dialog(confirm(
        "Reset all options?",
        "Every option goes back to its default, passwords included (the API key stays). The URL is kept.",
        "Reset all",
        ButtonKind::Danger,
        Action::ResetAll,
    ));
}

fn reset_all(app: &mut App) {
    app.session.config.reset_all();
    let api_key = app.secrets.get(config::AI_API_KEY).map(str::to_string);
    app.secrets = SecretValues::default();
    if let Some(key) = api_key {
        app.secrets.set(config::AI_API_KEY, &key);
    }
    app.setup.editor.clear();
    app.setup.start_error = None;
}

// ---------------------------------------------------------------------------------------------
// Import and export
// ---------------------------------------------------------------------------------------------

pub fn start_import_config_file(app: &mut App) {
    after_confirming_unsaved(app, "Importing replaces them.", Action::ImportConfigFile);
}

fn import_config_file(app: &mut App) {
    let request = FileRequest::new("Import a config file")
        .filter("Config files", &["conf", "txt", "cfg"])
        .filter("All files", &["*"]);
    let Some(path) = app.dialogs.open_file(&request) else {
        return;
    };
    app.spawn_task(move || {
        let result = read_text(&path);
        Box::new(move |app: &mut App| match result {
            Ok(text) => {
                let args = config::parse_config_file(&text);
                let mut imported = config::import_args(&app.schema, &args);
                // A relative input path in the file names a file next to it (GUI-SPEC §5.2).
                if let Some(folder) = path.parent() {
                    imported.config = imported.config.with_absolute_inputs(&app.schema, folder);
                }
                apply_import(app, imported, &file_label(&path));
            }
            Err(error) => app.error_dialog(
                "The file could not be read",
                format!("{} could not be imported.", path.display()),
                Some(error),
            ),
        })
    });
}

/// Reads a text file of reasonable size (a config file is small).
fn read_text(path: &std::path::Path) -> Result<String, String> {
    const MAX: u64 = 4 * 1024 * 1024;
    let size = std::fs::metadata(path).map_err(|error| error.to_string())?.len();
    if size > MAX {
        return Err(format!("The file is {} bytes; a config file is much smaller.", size));
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    String::from_utf8(bytes).map_err(|_| "The file is not UTF-8 text.".to_string())
}

fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

pub fn start_paste_command_line(app: &mut App) {
    after_confirming_unsaved(app, "Importing replaces them.", Action::PasteCommandLine);
}

fn open_paste_dialog(app: &mut App) {
    app.open_dialog(
        Dialog::new("Paste a command line")
            .message("Paste a siteone-crawler command written for PowerShell or Bash; the app tells which shell's quoting it reads it with (PowerShell as 7.3 or later passes it). Its options replace the current configuration; passwords it does not contain are cleared (the AI API key stays).")
            .multiline_input("Command line", "siteone-crawler --url=https://example.com/ --workers=5")
            .refresh(refresh_paste_dialog),
    );
}

/// The status line and the buttons of the paste dialog for its text (GUI-SPEC §5.2): how the
/// command reads, and when PowerShell and Bash read it differently which reading is used — or,
/// without a sign of the shell, the question.
fn refresh_paste_dialog(dialog: &mut Dialog<Action>) {
    use config::CommandLineReading::*;
    let text = dialog.input_text();
    dialog.buttons.clear();
    let import = |dialog: &mut Dialog<Action>, enabled: bool| {
        dialog.push_button(
            "Import",
            ButtonKind::Primary,
            Some(Action::ImportCommandLine(None)),
            enabled,
        );
    };
    if text.is_empty() {
        dialog.status = None;
        import(dialog, false);
        dialog.push_button("Cancel", ButtonKind::Secondary, None, true);
        return;
    }
    dialog.status = Some(match config::read_command_line(&text) {
        Same(_) => {
            import(dialog, true);
            (Tone::Success, "PowerShell and Bash read this the same way.".to_string())
        }
        Only { shell, other_error, .. } => {
            import(dialog, true);
            (
                Tone::Info,
                format!(
                    "Read as {}. {} cannot read it: {other_error}",
                    shell.label(),
                    shell.other().label()
                ),
            )
        }
        Detected {
            shell,
            argv,
            hint,
            other,
        } => {
            let instead = shell.other();
            dialog.push_button(
                format!("Import as {} instead", instead.label()),
                ButtonKind::Secondary,
                Some(Action::ImportCommandLine(Some(instead))),
                true,
            );
            import(dialog, true);
            let difference = match first_difference(&argv, &other) {
                (_, Some(theirs)) => format!("would read {} instead", shown(theirs)),
                (Some(mine), None) => format!("would leave out {}", shown(mine)),
                (None, None) => "would read it differently".to_string(),
            };
            (
                Tone::Info,
                format!("Read as {} ({hint}). {} {difference}.", shell.label(), instead.label()),
            )
        }
        Ambiguous {
            powershell,
            bash,
            hints,
        } => {
            for shell in [Shell::PowerShell, Shell::Posix] {
                dialog.push_button(
                    format!("Import as {}", shell.label()),
                    ButtonKind::Secondary,
                    Some(Action::ImportCommandLine(Some(shell))),
                    true,
                );
            }
            let reads = |argv: Option<&str>| argv.map(shown).unwrap_or_else(|| "nothing".to_string());
            let (ps, sh) = first_difference(&powershell, &bash);
            let why = if hints.is_empty() {
                "nothing in it shows which shell it is written for".to_string()
            } else {
                let signs: Vec<String> = hints
                    .iter()
                    .map(|hint| format!("{} ({})", hint.what, hint.shell.label()))
                    .collect();
                format!("it has signs of both: {}", signs.join(", "))
            };
            (
                Tone::Warning,
                format!(
                    "PowerShell and Bash read this differently (PowerShell: {}; Bash: {}), and {why}. Which shell is it for?",
                    reads(ps),
                    reads(sh)
                ),
            )
        }
        Invalid {
            shell,
            error,
            other_error,
        } => {
            import(dialog, false);
            let message = if error == other_error {
                format!("Neither PowerShell nor Bash can read this. {error}")
            } else {
                format!(
                    "As {}: {error}\nAs {}: {other_error}",
                    shell.label(),
                    shell.other().label()
                )
            };
            (Tone::Danger, message)
        }
    });
    dialog.push_button("Cancel", ButtonKind::Secondary, None, true);
}

/// The first argument where two readings differ, from each (`None` past its end).
fn first_difference<'a>(a: &'a [String], b: &'a [String]) -> (Option<&'a str>, Option<&'a str>) {
    let at = a.iter().zip(b).take_while(|(a, b)| a == b).count();
    (a.get(at).map(String::as_str), b.get(at).map(String::as_str))
}

/// An argument in quotes for the status line: line breaks and tabs visible, long ones shortened.
fn shown(arg: &str) -> String {
    const MAX: usize = 70;
    let visible = arg.replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
    let short: String = visible.chars().take(MAX).collect();
    let ellipsis = if visible.chars().count() > MAX { "…" } else { "" };
    format!("“{short}{ellipsis}”")
}

fn import_command_line(app: &mut App, mut dialog: Dialog<Action>, shell: Option<Shell>) {
    use config::CommandLineReading::*;
    let text = dialog.input_text();
    if text.is_empty() {
        dialog.error = Some("Paste a command line first.".to_string());
        app.open_dialog(dialog);
        return;
    }
    let (read, shell) = match shell {
        Some(shell) => (config::split_command_line(&text, shell), Some(shell)),
        None => match config::read_command_line(&text) {
            Same(argv) => (Ok(argv), None),
            Only { shell, argv, .. } | Detected { shell, argv, .. } => (Ok(argv), Some(shell)),
            Ambiguous { .. } => (
                Err("PowerShell and Bash read this differently: choose the shell it is written for.".to_string()),
                None,
            ),
            Invalid { error, .. } => (Err(error), None),
        },
    };
    match read {
        Ok(args) => {
            let imported = config::import_args(&app.schema, &args);
            let source = match shell {
                Some(shell) => format!("the command line (read as {})", shell.label()),
                None => "the command line".to_string(),
            };
            apply_import(app, imported, &source);
        }
        Err(error) => {
            dialog.error = Some(error);
            app.open_dialog(dialog);
        }
    }
}

/// Replaces the configuration with an import and reports what was not taken over.
pub fn apply_import(app: &mut App, imported: Imported, source: &str) {
    let Imported {
        mut config,
        secrets,
        unknown,
        ignored,
        invalid,
    } = imported;
    if config.url.trim().is_empty() {
        config.url = app.session.config.url.clone();
    }
    let count = config.values.len() + secrets.options().count();
    app.session.config = config;
    // The passwords are part of the configuration it replaces: one the import does not name is
    // cleared (the API key aside). Saved profiles keep theirs.
    app.secrets = secrets.replacing(&app.secrets);
    app.setup.editor.clear();
    app.setup.start_error = None;

    let mut details = Vec::new();
    details.extend(unknown.iter().map(|item| format!("unknown: {item}")));
    details.extend(invalid.iter().map(|item| format!("invalid: {item}")));
    details.extend(ignored.iter().map(|item| format!("set by the GUI: {item}")));
    if details.is_empty() {
        app.toasts.success(format!(
            "Imported {} from {source}.",
            crate::ui::widgets::format::plural(count as u64, "option")
        ));
    } else {
        app.open_dialog(
            Dialog::new("Imported with remarks")
                .tone(Tone::Warning)
                .message(format!(
                    "Imported {} from {source}. These were not taken over:",
                    crate::ui::widgets::format::plural(count as u64, "option")
                ))
                .details(details)
                .cancel("OK"),
        );
    }
}

pub fn open_export(app: &mut App) {
    let has_secrets = !app.secrets.is_empty();
    let mut dialog = Dialog::new("Export a config file")
        .message("The file holds one CLI argument per line and works with siteone-crawler --config-file=<file>.");
    if has_secrets {
        dialog = dialog.check("Include passwords and API keys (written in plain text)", false);
    }
    app.open_dialog(
        dialog
            .button("Export…", ButtonKind::Primary, Action::ExportConfigFile)
            .cancel("Cancel"),
    );
}

fn export_config_file(app: &mut App, include_secrets: bool) {
    let name = format!(
        "{}.conf",
        app.session
            .profile
            .clone()
            .unwrap_or_else(|| "siteone-crawler".to_string())
    );
    let request = FileRequest::new("Export a config file")
        .filter("Config files", &["conf"])
        .file_name(name);
    let Some(path) = app.dialogs.save_file(&request) else {
        return;
    };
    let text = export_text(app, include_secrets);
    write_export(app, path, text);
}

/// The config-file text of the current configuration; secrets only when included.
pub fn export_text(app: &App, include_secrets: bool) -> config::ConfigFileExport {
    let argv = app
        .run_config()
        .to_preview_argv(&app.schema, &app.secrets, include_secrets);
    let argv: Vec<String> = if include_secrets {
        argv
    } else {
        argv.into_iter()
            .filter(|arg| {
                let name = arg.split('=').next().unwrap_or(arg);
                !app.schema
                    .get(name.trim_end_matches(':'))
                    .is_some_and(|spec| spec.secret)
            })
            .collect()
    };
    config::config_file_text(&argv)
}

fn write_export(app: &mut App, path: PathBuf, export: config::ConfigFileExport) {
    app.spawn_task(move || {
        let result = crate::model::storage::write_atomic(&path, export.text.as_bytes());
        Box::new(move |app: &mut App| match result {
            Ok(()) if export.inexact.is_empty() => {
                app.toasts.success(format!("Exported to {}.", path.display()));
            }
            Ok(()) => app.toasts.warning(format!(
                "Exported to {}. A config file cannot hold these values exactly, so they were left out: {}.",
                path.display(),
                export.inexact.join(", ")
            )),
            Err(error) => app.error_dialog(
                "The file could not be written",
                format!("{} could not be written.", path.display()),
                Some(error.to_string()),
            ),
        })
    });
}

/// The command line of the current configuration for `shell` (secrets masked unless `reveal`).
pub fn command_text(app: &App, shell: Shell, reveal: bool) -> String {
    let argv = app.run_config().to_preview_argv(&app.schema, &app.secrets, reveal);
    let program = app
        .crawler
        .path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| config::PROGRAM_NAME.to_string());
    config::command_line(&program, &argv, shell)
}
