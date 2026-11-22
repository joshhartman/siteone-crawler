//! The start flow end to end with the real crawler binary: Start validates, creates the run folder,
//! spawns the crawler, switches to the Run page and pumps the run's messages into its state until
//! the outcome. Skipped (with a message) when `target/<profile>/siteone-crawler.exe` is not built.

mod ui_common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::accesskit::Role;
use eframe::egui::{self};
use egui_kittest::kittest::{NodeT, Queryable};
use siteone_crawler_gui::app::Page;
use siteone_crawler_gui::model::config::Value;
use siteone_crawler_gui::model::run_state::RunOutcome;
use ui_common::{Env, LARGE, harness, settle, shows_text};

/// The crawler next to the test's target folder, if built.
fn built_crawler() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let profile_dir = exe.parent()?.parent()?;
    let binary = profile_dir.join(siteone_crawler_gui::model::paths::crawler_binary_name());
    binary.is_file().then_some(binary)
}

#[test]
fn start_runs_the_crawler_and_the_run_page_follows_it_to_the_outcome() {
    let Some(crawler) = built_crawler() else {
        eprintln!("skipped: the siteone-crawler binary is not built in this target folder");
        return;
    };
    let env = Env::new();
    let mut app = env.app_with(Some(crawler), Vec::new());
    // Port 1 refuses connections: the crawl ends quickly with "no pages crawled".
    app.session.config.url = "http://127.0.0.1:1/".to_string();
    let schema = app.schema.clone();
    for (name, value) in [
        ("--single-page", Value::Bool(true)),
        ("--timeout", Value::Text("2".to_string())),
    ] {
        app.session.config.set(&schema, name, value).expect("set");
    }
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.get_by_role_and_label(Role::Button, "Start crawl").click();
    harness.run_steps(2);

    let state = harness.state();
    assert_eq!(
        state.page,
        Page::Run,
        "{:?} / {:?}",
        state.setup.start_error,
        state.dialog.as_ref().map(|d| &d.title)
    );
    let run_dir = state.current_run.as_ref().expect("a current run").plan.run_dir.clone();
    assert!(run_dir.starts_with(env.runs_dir()), "{}", run_dir.display());
    assert!(run_dir.is_dir());

    let deadline = Instant::now() + Duration::from_secs(90);
    while harness.state().has_active_run() {
        assert!(Instant::now() < deadline, "the crawl did not end");
        harness.step();
        std::thread::sleep(Duration::from_millis(50));
    }
    harness.run_steps(2);
    let outcome = harness.state().current_run.as_ref().expect("the run").state.outcome();
    assert_eq!(outcome, RunOutcome::NoPagesCrawled, "{outcome:?}");
    assert!(shows_text(&harness, "No pages crawled"));
    assert!(!harness.state().has_active_run());
    // Start is available again.
    harness.get_by_role_and_label(Role::Button, "Setup").click();
    settle(&mut harness);
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Start crawl")
            .accesskit_node()
            .is_disabled()
    );
}
