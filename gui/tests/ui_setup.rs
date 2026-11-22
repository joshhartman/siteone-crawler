//! Interaction tests of the Setup page (GUI-SPEC §8): typing a URL enables Start; a preset changes
//! the form and the command preview; searching filters the option list; changing and resetting an
//! option adds and removes it in the preview; an invalid configuration shows the CLI's error on
//! Start; profiles save and load; a missing crawler and a failing start are reported.

mod ui_common;

use eframe::egui::accesskit::{Role, Toggled};
use eframe::egui::{self, Key, Modifiers};
use egui_kittest::kittest::{NodeT, Queryable};
use siteone_crawler_gui::app::{App, Page};
use siteone_crawler_gui::model::config::{ACTION_LINK_PATTERNS, Value};
use ui_common::{Env, LARGE, harness, settle, shows_text};

fn preview(app: &App) -> Vec<String> {
    app.session.config.to_preview_argv(&app.schema, &app.secrets, false)
}

/// Replaces the text of the single-line field named `label`.
fn type_into(harness: &mut egui_kittest::Harness<'_, App>, label: &str, text: &str) {
    harness.get_by_role_and_label(Role::TextInput, label).focus();
    harness.run();
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.get_by_role_and_label(Role::TextInput, label).type_text(text);
    harness.run();
}

/// Whether the toggle-like widget (chip, card, switch) named `label` is on.
fn is_on(harness: &egui_kittest::Harness<'_, App>, role: Role, label: &str) -> bool {
    harness.get_by_role_and_label(role, label).accesskit_node().toggled() == Some(Toggled::True)
}

/// Clicks the button named `label` (static text with the same words is not a button).
fn click(harness: &mut egui_kittest::Harness<'_, App>, label: &str) {
    harness.get_by_role_and_label(Role::Button, label).click();
}

#[test]
fn typing_a_url_enables_start() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Start crawl")
            .accesskit_node()
            .is_disabled()
    );

    type_into(&mut harness, "Website URL", "example.com");
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Start crawl")
            .accesskit_node()
            .is_disabled()
    );
    assert_eq!(harness.state().session.config.url, "example.com");
}

#[test]
fn leaving_the_url_field_completes_the_scheme_and_checks_the_shape() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    type_into(&mut harness, "Website URL", "example.com/shop");
    harness.key_press(Key::Tab);
    harness.run();
    assert_eq!(harness.state().session.config.url, "https://example.com/shop");

    type_into(&mut harness, "Website URL", "ftp://example.com");
    harness.key_press(Key::Tab);
    harness.run();
    assert!(shows_text(&harness, "must start with http"));
}

#[test]
fn applying_a_preset_changes_the_form_and_the_command_preview() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    assert!(
        !preview(harness.state())
            .iter()
            .any(|arg| arg.starts_with("--disable-images"))
    );

    click(&mut harness, "SEO Analysis");
    settle(&mut harness);
    let state = harness.state();
    assert_eq!(
        state.session.config.values.get("--disable-images"),
        Some(&Value::Bool(true))
    );
    assert!(preview(state).contains(&"--disable-images=true".to_string()));
    // The preview bar shows it, and the card is the selected one.
    assert!(shows_text(&harness, "--disable-images=true"));
    assert!(is_on(&harness, Role::Button, "SEO Analysis"));
    // The form shows it: the "Images" download chip is off (the Content section is open in the
    // two-column layout of a large window).
    harness
        .get_by_role_and_label(Role::DisclosureTriangle, "Content")
        .scroll_to_me();
    settle(&mut harness);
    assert!(!is_on(&harness, Role::CheckBox, "Images"));
}

