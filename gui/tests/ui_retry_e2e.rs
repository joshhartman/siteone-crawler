//! End to end with the real crawler: a small site on `127.0.0.1` (the crawler's `--serve-offline`)
//! links to a page that is missing; after the crawl the page is created, the row is retried from
//! the Link checker tab, it answers 200 and leaves the broken views and `link-report.html`.

mod core_support;
mod ui_common;

use std::time::{Duration, Instant};

use core_support::{crawler_binary, serve, write_site};
use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use siteone_crawler_gui::app::{App, Page};
use siteone_crawler_gui::model::config::Value;
use siteone_crawler_gui::model::run_state::RunOutcome;
use siteone_crawler_gui::ui::run::broken_rows;
use ui_common::{Env, LARGE, harness, settle};

/// Runs frames until the crawl, the link texts and a retry pass have all ended.
fn wait_until_idle(harness: &mut Harness<'_, App>) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let app = harness.state();
        let busy = app.has_active_run()
            || app.link_check.is_some()
            || app.text_job.is_some()
            || app.retry.is_some()
            || app.pending_tasks() > 0;
        if !busy {
            settle(harness);
            let app = harness.state();
            if !(app.has_active_run() || app.text_job.is_some() || app.retry.is_some()) {
                return;
            }
        }
        assert!(Instant::now() < deadline, "the app did not settle");
        harness.step();
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn a_missing_page_created_after_the_crawl_is_retried_from_the_link_checker() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let site = tmp.path().join("site");
    write_site(&site, 2);
    let index = site.join("index.html");
    let html = std::fs::read_to_string(&index).expect("index");
    std::fs::write(
        &index,
        html.replace("</ul>", "<li><a href=\"/missing-page.html\">Gone page</a></li></ul>"),
    )
    .expect("index");
    let server = serve(&site, &tmp.path().join("server"));
    let missing = format!("{}missing-page.html", server.url());

    let env = Env::new();
    let mut app = env.app_with(Some(crawler_binary().to_path_buf()), Vec::new());
    app.fetch_link_texts = true;
    app.session.config.url = server.url();
    let schema = app.schema.clone();
    app.session
        .config
        .set(&schema, "--max-reqs-per-sec", Value::Text("100".to_string()))
        .expect("set");
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.state_mut().page = Page::Setup;
    settle(&mut harness);
    harness.get_by_role_and_label(Role::Button, "Start crawl").click();
    harness.run_steps(2);
    let run_dir = harness
        .state()
        .current_run
        .as_ref()
        .expect("a run")
        .plan
        .run_dir
        .clone();
    wait_until_idle(&mut harness);
    let run = harness.state().current_run.as_ref().expect("the run");
    assert_eq!(
        run.state.outcome(),
        RunOutcome::Finished,
        "{}",
        run.state.console.plain_text()
    );

    harness.get_by_role_and_label(Role::Tab, "Link checker").click();
    settle(&mut harness);
    let rows = broken_rows(harness.state());
    let gone = rows.iter().find(|row| row.url == missing).expect("the missing page");
    assert_eq!(gone.code, 404);
    let report = std::fs::read_to_string(run_dir.join("link-report.html")).expect("link report");
    assert!(report.contains(&missing), "{report}");

    // The page exists now; retry its row.
    std::fs::write(
        site.join("missing-page.html"),
        "<!DOCTYPE html><html><head><title>Found again</title></head><body><p>Back.</p></body></html>",
    )
    .expect("page");
    harness
        .get_by_role_and_label(Role::CheckBox, &format!("Select {missing}"))
        .click();
    settle(&mut harness);
    harness.get_by_role_and_label(Role::Button, "Retry now").click();
    harness.run_steps(2);
    assert!(harness.state().retry.is_some(), "the pass started");
    wait_until_idle(&mut harness);

    let app = harness.state();
    let run = app.current_run.as_ref().expect("the run");
    let row = run.state.urls.iter().find(|row| *row.url == missing).expect("row");
    assert_eq!(row.status, 200);
    let entry = run.retries.entry(&missing).expect("an entry");
    assert_eq!(
        entry.newest.as_ref().and_then(|answer| answer.title()),
        Some("Found again")
    );
    assert!(
        broken_rows(app).iter().all(|row| row.url != missing),
        "it left All broken"
    );
    // The report, written after the pass, lists only what is still broken.
    let report = std::fs::read_to_string(run_dir.join("link-report.html")).expect("link report");
    assert!(!report.contains(&missing), "{report}");
    assert!(run_dir.join("retries.json").is_file());
    assert!(!run_dir.join(".retry").exists(), "the child's files are removed");
}
