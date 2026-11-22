//! Snapshots of the Setup and Settings pages at 1280×820 and 1000×640, light and dark
//! (GUI-SPEC §8). See `ui_common` for why they are written and checked rather than compared.

mod ui_common;

use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use siteone_crawler_gui::app::Page;
use siteone_crawler_gui::model::config::Value;
use ui_common::{Env, LARGE, SMALL, harness, settle, snapshot};

const SIZES: [(&str, egui::Vec2); 2] = [("1280x820", LARGE), ("1000x640", SMALL)];
const THEMES: [(&str, egui::Theme); 2] = [("light", egui::Theme::Light), ("dark", egui::Theme::Dark)];

/// Renders `prepare`d apps in every size and theme as `<name>-<size>-<theme>.png`.
fn each_variant(
    name: &str,
    prepare: impl Fn(&Env) -> siteone_crawler_gui::app::App,
    after: impl Fn(&mut egui_kittest::Harness<'_, siteone_crawler_gui::app::App>),
) {
    let _gpu = ui_common::gpu_lock();
    for (size_name, size) in SIZES {
        for (theme_name, theme) in THEMES {
            let env = Env::new();
            let mut harness = harness(prepare(&env), size, theme);
            settle(&mut harness);
            after(&mut harness);
            settle(&mut harness);
            snapshot(&mut harness, &format!("{name}-{size_name}-{theme_name}"));
        }
    }
}

fn configured(env: &Env) -> siteone_crawler_gui::app::App {
    let mut app = env.app();
    app.session.config.url = "https://crawler.siteone.io/".to_string();
    let schema = app.schema.clone();
    for (name, value) in [
        ("--workers", Value::Text("5".to_string())),
        ("--max-depth", Value::Text("3".to_string())),
        ("--include-regex", Value::List(vec!["/docs/".to_string()])),
        ("--disable-images", Value::Bool(true)),
    ] {
        app.session.config.set(&schema, name, value).expect("set");
    }
    app
}

#[test]
fn setup_simple_view() {
    each_variant("setup-simple", configured, |_| {});
}

#[test]
fn setup_empty_first_run() {
    each_variant("setup-first-run", |env| env.app(), |_| {});
}

#[test]
fn setup_without_crawler() {
    each_variant("setup-no-crawler", |env| env.app_with(None, Vec::new()), |_| {});
}

#[test]
fn setup_all_options_view() {
    each_variant("setup-all-options", configured, |harness| {
        harness.get_by_role_and_label(Role::RadioButton, "All options").click();
    });
}

#[test]
fn setup_search() {
    each_variant(
        "setup-search",
        |env| {
            let mut app = configured(env);
            app.setup.search = "timeout".to_string();
            app
        },
        |_| {},
    );
}

#[test]
fn setup_ai_panel() {
    each_variant(
        "setup-ai",
        |env| {
            let mut app = configured(env);
            app.session.config.ai_enabled = true;
            let schema = app.schema.clone();
            app.session
                .config
                .set(
                    &schema,
                    "--ai-endpoint",
                    Value::Text("http://localhost:8000/v1".to_string()),
                )
                .expect("set");
            app.session
                .config
                .set(&schema, "--ai-model", Value::Text("qwen3-32b".to_string()))
                .expect("set");
            app.secrets.set("--ai-api-key", "sk-test-key");
            app
        },
        |harness| {
            harness
                .get_by_role_and_label(Role::Button, "Load models")
                .scroll_to_me();
        },
    );
}

#[test]
fn setup_command_preview_open_with_a_start_error() {
    each_variant(
        "setup-preview-error",
        |env| {
            let mut app = configured(env);
            app.setup.preview_open = true;
            app.setup.start_error = Some("Invalid value 'abc' for option --workers. Expected an integer.".to_string());
            app
        },
        |_| {},
    );
}

#[test]
fn settings_page() {
    each_variant(
        "settings",
        |env| {
            let mut app = env.app();
            app.page = Page::Settings;
            app
        },
        |_| {},
    );
}

#[test]
fn a_confirmation_dialog() {
    each_variant("dialog-reset", configured, |harness| {
        harness.get_by_role_and_label(Role::Button, "Reset all").click();
    });
}

/// An app without changes (the paste dialog opens without asking first).
fn unmodified(env: &Env) -> siteone_crawler_gui::app::App {
    let mut app = env.app();
    app.session.config.url = "https://example.com/".to_string();
    app
}

/// The paste dialog with a command typed into it.
fn paste_dialog(text: &'static str) -> impl Fn(&mut egui_kittest::Harness<'_, siteone_crawler_gui::app::App>) {
    move |harness| {
        harness
            .get_by_role_and_label(Role::Button, "More profile actions")
            .click();
        harness.run();
        harness.get_by_label("Paste command line…").click();
        harness.run();
        harness
            .get_by_role_and_label(Role::MultilineTextInput, "Command line")
            .focus();
        harness.run();
        harness
            .get_by_role_and_label(Role::MultilineTextInput, "Command line")
            .type_text(text);
    }
}

#[test]
fn the_paste_dialog_with_a_detected_shell() {
    each_variant(
        "dialog-paste-detected",
        unmodified,
        paste_dialog(
            "siteone-crawler --url=https://example.com/ --offline-export-dir=C:\\Exports\\site '--user-agent=Sam''s bot'",
        ),
    );
}

#[test]
fn the_paste_dialog_asking_for_the_shell() {
    each_variant(
        "dialog-paste-ambiguous",
        unmodified,
        paste_dialog("siteone-crawler --url=https://example.com/ --user-agent=a\\ b --workers=3"),
    );
}

#[test]
fn setup_all_sections_open() {
    each_variant(
        "setup-sections",
        |env| {
            let mut app = configured(env);
            let schema = app.schema.clone();
            app.session
                .config
                .set(&schema, "--offline-export-dir", Value::Text("offline".to_string()))
                .expect("set");
            app.secrets.set("--http-auth", "user:secret");
            app
        },
        |harness| {
            for section in ["Content", "Access", "Browser rendering"] {
                harness
                    .get_by_role_and_label(Role::DisclosureTriangle, section)
                    .scroll_to_me();
                harness.run();
                harness.get_by_role_and_label(Role::DisclosureTriangle, section).click();
                harness.run();
            }
            harness
                .get_by_role_and_label(Role::DisclosureTriangle, "Exports")
                .scroll_to_me();
        },
    );
}