#[test]
fn a_preset_over_hand_made_changes_asks_first() {
    let env = Env::new();
    let mut app = env.app();
    app.session
        .config
        .set(&app.schema, "--workers", Value::Text("9".to_string()))
        .expect("set");
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    click(&mut harness, "Quick Audit");
    settle(&mut harness);
    assert!(harness.state().dialog.is_some(), "a confirmation is shown");
    assert_eq!(
        harness.state().session.config.values.get("--workers"),
        Some(&Value::Text("9".to_string()))
    );
    click(&mut harness, "Apply preset");
    settle(&mut harness);
    assert!(harness.state().dialog.is_none());
    assert_eq!(
        harness.state().session.config.values.get("--workers"),
        Some(&Value::Text("5".to_string())),
        "Quick Audit sets 5 workers"
    );
}

#[test]
fn searching_filters_the_option_list() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.get_by_role_and_label(Role::RadioButton, "All options").click();
    settle(&mut harness);
    assert!(
        harness.query_by_role_and_label(Role::TextInput, "--timeout").is_some(),
        "the first group lists --timeout"
    );

    type_into(&mut harness, "Search options", "robots");
    settle(&mut harness);
    assert!(
        harness
            .query_by_role_and_label(Role::CheckBox, "--ignore-robots-txt")
            .is_some()
    );
    assert!(
        harness.query_by_role_and_label(Role::TextInput, "--timeout").is_none(),
        "non-matching options are hidden"
    );
    assert!(shows_text(&harness, "Results for “robots”"));

    type_into(&mut harness, "Search options", "zzz-no-such-option");
    settle(&mut harness);
    assert!(shows_text(&harness, "No option matches"));
}

#[test]
fn changing_then_resetting_an_option_adds_and_removes_it_in_the_preview() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    type_into(&mut harness, "Search options", "robots");
    settle(&mut harness);

    harness
        .get_by_role_and_label(Role::CheckBox, "--ignore-robots-txt")
        .click();
    settle(&mut harness);
    assert!(preview(harness.state()).contains(&"--ignore-robots-txt=true".to_string()));
    assert!(shows_text(&harness, "--ignore-robots-txt=true"));
    assert!(shows_text(&harness, "1 changed"));

    click(&mut harness, "Reset --ignore-robots-txt");
    settle(&mut harness);
    assert!(!preview(harness.state()).iter().any(|arg| arg.contains("ignore-robots")));
    assert!(!shows_text(&harness, "--ignore-robots-txt=true"));
    assert!(shows_text(&harness, "No changes"));
}

#[test]
fn a_number_typed_into_an_option_reaches_the_preview_and_bad_input_is_flagged() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.get_by_role_and_label(Role::RadioButton, "All options").click();
    settle(&mut harness);
    type_into(&mut harness, "--timeout", "7");
    settle(&mut harness);
    assert!(preview(harness.state()).contains(&"--timeout=7".to_string()));

    type_into(&mut harness, "--timeout", "x");
    settle(&mut harness);
    assert!(shows_text(&harness, "Enter a whole number."));
}

#[test]
fn an_invalid_configuration_shows_the_cli_error_on_start() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    type_into(&mut harness, "Website URL", "https://example.com/");
    {
        let app = harness.state_mut();
        let schema = app.schema.clone();
        app.session
            .config
            .set(&schema, "--workers", Value::Text("abc".to_string()))
            .expect("set");
    }
    click(&mut harness, "Start crawl");
    settle(&mut harness);
    let error = harness.state().setup.start_error.clone().expect("a start error");
    assert!(error.contains("--workers"), "{error}");
    assert!(shows_text(&harness, "--workers"));
    assert_eq!(harness.state().page, Page::Setup);
    assert!(harness.state().current_run.is_none());
    // No run folder was created.
    let runs = std::fs::read_dir(env.runs_dir()).map(|dir| dir.count()).unwrap_or(0);
    assert_eq!(runs, 0);
}

