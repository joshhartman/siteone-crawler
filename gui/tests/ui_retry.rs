//! The retry queue on the Run page (GUI-SPEC §4.9, §6.3) with scripted passes: rows are selected by
//! checkbox, header checkbox and Shift range; queued and retried; a row retried OK leaves the broken
//! views and the link report, and no view keeps track of it; the counts of the tab, the views, the
//! Overview and the History record follow; the notice about the crawler's outputs offers Run again;
//! a run reopened from History after a restart has its queue and results; the buttons are disabled
//! with a reason while a crawl, a check or another pass runs, and a crawl stops a pass; a run whose
//! configuration is incomplete retries its external links only.

mod ui_common;

use eframe::egui::accesskit::Role;
use eframe::egui::{self, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use siteone_crawler_gui::app::{App, Page, PastLoad};
use siteone_crawler_gui::backend::retry::RetryTarget;
use siteone_crawler_gui::model::config::Value;
use siteone_crawler_gui::model::history::RunRecord;
use siteone_crawler_gui::model::link_check::CheckReport;
use siteone_crawler_gui::model::retry::RetryState;
use siteone_crawler_gui::ui::run::broken::Scope;
use siteone_crawler_gui::ui::run::{Tab, broken_pages, broken_rows};
use ui_common::runs::{self, PassScript};
use ui_common::{Env, LARGE, harness, settle, shows_text};

fn click(harness: &mut Harness<'_, App>, role: Role, label: &str) {
    harness.get_by_role_and_label(role, label).click();
    settle(harness);
}

fn select(harness: &mut Harness<'_, App>, url: &str) {
    click(harness, Role::CheckBox, &format!("Select {url}"));
}

fn api(version: u32) -> String {
    format!("{}docs/api/v{version}/", runs::SITE)
}

fn selected(harness: &Harness<'_, App>) -> usize {
    harness.state().run_ui.broken.selection.len()
}

fn retries(app: &App) -> &RetryState {
    match &app.current_run {
        Some(run) if matches!(app.viewed_run, siteone_crawler_gui::app::ViewedRun::Current) => &run.retries,
        _ => match app.past_run.as_ref().map(|past| &past.load) {
            Some(PastLoad::Ready(data)) => &data.retries,
            _ => panic!("no run shown"),
        },
    }
}

fn status_of(app: &App, url: &str) -> i32 {
    let state = match app.past_run.as_ref().map(|past| &past.load) {
        Some(PastLoad::Ready(data)) => &data.state,
        _ => &app.current_run.as_ref().expect("run").state,
    };
    state.urls.iter().find(|row| &*row.url == url).expect("row").status
}

fn is_disabled(harness: &Harness<'_, App>, role: Role, label: &str) -> bool {
    harness
        .get_by_role_and_label(role, label)
        .accesskit_node()
        .is_disabled()
}

#[test]
fn rows_are_selected_queued_and_retried_and_the_views_and_counts_follow() {
    let env = Env::new();
    let mut app = env.app();
    let control = runs::finished(&mut app);
    let passes = runs::scripted_passes(&mut app, PassScript::Answer);
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    click(&mut harness, Role::Tab, "Link checker");
    assert_eq!(harness.state().run_ui.broken.broken_total(), 6);
    let errors_before = harness
        .state()
        .current_run
        .as_ref()
        .expect("run")
        .state
        .status_counts
        .errors();

    // A checkbox, then a Shift range from it.
    select(&mut harness, runs::FIXTURE_404);
    assert_eq!(selected(&harness), 1);
    harness
        .get_by_role_and_label(Role::CheckBox, &format!("Select {}", api(33)))
        .click_modifiers(Modifiers::SHIFT);
    settle(&mut harness);
    assert_eq!(selected(&harness), 4, "the 404 and the three 429s");
    assert!(shows_text(&harness, "4 selected"));
    // The header checkbox selects every shown row, then none.
    click(&mut harness, Role::CheckBox, "Select all shown rows");
    assert_eq!(selected(&harness), 6);
    click(&mut harness, Role::CheckBox, "Select all shown rows");
    assert_eq!(selected(&harness), 0);
    // A view change clears the selection.
    select(&mut harness, &api(31));
    click(&mut harness, Role::RadioButton, "Internal pages");
    assert_eq!(selected(&harness), 0);
    click(&mut harness, Role::RadioButton, "All broken");

    // Add to queue.
    select(&mut harness, &api(31));
    select(&mut harness, &api(32));
    click(&mut harness, Role::Button, "Add to queue");
    assert_eq!(retries(harness.state()).queue, [api(31), api(32)]);
    assert!(shows_text(&harness, "Retry queue: 2 URLs · one request each."));
    click(&mut harness, Role::RadioButton, "Retry queue");
    let queued: Vec<String> = broken_rows(harness.state()).into_iter().map(|row| row.url).collect();
    assert_eq!(queued, [api(31), api(32)]);
    assert!(RetryState::path(&control.run_dir).is_file(), "the queue is saved");

    // Retry queue: both answer 200 now.
    click(&mut harness, Role::Button, "Retry queue");
    let pass = passes.lock().expect("passes").pop().expect("a pass");
    assert_eq!(pass.len(), 2);
    assert!(
        pass.iter()
            .all(|item| matches!(item.target, RetryTarget::Crawled { .. }))
    );
    let app = harness.state();
    assert!(app.retry.is_none(), "the pass ended");
    assert_eq!((status_of(app, &api(31)), status_of(app, &api(32))), (200, 200));
    assert!(retries(app).queue.is_empty());
    // The empty queue view is gone; the rows left the broken views.
    assert_eq!(app.run_ui.broken.filter.scope, Scope::AllBroken);
    assert_eq!(app.run_ui.broken.broken_total(), 4);
    assert!(
        broken_rows(app)
            .iter()
            .all(|row| row.url != api(31) && row.url != api(32))
    );
    // The pass says how it ended; no view or link keeps the retried URLs.
    assert!(shows_text(&harness, "Retried 2 URLs: 2 now OK."));
    assert!(harness.query_by_role_and_label(Role::RadioButton, "Retried").is_none());
    assert!(!shows_text(&harness, "Show retried"));

    // The Overview counts the newest answers and says the crawler's outputs are older.
    click(&mut harness, Role::Tab, "Overview");
    assert!(!shows_text(&harness, "retried after the crawl"));
    assert!(shows_text(&harness, "Retries changed answers after the crawl."));
    let errors = harness
        .state()
        .current_run
        .as_ref()
        .expect("run")
        .state
        .status_counts
        .errors();
    assert_eq!(errors, errors_before - 2);
    settle(&mut harness);
    assert_eq!(
        RunRecord::read(&control.run_dir).expect("run.json").summary.errors,
        errors
    );
    let report = std::fs::read_to_string(control.file("link-report.html")).expect("link report");
    // The report lists what is still broken: not the URLs that answer OK now.
    assert!(report.contains(&api(33)), "{report}");
    assert!(
        !report.contains(&api(31)) && !report.contains(&api(32)) && !report.contains("Retried"),
        "{report}"
    );
    // The URLs tab shows the retried answer and title.
    click(&mut harness, Role::Tab, "URLs");
    harness.state_mut().run_ui.urls.filter.text = "docs/api/v3".to_string();
    settle(&mut harness);
    assert!(shows_text(&harness, "Retried page"));
    harness.state_mut().run_ui.urls.filter.text.clear();
    click(&mut harness, Role::Tab, "Files");
    assert!(shows_text(&harness, "Retries changed answers after the crawl."));
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Open Link report")
            .is_some()
    );
    // Run again (the notice's link; the header has one too).
    let run_again = harness
        .query_all_by_role_and_label(Role::Button, "Run again")
        .last()
        .expect("Run again");
    run_again.click();
    settle(&mut harness);
    assert_eq!(harness.state().page, Page::Setup);

    // Queue one more, then restart: the run from History has its queue and its results.
    harness.state_mut().page = Page::Run;
    settle(&mut harness);
    click(&mut harness, Role::Tab, "Link checker");
    click(&mut harness, Role::RadioButton, "All broken");
    let hero = format!("{}images/hero-background.webp", runs::SITE);
    select(&mut harness, &hero);
    click(&mut harness, Role::Button, "Add to queue");
    settle(&mut harness);
    let run_dir = control.run_dir.clone();
    drop(harness);
    let mut harness = ui_common::harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.state_mut().open_past_run(run_dir);
    settle(&mut harness);
    harness.state_mut().run_ui.tab = Tab::Broken;
    settle(&mut harness);
    let app = harness.state();
    assert_eq!(retries(app).queue, [hero]);
    assert_eq!(
        status_of(app, &api(31)),
        200,
        "the newest answer is merged into the rows"
    );
    assert_eq!(app.run_ui.broken.broken_total(), 4);
    assert!(
        broken_rows(app)
            .iter()
            .all(|row| row.url != api(31) && row.url != api(32))
    );
}

