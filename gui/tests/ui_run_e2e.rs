//! End to end with the real crawler against a locally served site (no network): Start → live →
//! Finished → scores on the Run page → the run recorded in History, with a broken link and a
//! broken stylesheet background image found and joined to the pages that reference them.

mod core_support;
mod ui_common;

use std::time::{Duration, Instant};

use core_support::{crawler_binary, serve, write_site};
use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use siteone_crawler_gui::app::{Page, ResultState};
use siteone_crawler_gui::model::config::Value;
use siteone_crawler_gui::model::history::{RecordedOutcome, RunRecord};
use siteone_crawler_gui::model::results::ResultLoad;
use siteone_crawler_gui::model::run_state::RunOutcome;
use siteone_crawler_gui::ui::run::broken::Where;
use siteone_crawler_gui::ui::run::broken_rows;
use ui_common::{Env, LARGE, harness, settle, shows_text};

#[test]
fn a_real_crawl_runs_to_the_end_with_scores_broken_links_and_a_history_record() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let site = tmp.path().join("site");
    write_site(&site, 4);
    // A missing page and a stylesheet with a missing background image.
    let index = site.join("index.html");
    let html = std::fs::read_to_string(&index).expect("index");
    std::fs::write(
        &index,
        html.replace(
            "</ul>",
            "<li><a href=\"/missing-page.html\">Gone</a></li></ul><div class=\"hero\">Hero</div>",
        ),
    )
    .expect("index");
    std::fs::write(
        site.join("style.css"),
        "body { font-family: sans-serif; }\n.hero { background: url('/img/missing-background.jpg'); }\n",
    )
    .expect("css");
    let server = serve(&site, &tmp.path().join("server"));

    let env = Env::new();
    let mut app = env.app_with(Some(crawler_binary().to_path_buf()), Vec::new());
    app.session.config.url = server.url();
    let schema = app.schema.clone();
    app.session
        .config
        .set(&schema, "--max-reqs-per-sec", Value::Text("100".to_string()))
        .expect("set");
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.get_by_role_and_label(Role::Button, "Start crawl").click();
    harness.run_steps(2);
    assert_eq!(
        harness.state().page,
        Page::Run,
        "{:?} / {:?}",
        harness.state().setup.start_error,
        harness.state().dialog.as_ref().map(|dialog| &dialog.title)
    );
    let run_dir = harness
        .state()
        .current_run
        .as_ref()
        .expect("a run")
        .plan
        .run_dir
        .clone();
    assert!(
        RunRecord::read(&run_dir).is_ok(),
        "the initial record is written at start"
    );

    let deadline = Instant::now() + Duration::from_secs(120);
    while harness.state().has_active_run() {
        assert!(Instant::now() < deadline, "the crawl did not end");
        harness.step();
        std::thread::sleep(Duration::from_millis(30));
    }
    settle(&mut harness);
    let run = harness.state().current_run.as_ref().expect("the run");
    assert_eq!(
        run.state.outcome(),
        RunOutcome::Finished,
        "{}",
        run.state.console.plain_text()
    );
    let ResultState::Ready(ResultLoad::Loaded(data)) = &run.result else {
        panic!("the result JSON did not load: {:?}", run.result);
    };
    let overall = data.scores.as_ref().expect("scores").overall.score;
    assert!(shows_text(&harness, "Overall score"));

    // The broken page and the broken background image, with where they were found.
    harness.get_by_role_and_label(Role::Tab, "Link checker").click();
    settle(&mut harness);
    let shown = broken_rows(harness.state());
    let page = shown
        .iter()
        .find(|row| row.url.ends_with("/missing-page.html"))
        .unwrap_or_else(|| panic!("no missing page in {shown:?}"));
    assert_eq!((page.code, page.place), (404, Where::InternalPage));
    assert!(page.found_on().is_some_and(|source| source.ends_with('/')), "{page:?}");
    let image = shown
        .iter()
        .find(|row| row.url.ends_with("/img/missing-background.jpg"))
        .unwrap_or_else(|| panic!("no missing image in {shown:?}"));
    assert_eq!((image.code, image.place), (404, Where::InternalAsset));
    assert!(
        image.found_on().is_some_and(|source| source.ends_with("style.css")),
        "{image:?}"
    );

    // Recorded in History, with the score.
    let record = RunRecord::read(&run_dir).expect("run.json");
    assert_eq!(record.outcome, Some(RecordedOutcome::Finished));
    assert_eq!(record.exit_code, Some(0));
    assert_eq!(record.summary.overall_score, Some(overall));
    assert!(record.summary.urls >= 6, "{record:?}");
    harness.get_by_role_and_label(Role::Button, "History").click();
    settle(&mut harness);
    assert_eq!(harness.state().history_ui.rows.len(), 1);
    assert!(shows_text(&harness, &server.url()));
}
