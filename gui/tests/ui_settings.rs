//! Interaction tests of the Settings page (GUI-SPEC §6.5): theme, runs folder, cache clearing,
//! crawler status.

mod ui_common;

use eframe::egui::accesskit::Role;
use eframe::egui::{self, Key, Modifiers};
use egui_kittest::kittest::Queryable;
use siteone_crawler_gui::app::Page;
use siteone_crawler_gui::model::settings::Theme;
use ui_common::{Env, LARGE, harness, settle, shows_text};

#[test]
fn the_theme_switch_changes_the_setting() {
    let env = Env::new();
    let mut app = env.app();
    app.page = Page::Settings;
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.get_by_role_and_label(Role::RadioButton, "Dark").click();
    settle(&mut harness);
    assert_eq!(harness.state().settings.theme, Theme::Dark);
    harness.state_mut().flush();
    let saved = siteone_crawler_gui::model::settings::Settings::load(&env.paths());
    assert_eq!(saved.value.theme, Theme::Dark, "the setting is saved");
}

#[test]
fn the_runs_folder_can_be_changed_and_reset() {
    let env = Env::new();
    let chosen = env.dir.path().join("my runs ž");
    let mut app = env.app_with(Some(env.fake_crawler()), vec![Some(chosen.clone())]);
    app.page = Page::Settings;
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.get_by_role_and_label(Role::Button, "Change…").click();
    settle(&mut harness);
    assert_eq!(harness.state().settings.runs_dir.as_deref(), Some(chosen.as_path()));
    assert!(shows_text(&harness, "my runs ž"));
    harness
        .get_by_role_and_label(Role::Button, "Use the default folder")
        .click();
    settle(&mut harness);
    assert_eq!(harness.state().settings.runs_dir, None);
}

#[test]
fn clearing_the_caches_asks_first_and_empties_them() {
    let env = Env::new();
    let mut app = env.app();
    app.page = Page::Settings;
    let cached = app.paths.http_cache_dir().join("entry.bin");
    std::fs::write(&cached, vec![0u8; 4096]).expect("cache entry");
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    assert!(shows_text(&harness, "Caches: 4.00 KB"), "the size is measured");
    harness.get_by_role_and_label(Role::Button, "Clear caches").click();
    settle(&mut harness);
    assert!(harness.state().dialog.is_some());
    assert!(cached.exists(), "nothing is deleted before the confirmation");
    // The dialog's button has the same name as the page's; the dialog's is the last one.
    let confirm = harness
        .get_all_by_role_and_label(Role::Button, "Clear caches")
        .last()
        .expect("the dialog button");
    confirm.click();
    settle(&mut harness);
    assert!(!cached.exists());
    assert!(
        harness.state().paths.http_cache_dir().is_dir(),
        "the folder itself stays"
    );
    assert!(shows_text(&harness, "Caches: 0 B"));
}

#[test]
fn the_crawler_card_shows_a_missing_crawler_and_ctrl_f_focuses_the_option_search() {
    let env = Env::new();
    let mut app = env.app_with(None, Vec::new());
    app.page = Page::Settings;
    let mut harness = harness(app, LARGE, egui::Theme::Dark);
    settle(&mut harness);
    assert!(shows_text(&harness, "Not found"));
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Num1);
    settle(&mut harness);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::F);
    settle(&mut harness);
    assert!(
        harness
            .get_by_role_and_label(Role::TextInput, "Search options")
            .is_focused()
    );
}