#[test]
fn retry_buttons_are_disabled_with_a_reason_and_a_crawl_stops_a_pass() {
    let env = Env::new();
    let mut app = env.app();
    let control = runs::finished(&mut app);
    let _passes = runs::scripted_passes(&mut app, PassScript::Hold);
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    let run_dir = control.run_dir.clone();

    // While the run's external links are checked.
    runs::with_check(harness.state_mut(), 31, true);
    click(&mut harness, Role::Tab, "Link checker");
    select(&mut harness, runs::FIXTURE_404);
    assert!(is_disabled(&harness, Role::Button, "Retry now"));
    assert_eq!(
        harness.state().retry_blocker(&run_dir),
        Some("The external links of this run are being checked")
    );
    harness.state_mut().stop_link_check();
    settle(&mut harness);
    assert!(!is_disabled(&harness, Role::Button, "Retry now"));

    // While a pass runs: the bar shows it, a second one cannot start.
    click(&mut harness, Role::Button, "Retry now");
    assert!(harness.state().retry.is_some());
    assert!(shows_text(&harness, "Retrying 1 of 1"));
    assert!(harness.query_by_role_and_label(Role::Button, "Stop retrying").is_some());
    assert_eq!(harness.state().retry_blocker(&run_dir), Some("A retry is running"));

    // A crawl stops the pass; the URL in flight stays queued.
    let live = runs::attach(harness.state_mut(), runs::SITE);
    runs::run_started(&live, runs::SITE);
    settle(&mut harness);
    assert!(harness.state().retry.is_none(), "the crawl stopped the pass");
    let (stored, _) = RetryState::load(&run_dir);
    assert_eq!(stored.queue, [runs::FIXTURE_404.to_string()]);

    // While a crawl runs, the run from History cannot be retried.
    harness.state_mut().open_past_run(run_dir.clone());
    settle(&mut harness);
    harness.state_mut().run_ui.tab = Tab::Broken;
    settle(&mut harness);
    assert!(is_disabled(&harness, Role::Button, "Retry queue"));
    assert_eq!(
        harness.state().retry_blocker(&run_dir),
        Some("A crawl is running; retry when it has finished")
    );
}