#[test]
fn ctrl_enter_starts_and_a_spawn_failure_is_a_dialog_without_a_run_folder() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    type_into(&mut harness, "Website URL", "https://example.com/");
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    settle(&mut harness);
    let state = harness.state();
    assert!(state.setup.start_error.is_none(), "{:?}", state.setup.start_error);
    let dialog = state.dialog.as_ref().expect("a dialog");
    assert_eq!(dialog.title, "The crawl could not be started");
    assert!(state.current_run.is_none());
    assert!(shows_text(&harness, "The crawl could not be started"));
    let runs = std::fs::read_dir(env.runs_dir()).map(|dir| dir.count()).unwrap_or(0);
    assert_eq!(runs, 0, "the run folder of a failed start is removed");
    harness.key_press(Key::Escape);
    settle(&mut harness);
    assert!(harness.state().dialog.is_none());
}

#[test]
fn a_missing_crawler_shows_the_banner_and_disables_start() {
    let env = Env::new();
    let mut harness = harness(env.app_with(None, Vec::new()), LARGE, egui::Theme::Light);
    settle(&mut harness);
    type_into(&mut harness, "Website URL", "https://example.com/");
    assert!(shows_text(&harness, "was not found"));
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Start crawl")
            .accesskit_node()
            .is_disabled()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Locate the crawler")
            .is_some()
    );
}

#[test]
fn locating_the_crawler_stores_the_path_and_enables_start() {
    let env = Env::new();
    let exe = env.dir.path().join("my crawler ž").join("siteone-crawler.exe");
    let mut app = env.app_with(None, vec![Some(exe.clone())]);
    app.session.config.url = "https://example.com/".to_string();
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    click(&mut harness, "Locate the crawler");
    settle(&mut harness);
    assert_eq!(harness.state().settings.crawler_path.as_deref(), Some(exe.as_path()));
}

#[test]
fn a_profile_saves_and_loads_with_its_secrets() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    type_into(&mut harness, "Website URL", "https://example.com/");
    {
        let app = harness.state_mut();
        let schema = app.schema.clone();
        app.session
            .config
            .set(&schema, "--workers", Value::Text("6".to_string()))
            .expect("set");
        app.secrets.set("--http-auth", "user:secret");
    }
    click(&mut harness, "Save as");
    settle(&mut harness);
    type_into(&mut harness, "Profile name", "Prod site");
    click(&mut harness, "Save profile");
    settle(&mut harness);
    {
        let app = harness.state();
        assert_eq!(app.session.profile.as_deref(), Some("prod-site"));
        assert!(!app.is_modified());
        assert!(app.paths.profiles_dir().join("prod-site.json").is_file());
        let text = std::fs::read_to_string(app.paths.profiles_dir().join("prod-site.json")).expect("profile");
        assert!(!text.contains("user:secret"), "secrets never reach the profile file");
        assert_eq!(
            app.secret_store.get("prod-site/--http-auth").expect("store").as_deref(),
            Some("user:secret")
        );
    }

    // Change it, then load the profile again: it asks, then restores values and secrets.
    {
        let app = harness.state_mut();
        let schema = app.schema.clone();
        app.session
            .config
            .set(&schema, "--workers", Value::Text("2".to_string()))
            .expect("set");
        app.secrets.remove("--http-auth");
    }
    settle(&mut harness);
    assert!(shows_text(&harness, "Unsaved changes"));
    harness.state_mut().session.profile = None;
    harness.state_mut().baseline = None;
    settle(&mut harness);
    harness.get_by_label("Profile").click();
    settle(&mut harness);
    harness.get_by_label("Prod site").click();
    settle(&mut harness);
    assert!(harness.state().dialog.is_some(), "unsaved changes are confirmed first");
    click(&mut harness, "Discard and continue");
    settle(&mut harness);
    let app = harness.state();
    assert_eq!(app.session.profile.as_deref(), Some("prod-site"));
    assert_eq!(
        app.session.config.values.get("--workers"),
        Some(&Value::Text("6".to_string()))
    );
    assert_eq!(app.secrets.get("--http-auth"), Some("user:secret"));
    assert!(!app.is_modified());
}