#[test]
fn a_run_whose_configuration_is_incomplete_retries_only_its_external_links() {
    let env = Env::new();
    let mut app = env.app();
    let schema = app.schema.clone();
    app.session
        .config
        .set(
            &schema,
            "--header",
            Value::List(vec!["Cookie: session=abc".to_string()]),
        )
        .expect("header");
    let control = runs::finished(&mut app);
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    runs::with_check(harness.state_mut(), 88, false);
    let report: CheckReport = harness
        .state()
        .current_run
        .as_ref()
        .and_then(|run| run.links.report.clone())
        .expect("a check");
    report.write(&control.run_dir).expect("external-links.json");
    let broken_link = report
        .results
        .iter()
        .find(|link| link.status == Some(404))
        .expect("a broken link")
        .url
        .clone();
    drop(harness);

    // After a restart the headers are not stored: the run is incomplete for crawled URLs.
    let mut app = env.app();
    let passes = runs::scripted_passes(&mut app, PassScript::Answer);
    let mut harness = ui_common::harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.state_mut().open_past_run(control.run_dir.clone());
    settle(&mut harness);
    harness.state_mut().run_ui.tab = Tab::Broken;
    settle(&mut harness);
    let missing = harness.state().retry_missing(&control.run_dir).expect("incomplete");
    assert!(missing.contains("--header"), "{missing}");
    // A crawled row alone cannot be retried; with an external link the pass runs.
    select(&mut harness, runs::FIXTURE_404);
    assert!(is_disabled(&harness, Role::Button, "Retry now"));
    select(&mut harness, &broken_link);
    assert!(!is_disabled(&harness, Role::Button, "Retry now"));
    click(&mut harness, Role::Button, "Retry now");
    let pass = passes.lock().expect("passes").pop().expect("a pass");
    let refused = pass.iter().find(|item| item.url == runs::FIXTURE_404).expect("crawled");
    assert!(matches!(&refused.target, RetryTarget::Refused(reason) if reason.contains("--header")));
    assert!(
        pass.iter()
            .any(|item| item.url == broken_link && matches!(item.target, RetryTarget::External(_)))
    );
    let app = harness.state();
    let state = retries(app);
    assert_eq!(
        state.queue,
        [runs::FIXTURE_404.to_string()],
        "not retried: stays queued"
    );
    assert!(
        state
            .entry(runs::FIXTURE_404)
            .and_then(|entry| entry.error.as_deref())
            .is_some()
    );
    assert!(state.entry(&broken_link).is_some_and(|entry| entry.current().is_ok()));
    // The check's entry is replaced in external-links.json.
    settle(&mut harness);
    let stored = CheckReport::read(&control.run_dir).expect("readable").expect("stored");
    let entry = stored
        .results
        .iter()
        .find(|link| link.url == broken_link)
        .expect("entry");
    assert_eq!(entry.status, Some(200));
    assert!(shows_text(&harness, "1 could not be retried."));
}

#[test]
fn broken_links_are_listed_by_page_and_selected_and_retried_there() {
    let env = Env::new();
    let mut app = env.app();
    runs::finished(&mut app);
    let passes = runs::scripted_passes(&mut app, PassScript::Answer);
    // Tall enough to draw every row (the table draws only the rows in view).
    let mut harness = harness(app, egui::vec2(1280.0, 1400.0), egui::Theme::Light);
    settle(&mut harness);
    runs::with_check(harness.state_mut(), 12, false);
    // A broken external link found on two pages: the external link's and the fixture 404's.
    let shared = {
        let run = harness.state_mut().current_run.as_mut().expect("run");
        let report = run.links.report.as_mut().expect("a check");
        let link = report
            .results
            .iter_mut()
            .find(|link| link.status == Some(404))
            .expect("a broken link");
        link.sources.push(runs::FIXTURE_404_SOURCE.to_string());
        link.url.clone()
    };
    click(&mut harness, Role::Tab, "Link checker");
    assert!(broken_pages(harness.state()).is_empty(), "by link at first");
    let by_link = broken_rows(harness.state()).len();
    click(&mut harness, Role::CheckBox, "By page");
    assert!(harness.state().run_ui.broken.by_page);

    let groups = broken_pages(harness.state());
    let pages_of = |url: &str| -> Vec<Option<String>> {
        groups
            .iter()
            .filter(|(_, rows)| rows.iter().any(|row| row.url == url))
            .map(|(page, _)| page.clone())
            .collect()
    };
    // Each link under every page it was found on; links of an unknown page last.
    assert_eq!(pages_of(&shared).len(), 2, "{groups:#?}");
    assert_eq!(
        pages_of(runs::FIXTURE_404),
        [Some(runs::FIXTURE_404_SOURCE.to_string())]
    );
    let (last_page, last_rows) = groups.last().expect("groups");
    assert_eq!(*last_page, None);
    assert!(last_rows.iter().any(|row| row.code == 429), "{last_rows:#?}");
    // Pages with more broken links first.
    let sizes: Vec<usize> = groups
        .iter()
        .filter(|(page, _)| page.is_some())
        .map(|(_, rows)| rows.len())
        .collect();
    assert!(sizes.windows(2).all(|pair| pair[0] >= pair[1]), "{sizes:?}");
    let rows: usize = groups.iter().map(|(_, rows)| rows.len()).sum();
    assert_eq!(rows, by_link + 1, "one more row: the link found on two pages");
    let pages = groups.iter().filter(|(page, _)| page.is_some()).count();
    assert!(shows_text(
        &harness,
        &format!("{by_link} broken links on {pages} pages")
    ));
    assert!(shows_text(&harness, "Page not reported by the crawler"));
    assert!(shows_text(
        &harness,
        &format!("Page {}, 2 broken links", runs::FIXTURE_404_SOURCE)
    ));

    // A URL ticked under one page is ticked under the other too.
    let label = format!("Select {shared}");
    harness
        .query_all_by_role_and_label(Role::CheckBox, &label)
        .next()
        .expect("a checkbox")
        .click();
    settle(&mut harness);
    assert_eq!(selected(&harness), 1);
    let ticked: Vec<bool> = harness
        .query_all_by_role_and_label(Role::CheckBox, &label)
        .map(|node| node.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True))
        .collect();
    assert_eq!(ticked, [true, true]);
    // Shift extends over the rows as drawn, page headings skipped; the header checkbox selects
    // every link once.
    harness
        .get_by_role_and_label(Role::CheckBox, &format!("Select {}", runs::FIXTURE_404))
        .click_modifiers(Modifiers::SHIFT);
    settle(&mut harness);
    assert!(selected(&harness) >= 2, "{}", selected(&harness));
    click(&mut harness, Role::CheckBox, "Select all shown rows");
    click(&mut harness, Role::CheckBox, "Select all shown rows");
    assert_eq!(selected(&harness), 0);
    click(&mut harness, Role::CheckBox, "Select all shown rows");
    assert_eq!(selected(&harness), by_link);
    click(&mut harness, Role::CheckBox, "Select all shown rows");
    // Switching the layout keeps the selection; the filter narrows to one page.
    select(&mut harness, runs::FIXTURE_404);
    click(&mut harness, Role::CheckBox, "By page");
    assert_eq!(selected(&harness), 1);
    click(&mut harness, Role::CheckBox, "By page");
    assert_eq!(selected(&harness), 1);
    harness.state_mut().run_ui.broken.filter.text = "performance-metrics".to_string();
    settle(&mut harness);
    let groups = broken_pages(harness.state());
    assert_eq!(groups.len(), 1, "{groups:#?}");
    assert_eq!(groups[0].0.as_deref(), Some(runs::FIXTURE_404_SOURCE));
    assert_eq!(groups[0].1.len(), 2);
    // Retry now on a row selected by page.
    select(&mut harness, runs::FIXTURE_404);
    click(&mut harness, Role::Button, "Retry now");
    let pass = passes.lock().expect("passes").pop().expect("a pass");
    assert_eq!(
        pass.iter().map(|item| item.url.as_str()).collect::<Vec<_>>(),
        [runs::FIXTURE_404]
    );
    let groups = broken_pages(harness.state());
    assert!(
        groups
            .iter()
            .all(|(_, rows)| rows.iter().all(|row| row.url != runs::FIXTURE_404)),
        "answered OK: out of the broken views"
    );
    // The views that list every link or the queue are by link only.
    harness.state_mut().run_ui.broken.filter.text.clear();
    click(&mut harness, Role::RadioButton, "All external");
    assert!(broken_pages(harness.state()).is_empty());
    assert!(is_disabled(&harness, Role::CheckBox, "By page"));
}

#[test]
fn a_view_emptied_by_a_retry_says_what_it_covers() {
    let env = Env::new();
    let mut app = env.app();
    runs::finished(&mut app);
    let _passes = runs::scripted_passes(&mut app, PassScript::Answer);
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    click(&mut harness, Role::Tab, "Link checker");
    click(&mut harness, Role::RadioButton, "Internal pages");
    click(&mut harness, Role::CheckBox, "Select all shown rows");
    click(&mut harness, Role::Button, "Retry now");
    let app = harness.state();
    assert!(app.retry.is_none(), "the pass ended");
    assert_eq!(app.run_ui.broken.filter.scope, Scope::InternalPages);
    assert!(broken_rows(app).is_empty());
    assert!(
        app.run_ui.broken.broken_total() > 0,
        "other views still have broken rows"
    );
    // Not "No broken links found": assets and external links are still broken.
    assert!(shows_text(&harness, "No broken internal pages"));
    assert!(!shows_text(&harness, "No broken links found"));
}