#[test]
fn a_pasted_command_line_is_imported_and_unknown_options_are_listed() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    click(&mut harness, "More profile actions");
    settle(&mut harness);
    harness.get_by_label("Paste command line…").click();
    settle(&mut harness);
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Command line")
        .focus();
    harness.run();
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Command line")
        .type_text("siteone-crawler --url=https://example.cz/ --workers=4 --bogus=1");
    harness.run();
    click(&mut harness, "Import as PowerShell");
    settle(&mut harness);
    let app = harness.state();
    assert_eq!(app.session.config.url, "https://example.cz/");
    assert_eq!(
        app.session.config.values.get("--workers"),
        Some(&Value::Text("4".to_string()))
    );
    let dialog = app.dialog.as_ref().expect("a report of what was not imported");
    assert!(
        dialog.details.iter().any(|item| item.contains("--bogus")),
        "{:?}",
        dialog.details
    );
}

#[test]
fn check_external_links_is_a_setting_of_the_app_not_of_the_command() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    let before = preview(harness.state());
    assert!(!harness.state().is_modified());

    harness
        .get_by_role_and_label(Role::CheckBox, "Check external links")
        .scroll_to_me();
    settle(&mut harness);
    harness
        .get_by_role_and_label(Role::CheckBox, "Check external links")
        .click();
    settle(&mut harness);
    let state = harness.state();
    assert!(state.session.config.check_external_links);
    assert!(is_on(&harness, Role::CheckBox, "Check external links"));
    // Nothing reaches the command: the crawler never requests other sites.
    assert_eq!(preview(state), before);
    assert!(
        preview(state)
            .iter()
            .all(|arg| !arg.contains("allowed-domain") && !arg.contains("single-foreign-page"))
    );
    // The note beside the switch and the one beside the command; no warning any more.
    assert!(shows_text(&harness, "one request per link, after the crawl"));
    assert!(shows_text(&harness, "+ external links checked by the app"));
    assert!(!shows_text(&harness, "security software"));
    // Scope counts it as a change, and so does the "modified" state.
    assert!(shows_text(&harness, "1 changed"));
    assert!(state.is_modified());
    {
        let _gpu = ui_common::gpu_lock();
        ui_common::snapshot(&mut harness, "setup-external-links-1280x820-light");
    }

    harness
        .get_by_role_and_label(Role::CheckBox, "Check external links")
        .click();
    settle(&mut harness);
    assert!(!harness.state().session.config.check_external_links);
    assert!(!shows_text(&harness, "+ external links checked by the app"));
    assert!(!harness.state().is_modified());
}

#[test]
fn skipping_cart_and_action_links_is_on_by_default_and_says_so_in_the_command() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    let skip = |argv: &[String]| -> Vec<String> {
        argv.iter()
            .filter(|arg| arg.starts_with("--ignore-regex"))
            .cloned()
            .collect()
    };
    let patterns: Vec<String> = ACTION_LINK_PATTERNS
        .iter()
        .map(|pattern| format!("--ignore-regex:={pattern}"))
        .collect();
    // On by default: the command carries the patterns; nothing counts as changed.
    assert_eq!(skip(&preview(harness.state())), patterns);
    assert!(shows_text(&harness, "--ignore-regex:=/[?&]add-to-cart=/"));
    assert!(!harness.state().is_modified());
    assert!(shows_text(&harness, "No changes"));
    let switch = "Skip cart and action links";
    harness.get_by_role_and_label(Role::CheckBox, switch).scroll_to_me();
    settle(&mut harness);
    assert!(is_on(&harness, Role::CheckBox, switch));
    assert!(shows_text(&harness, "add to cart, log out, …"));

    // Off: the patterns leave the command; Scope and the modified state count it.
    harness.get_by_role_and_label(Role::CheckBox, switch).click();
    settle(&mut harness);
    assert!(!harness.state().session.config.skip_action_links);
    assert!(skip(&preview(harness.state())).is_empty());
    assert!(shows_text(&harness, "1 changed"));
    assert!(harness.state().is_modified());
    // The user's own pattern stays first when the switch is on again.
    {
        let app = harness.state_mut();
        let schema = app.schema.clone();
        app.session
            .config
            .set(&schema, "--ignore-regex", Value::List(vec!["/\\.pdf$/".to_string()]))
            .expect("set");
    }
    harness.get_by_role_and_label(Role::CheckBox, switch).click();
    settle(&mut harness);
    let mut expected = vec!["--ignore-regex:=/\\.pdf$/".to_string()];
    expected.extend(patterns.iter().cloned());
    assert_eq!(skip(&preview(harness.state())), expected);
    {
        let _gpu = ui_common::gpu_lock();
        ui_common::snapshot(&mut harness, "setup-action-links-1280x820-light");
    }

    // Kept in the session: off survives a restart.
    harness.get_by_role_and_label(Role::CheckBox, switch).click();
    settle(&mut harness);
    harness.state_mut().flush();
    drop(harness);
    let app = env.app();
    assert!(!app.session.config.skip_action_links);
}

#[test]
fn turning_ai_on_shows_the_panel_and_a_failing_model_list_is_reported() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    assert!(harness.query_by_role_and_label(Role::Button, "Load models").is_none());
    // The AI section is at the bottom of the Simple view.
    harness
        .get_by_role_and_label(Role::CheckBox, "Use AI analysis")
        .scroll_to_me();
    settle(&mut harness);
    harness.get_by_role_and_label(Role::CheckBox, "Use AI analysis").click();
    settle(&mut harness);
    assert!(harness.state().session.config.ai_enabled);
    harness
        .get_by_role_and_label(Role::Button, "Load models")
        .scroll_to_me();
    settle(&mut harness);
    click(&mut harness, "Load models");
    settle(&mut harness);
    let models = harness.state().setup.ai.models.clone().expect("an answer");
    assert!(models.is_err(), "the fake crawler cannot list models");
    assert!(!harness.state().setup.ai.loading_models);
    // The AI provider is emitted once AI is on.
    assert!(
        preview(harness.state())
            .iter()
            .any(|arg| arg.starts_with("--ai-provider="))
    );
}

#[test]
fn keyboard_shortcuts_switch_pages() {
    let env = Env::new();
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Num4);
    settle(&mut harness);
    assert_eq!(harness.state().page, Page::Settings);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Num2);
    settle(&mut harness);
    assert_eq!(harness.state().page, Page::Run);
    assert!(shows_text(&harness, "No crawl yet"));
    click(&mut harness, "Go to Setup");
    settle(&mut harness);
    assert_eq!(harness.state().page, Page::Setup);
}

#[test]
fn the_session_is_restored_after_a_restart() {
    let env = Env::new();
    {
        let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
        settle(&mut harness);
        type_into(&mut harness, "Website URL", "https://example.com/again");
        click(&mut harness, "Single Page");
        settle(&mut harness);
        harness.state_mut().flush();
    }
    let app = env.app();
    assert_eq!(app.session.config.url, "https://example.com/again");
    assert_eq!(app.session.config.values.get("--single-page"), Some(&Value::Bool(true)));
}

#[test]
fn a_corrupt_session_file_becomes_a_notice() {
    let env = Env::new();
    let paths = env.paths();
    paths.ensure().expect("dirs");
    std::fs::write(paths.session_file(), "{ broken").expect("written");
    let app = env.app();
    assert!(
        app.toasts
            .items()
            .iter()
            .any(|toast| toast.text.contains("could not be loaded")),
        "{:?}",
        app.toasts.items()
    );
    assert!(app.session.config.values.is_empty());
}
