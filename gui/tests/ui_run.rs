//! Interaction tests of the Run and History pages (GUI-SPEC §8): the URL table filters and sorts;
//! Stop → Stopping → Force quit; a finished run is recorded and reopens from History after a
//! restart with the same data; Run again loads the configuration; Delete removes the run folder;
//! closing the window during a run asks first; the Link checker tab filters and joins Found on;
//! and a performance guard with 100 000 URL rows.

mod ui_common;

use std::time::{Duration, Instant};

use eframe::egui::accesskit::Role;
use eframe::egui::{self, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use siteone_crawler_gui::app::{App, Page, PastLoad, ResultState, ViewedRun};
use siteone_crawler_gui::model::config::Value;
use siteone_crawler_gui::model::events::{Event, UrlEvent};
use siteone_crawler_gui::model::history::{RecordedOutcome, RunRecord};
use siteone_crawler_gui::model::results::ResultLoad;
use siteone_crawler_gui::model::run_state::{RunOutcome, StatusClass};
use siteone_crawler_gui::ui::run::broken::Where;
use siteone_crawler_gui::ui::run::urls::SortKey;
use siteone_crawler_gui::ui::run::{Tab, broken_rows};
use ui_common::runs;
use ui_common::{Env, LARGE, harness, settle, shows_text};

fn click(harness: &mut Harness<'_, App>, role: Role, label: &str) {
    harness.get_by_role_and_label(role, label).click();
    settle(harness);
}

fn type_into(harness: &mut Harness<'_, App>, label: &str, text: &str) {
    harness.get_by_role_and_label(Role::TextInput, label).focus();
    harness.run();
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.key_press(Key::Delete);
    harness.run();
    if !text.is_empty() {
        harness.get_by_role_and_label(Role::TextInput, label).type_text(text);
    }
    settle(harness);
}

/// The URLs the URLs tab shows now, in order.
fn shown_urls(app: &App) -> Vec<String> {
    let state = &app.current_run.as_ref().expect("a run").state;
    app.run_ui
        .urls
        .index
        .rows()
        .iter()
        .map(|index| state.urls[*index as usize].url.to_string())
        .collect()
}

fn record_of(control: &runs::Control) -> RunRecord {
    RunRecord::read(&control.run_dir).expect("run.json")
}

#[test]
fn the_url_table_filters_and_sorts() {
    let env = Env::new();
    let mut app = env.app();
    let _control = runs::finished(&mut app);
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    click(&mut harness, Role::Tab, "URLs");
    assert_eq!(shown_urls(harness.state()).len(), 73);

    type_into(&mut harness, "Filter URLs", "SITEMAP.XML");
    let shown = shown_urls(harness.state());
    assert!(
        !shown.is_empty() && shown.iter().all(|url| url.contains("sitemap.xml")),
        "{shown:?}"
    );
    // The filter also reads the page titles of the result JSON's `seo` table.
    type_into(&mut harness, "Filter URLs", "Key Features •");
    let shown = shown_urls(harness.state());
    assert!(
        !shown.is_empty() && shown.iter().all(|url| url.contains("/introduction/key-features/")),
        "{shown:?}"
    );
    assert!(shows_text(&harness, "Key Features • SiteOne Crawler"));
    type_into(&mut harness, "Filter URLs", "");
    assert_eq!(shown_urls(harness.state()).len(), 73);

    // Status-class toggles: without 2xx and 3xx only the errors are left.
    click(&mut harness, Role::CheckBox, "2xx");
    click(&mut harness, Role::CheckBox, "3xx");
    {
        let app = harness.state();
        let state = &app.current_run.as_ref().expect("run").state;
        let statuses: Vec<i32> = app
            .run_ui
            .urls
            .index
            .rows()
            .iter()
            .map(|index| state.urls[*index as usize].status)
            .collect();
        assert_eq!(statuses.len() as u64, state.status_counts.errors());
        assert!(
            statuses.iter().all(|status| *status >= 400 || *status < 0),
            "{statuses:?}"
        );
    }
    click(&mut harness, Role::CheckBox, "2xx");
    click(&mut harness, Role::CheckBox, "3xx");

    // Sorting by time: ascending, then descending.
    click(&mut harness, Role::Button, "Sort by Time");
    let times = |app: &App| -> Vec<u32> {
        let state = &app.current_run.as_ref().expect("run").state;
        app.run_ui
            .urls
            .index
            .rows()
            .iter()
            .map(|index| state.urls[*index as usize].time_ms)
            .collect()
    };
    let ascending = times(harness.state());
    assert!(ascending.windows(2).all(|pair| pair[0] <= pair[1]), "{ascending:?}");
    assert_eq!(harness.state().run_ui.urls.sort, SortKey::Time);
    assert!(!harness.state().run_ui.urls.follow, "sorting switches Follow off");
    click(&mut harness, Role::Button, "Sort by Time");
    let descending = times(harness.state());
    assert!(descending.windows(2).all(|pair| pair[0] >= pair[1]), "{descending:?}");

    // Ctrl+F focuses the filter of the tab.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::F);
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "Filter URLs")
        .type_text("faq");
    settle(&mut harness);
    assert!(shown_urls(harness.state()).iter().all(|url| url.contains("faq")));
}

#[test]
fn stop_becomes_force_quit_which_asks_and_aborts() {
    let env = Env::new();
    let mut app = env.app();
    let control = runs::live(&mut app);
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    assert!(
        record_of(&control).outcome.is_none(),
        "the initial record has no outcome"
    );

    click(&mut harness, Role::Button, "Stop");
    assert_eq!(control.calls(), ["stop"]);
    let outcome = harness.state().current_run.as_ref().expect("run").state.outcome();
    assert_eq!(outcome, RunOutcome::Stopping);
    assert!(shows_text(&harness, "Stopping…"));

    click(&mut harness, Role::Button, "Force quit");
    assert!(harness.state().dialog.is_some(), "Force quit asks first");
    assert_eq!(control.calls(), ["stop"], "nothing killed before the confirmation");
    click(&mut harness, Role::Button, "End the crawl now");
    assert_eq!(control.calls(), ["stop", "kill"]);
    let run = harness.state().current_run.as_ref().expect("run");
    assert_eq!(run.state.outcome(), RunOutcome::Aborted);
    assert!(!harness.state().has_active_run());
    let record = record_of(&control);
    assert_eq!(record.outcome, Some(RecordedOutcome::Aborted));
    assert_eq!(record.summary.urls, 248);
    assert!(shows_text(&harness, "force-quit"));
}

#[test]
fn a_finished_run_is_recorded_and_reopens_from_history_after_a_restart() {
    let env = Env::new();
    let run_dir;
    {
        let mut app = env.app();
        let control = runs::finished(&mut app);
        run_dir = control.run_dir.clone();
        let mut harness = harness(app, LARGE, egui::Theme::Light);
        settle(&mut harness);
        let run = harness.state().current_run.as_ref().expect("run");
        assert_eq!(run.state.outcome(), RunOutcome::Finished);
        assert!(matches!(run.result, ResultState::Ready(ResultLoad::Loaded(_))));
        let record = record_of(&control);
        assert_eq!(record.outcome, Some(RecordedOutcome::Finished));
        assert_eq!(record.summary.overall_score, Some(8.3));
        assert_eq!(record.summary.overall_label.as_deref(), Some("Good"));
        assert_eq!(record.duration_ms, Some(25_132));
        assert!(shows_text(&harness, "Overall score 8.3 of 10, Good"));
        // The run is listed in History.
        click(&mut harness, Role::Button, "History");
        assert_eq!(harness.state().history_ui.rows.len(), 1);
    }

    // A restart: a new app on the same data folder.
    let app = env.app();
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    click(&mut harness, Role::Button, "History");
    assert!(shows_text(&harness, runs::SITE));
    harness.state_mut().history_ui.selected = Some(run_dir.clone());
    settle(&mut harness);
    click(&mut harness, Role::Button, "Open");
    let app = harness.state();
    assert_eq!(app.page, Page::Run);
    assert_eq!(
        app.viewed_run,
        ViewedRun::Past {
            run_dir: run_dir.clone()
        }
    );
    let Some(PastLoad::Ready(data)) = app.past_run.as_ref().map(|past| &past.load) else {
        panic!("the past run did not load");
    };
    assert_eq!(data.state.urls.len(), 73);
    assert_eq!(data.state.outcome(), RunOutcome::Finished);
    assert!(matches!(&data.result, ResultLoad::Loaded(result) if result.tables.len() == 27));
    assert!(!data.state.console.is_empty(), "the console comes from console.log");
    assert!(shows_text(&harness, "Overall score 8.3 of 10, Good"));
    click(&mut harness, Role::Tab, "Link checker");
    assert_eq!(harness.state().run_ui.broken.index.total(), 6);
}

#[test]
fn a_run_cut_short_by_the_app_reads_as_unfinished() {
    let env = Env::new();
    let mut app = env.app();
    let control = runs::live(&mut app);
    // The events file of a run the GUI did not see to its end.
    let lines: Vec<String> = runs::site_urls(5, 310);
    std::fs::write(control.file("events.ndjson"), lines.join("\n")).expect("events");
    drop(app);
    let app = env.app();
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.state_mut().open_past_run(control.run_dir.clone());
    settle(&mut harness);
    let Some(PastLoad::Ready(data)) = harness.state().past_run.as_ref().map(|past| &past.load) else {
        panic!("not loaded");
    };
    assert_eq!(data.state.outcome(), RunOutcome::Aborted);
    assert_eq!(data.state.urls.len(), 5);
    assert!(shows_text(&harness, "This run did not finish"));
}

#[test]
fn run_again_loads_the_configuration_and_names_what_is_missing() {
    let env = Env::new();
    let dir = runs::record(
        &env.runs_dir(),
        "20261118-090507-crawler.siteone.io",
        runs::SITE,
        Some(RecordedOutcome::Finished),
        Some(8.3),
        73,
    );
    let mut record = RunRecord::read(&dir).expect("record");
    let app = env.app();
    let schema = app.schema.clone();
    record
        .config
        .set(&schema, "--workers", Value::Text("7".to_string()))
        .expect("set");
    record.argv.push("--workers=7".to_string());
    record.argv.push("--http-auth=••••".to_string());
    record.argv.push("--header:=Cookie: ***".to_string());
    record.write(&dir).expect("written");

    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    click(&mut harness, Role::Button, "History");
    harness.state_mut().history_ui.selected = Some(dir);
    settle(&mut harness);
    click(&mut harness, Role::Button, "Run again");
    let app = harness.state();
    assert_eq!(app.page, Page::Setup);
    assert_eq!(app.session.config.url, runs::SITE);
    assert_eq!(
        app.session.config.values.get("--workers"),
        Some(&Value::Text("7".to_string()))
    );
    let notice = app
        .toasts
        .items()
        .last()
        .map(|toast| toast.text.clone())
        .unwrap_or_default();
    assert!(
        notice.contains("--http-auth") && notice.contains("--header"),
        "{notice}"
    );
}

#[test]
fn delete_removes_the_run_folder_after_confirming() {
    let env = Env::new();
    runs::history(&env.runs_dir());
    let dir = env.runs_dir().join("20261117-163012-www.example.com");
    let mut harness = harness(env.app(), LARGE, egui::Theme::Light);
    settle(&mut harness);
    click(&mut harness, Role::Button, "History");
    assert_eq!(harness.state().history_ui.rows.len(), 7);
    harness.state_mut().history_ui.selected = Some(dir.clone());
    settle(&mut harness);
    click(&mut harness, Role::Button, "Delete");
    let message = harness
        .state()
        .dialog
        .as_ref()
        .map(|dialog| dialog.message.clone())
        .unwrap_or_default();
    assert!(
        message.contains("20261117-163012-www.example.com") && message.contains(" B)"),
        "{message}"
    );
    assert!(dir.exists());
    click(&mut harness, Role::Button, "Delete run");
    assert!(!dir.exists());
    assert_eq!(harness.state().history_ui.rows.len(), 6);

    // A damaged run can be deleted too.
    let damaged = runs::damaged(&env.runs_dir());
    harness.state_mut().history_ui.stale = true;
    settle(&mut harness);
    assert!(shows_text(&harness, "Damaged"));
    harness.state_mut().history_ui.selected = Some(damaged.clone());
    settle(&mut harness);
    click(&mut harness, Role::Button, "Delete");
    click(&mut harness, Role::Button, "Delete run");
    assert!(!damaged.exists());
}

#[test]
fn closing_the_window_during_a_run_asks_first_and_records_an_abort() {
    let env = Env::new();
    let mut app = env.app();
    let control = runs::live(&mut app);
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);

    let close = |harness: &mut Harness<'_, App>| {
        harness
            .input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        harness.step();
    };
    close(&mut harness);
    let cancelled = harness
        .output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .is_some_and(|viewport| viewport.commands.contains(&egui::ViewportCommand::CancelClose));
    assert!(cancelled, "the close was cancelled");
    settle(&mut harness);
    assert!(shows_text(&harness, "A crawl is running"));
    assert!(harness.state().has_active_run());

    // Cancel keeps everything as it was.
    click(&mut harness, Role::Button, "Cancel");
    assert!(harness.state().has_active_run());
    assert!(control.calls().is_empty());

    close(&mut harness);
    settle(&mut harness);
    harness
        .get_by_role_and_label(Role::Button, "Stop the crawl and close")
        .click();
    harness.step();
    let closing = harness
        .output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .is_some_and(|viewport| viewport.commands.contains(&egui::ViewportCommand::Close));
    assert!(closing, "the window closes");
    assert_eq!(control.calls(), ["kill"]);
    assert!(harness.state().close_confirmed);
    assert_eq!(record_of(&control).outcome, Some(RecordedOutcome::Aborted));
}

/// eframe does not call `App::ui` while the window is minimized or covered, only `App::logic`:
/// a run must still be pumped, finalized and recorded then, and a close must still ask first.
#[test]
fn a_run_is_pumped_and_a_close_asks_first_while_the_window_is_hidden() {
    let env = Env::new();
    let mut app = env.app();
    let control = runs::finished(&mut app);
    let ctx = egui::Context::default();
    let hidden = |close: bool| {
        let mut input = egui::RawInput::default();
        let viewport = input.viewports.entry(egui::ViewportId::ROOT).or_default();
        viewport.minimized = Some(true);
        if close {
            viewport.events.push(egui::ViewportEvent::Close);
        }
        input
    };
    let _ = ctx.run_logic(&hidden(false), |ctx| app.logic(ctx));
    assert!(app.wait_for_background(Duration::from_secs(10)));
    assert_eq!(record_of(&control).outcome, Some(RecordedOutcome::Finished));
    assert!(matches!(
        app.current_run.as_ref().expect("run").result,
        ResultState::Ready(ResultLoad::Loaded(_))
    ));

    let mut app = env.app();
    let _control = runs::live(&mut app);
    let output = ctx.run_logic(&hidden(true), |ctx| app.logic(ctx));
    let commands = output
        .viewport_commands
        .get(&egui::ViewportId::ROOT)
        .cloned()
        .unwrap_or_default();
    assert!(commands.contains(&egui::ViewportCommand::CancelClose), "{commands:?}");
    assert!(
        commands.contains(&egui::ViewportCommand::Minimized(false)),
        "the window comes back to show the question: {commands:?}"
    );
    assert!(app.dialog.is_some());
    assert!(app.has_active_run());
}

/// Exiting the app applies what the crawler already reported: a run that has ended is recorded
/// with its real outcome, not as aborted.
#[test]
fn an_exit_records_a_run_that_has_already_ended_with_its_outcome() {
    let env = Env::new();
    let mut app = env.app();
    let control = runs::finished(&mut app);
    // Its messages, the exit included, were never drawn into a frame.
    app.flush();
    app.abort_for_exit();
    assert_eq!(record_of(&control).outcome, Some(RecordedOutcome::Finished));
}

/// Every control on every page has an accessible name (GUI-SPEC §6.1).
#[test]
fn every_control_on_every_page_has_an_accessible_name() {
    use egui_kittest::kittest::NodeT;
    use siteone_crawler_gui::ui::setup::SetupView;

    const CONTROLS: &[Role] = &[
        Role::Button,
        Role::CheckBox,
        Role::Switch,
        Role::RadioButton,
        Role::TextInput,
        Role::MultilineTextInput,
        Role::SearchInput,
        Role::ComboBox,
        Role::SpinButton,
        Role::Slider,
        Role::Tab,
        Role::Link,
        Role::MenuItem,
    ];
    let unnamed = |harness: &Harness<'_, App>, page: &str| -> Vec<String> {
        let controls: Vec<_> = harness.query_all_by(|node| CONTROLS.contains(&node.role())).collect();
        assert!(controls.len() > 5, "{page}: only {} controls found", controls.len());
        controls
            .into_iter()
            .map(|node| node.accesskit_node())
            .filter(|node| node.label().is_none_or(|label| label.trim().is_empty()))
            .map(|node| format!("{page}: {:?} {:?} {:?}", node.role(), node.value(), node.bounding_box()))
            .collect()
    };

    let env = Env::new();
    let mut app = env.app();
    app.session.config.ai_enabled = true;
    let _control = runs::finished_ai(&mut app);
    app.page = Page::Setup;
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    let mut missing = unnamed(&harness, "Setup");
    harness.state_mut().setup.view = SetupView::AllOptions;
    harness.state_mut().setup.search = "--".to_string();
    settle(&mut harness);
    missing.extend(unnamed(&harness, "All options"));
    harness.state_mut().setup.search.clear();

    harness.state_mut().page = Page::Run;
    for tab in [
        Tab::Overview,
        Tab::Urls,
        Tab::Broken,
        Tab::Findings,
        Tab::Tables,
        Tab::Ai,
        Tab::Files,
        Tab::Console,
    ] {
        harness.state_mut().run_ui.tab = tab;
        settle(&mut harness);
        missing.extend(unnamed(&harness, &format!("Run {tab:?}")));
    }
    for page in [Page::History, Page::Settings] {
        harness.state_mut().page = page;
        settle(&mut harness);
        missing.extend(unnamed(&harness, page.label()));
    }
    assert!(missing.is_empty(), "controls without a name:\n{}", missing.join("\n"));
}

#[test]
fn the_broken_links_tab_filters_and_joins_found_on() {
    let env = Env::new();
    let mut app = env.app();
    let _control = runs::finished(&mut app);
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    click(&mut harness, Role::Tab, "Link checker");
    let shown = |harness: &Harness<'_, App>| broken_rows(harness.state());
    let all = shown(&harness);
    assert_eq!(all.len(), 6);
    // Default order: internal pages, internal assets, external.
    let places: Vec<Where> = all.iter().map(|row| row.place).collect();
    assert!(places.windows(2).all(|pair| pair[0] <= pair[1]), "{places:?}");
    // Found on comes from the `404` table; a 429 is not in it.
    let not_found = all
        .iter()
        .find(|row| row.url == runs::FIXTURE_404)
        .expect("the 404 row");
    assert_eq!(not_found.found_on(), Some(runs::FIXTURE_404_SOURCE));
    assert!(
        all.iter()
            .filter(|row| row.code == 429)
            .all(|row| row.found_on().is_none())
    );
    assert!(shows_text(&harness, "not reported by the crawler"));
    // The run's external links were not checked: the banner offers the check.
    assert!(shows_text(&harness, "88 external links were found but not checked."));
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Check external links now")
            .is_some()
    );
    // What was not checked, from the result JSON and the link list.
    assert!(shows_text(
        &harness,
        "Not checked: 6 URLs disallowed by robots.txt · 1 external file"
    ));

    // An older run that crawled an external URL still lists it as External.
    click(&mut harness, Role::RadioButton, "External broken");
    let external = shown(&harness);
    assert_eq!(external.len(), 1);
    assert_eq!((external[0].code, external[0].checked), (-1, false));
    click(&mut harness, Role::RadioButton, "Internal assets");
    let assets = shown(&harness);
    assert_eq!(assets.len(), 1);
    assert!(assets[0].url.ends_with("hero-background.webp"));
    click(&mut harness, Role::RadioButton, "All broken");

    // The text filter reads the URL and Found on.
    type_into(&mut harness, "Filter links", "performance-metrics");
    let matched = shown(&harness);
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].url, runs::FIXTURE_404);
    type_into(&mut harness, "Filter links", "");

    // The Overview's breakdown opens the tab for one status class.
    click(&mut harness, Role::Tab, "Overview");
    click(&mut harness, Role::Button, "Show the 4xx broken links");
    assert_eq!(harness.state().run_ui.tab, Tab::Broken);
    assert_eq!(
        harness.state().run_ui.broken.filter.class,
        Some(StatusClass::ClientError)
    );
    assert!(shown(&harness).iter().all(|row| (400..500).contains(&row.code)));
    click(&mut harness, Role::Tab, "Overview");
    click(&mut harness, Role::Button, "Errors");
    assert_eq!(harness.state().run_ui.tab, Tab::Broken);
    assert_eq!(shown(&harness).len(), 6);
}

/// Adds `count` synthetic URL rows to the current run (every 10th broken).
fn add_rows(app: &mut App, from: u64, count: u64) {
    let run = app.current_run.as_mut().expect("run");
    let now = Instant::now();
    for n in from..from + count {
        let status = if n % 10 == 3 { 404 } else { 200 };
        let path = if n % 7 == 0 {
            format!("assets/image-{n}.png")
        } else {
            format!("section-{}/page-{n}/", n % 50)
        };
        run.state.apply(
            Event::Url(UrlEvent {
                url: format!("https://crawler.siteone.io/{path}"),
                status,
                content_type: if n % 7 == 0 { "Image" } else { "HTML" }.to_string(),
                time_ms: (n * 7919) % 3000,
                size: 1000 + n % 50_000,
                cached: n % 3 == 0,
                done: n + 1,
                total: 200_000,
            }),
            now,
        );
    }
}

#[test]
fn the_url_and_broken_links_tabs_stay_fluid_with_100_000_rows() {
    let env = Env::new();
    let mut app = env.app();
    let control = runs::attach(&mut app, runs::SITE);
    runs::run_started(&control, runs::SITE);
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    add_rows(harness.state_mut(), 0, 100_000);

    for tab in ["URLs", "Link checker"] {
        let started = Instant::now();
        click(&mut harness, Role::Tab, tab);
        let first = started.elapsed();
        assert!(first < Duration::from_secs(10), "{tab}: first frames took {first:?}");
    }
    assert_eq!(harness.state().run_ui.broken.index.total(), 10_000);

    click(&mut harness, Role::Tab, "URLs");
    // Frames without new data do no O(n) work: many frames stay cheap.
    let started = Instant::now();
    for _ in 0..30 {
        harness.step();
    }
    let idle = started.elapsed();
    assert!(idle < Duration::from_secs(6), "30 frames took {idle:?}");

    // A filter is applied within a generous bound.
    let started = Instant::now();
    type_into(&mut harness, "Filter URLs", "page-4242");
    let filtered = started.elapsed();
    assert!(filtered < Duration::from_secs(10), "filtering took {filtered:?}");
    assert!(shown_urls(harness.state()).iter().all(|url| url.contains("page-4242")));
    type_into(&mut harness, "Filter URLs", "");

    // New rows are merged into the index without rebuilding it.
    let rebuilds = harness.state().run_ui.urls.index.rebuilds;
    let broken_rebuilds = harness.state().run_ui.broken.view.rebuilds;
    add_rows(harness.state_mut(), 100_000, 50);
    harness.run();
    click(&mut harness, Role::Tab, "Link checker");
    click(&mut harness, Role::Tab, "URLs");
    assert_eq!(harness.state().run_ui.urls.index.rows().len(), 100_050);
    assert_eq!(harness.state().run_ui.urls.index.rebuilds, rebuilds);
    assert_eq!(harness.state().run_ui.broken.view.rebuilds, broken_rebuilds);
    assert_eq!(harness.state().run_ui.broken.index.total(), 10_005);
    // A sorted index takes new rows in their place.
    click(&mut harness, Role::Button, "Sort by Size");
    add_rows(harness.state_mut(), 100_050, 20);
    harness.run();
    let app = harness.state();
    let state = &app.current_run.as_ref().expect("run").state;
    let sizes: Vec<u64> = app
        .run_ui
        .urls
        .index
        .rows()
        .iter()
        .map(|index| state.urls[*index as usize].size)
        .collect();
    assert_eq!(sizes.len(), 100_070);
    assert!(sizes.windows(2).all(|pair| pair[0] <= pair[1]));
    drop(control);
}

#[test]
fn the_external_link_check_shows_progress_and_stop_keeps_what_was_checked() {
    let env = Env::new();
    let mut app = env.app();
    let control = runs::finished(&mut app);
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    runs::with_check(harness.state_mut(), 31, true);
    click(&mut harness, Role::Tab, "Link checker");
    assert!(shows_text(&harness, "External links: in progress, 31 / 88 links"));
    assert!(shows_text(&harness, "Checking external links: 31 of 88."));
    // The flagged links join the broken ones, with their link text and the page they were on.
    let rows = broken_rows(harness.state());
    let checked: Vec<_> = rows.iter().filter(|row| row.checked).collect();
    assert!(
        !checked.is_empty() && checked.iter().all(|row| row.status != "200"),
        "{checked:?}"
    );
    assert!(checked.iter().all(|row| row.found_on().is_some()));
    let refused = checked
        .iter()
        .find(|row| row.status.starts_with("Refused"))
        .expect("a refused link");
    assert_eq!(refused.status, "Refused 403");
    let internal = rows.iter().find(|row| row.url == runs::FIXTURE_404).expect("the 404");
    assert_eq!(internal.link_text, "Content type analysis");
    type_into(&mut harness, "Filter links", "content type analysis");
    assert_eq!(broken_rows(harness.state()).len(), 1, "the filter reads the link text");
    type_into(&mut harness, "Filter links", "");

    // Stop check (the header's primary action): what was checked is kept, marked as stopped.
    let stop = harness
        .query_all_by_role_and_label(Role::Button, "Stop check")
        .next()
        .expect("Stop check");
    stop.click();
    settle(&mut harness);
    let app = harness.state();
    assert!(app.link_check.is_none());
    let report = app
        .current_run
        .as_ref()
        .expect("run")
        .links
        .report
        .clone()
        .expect("a report");
    assert!(!report.complete);
    assert_eq!(report.results.len(), 31);
    let stored = siteone_crawler_gui::model::link_check::CheckReport::read(&control.run_dir)
        .expect("readable")
        .expect("written");
    assert_eq!(stored, report);
    assert!(shows_text(
        &harness,
        "The external link check was stopped after 31 of 88 links."
    ));
    assert!(shows_text(&harness, "External links: ended early, Stopped at 31 / 88"));
    let check_now = harness.get_by_role_and_label(Role::Button, "Check external links now");
    assert!(!check_now.accesskit_node().is_disabled());

    // The all-external view lists the working links too, broken first.
    click(&mut harness, Role::RadioButton, "All external");
    let all = broken_rows(harness.state());
    assert_eq!(
        all.len(),
        32,
        "31 checked and the crawled external URL of the older kind"
    );
    assert!(all.iter().any(|row| row.status == "200" && row.title.is_some()));
    let first_ok = all.iter().position(|row| row.status == "200").expect("an OK link");
    assert!(all[..first_ok].iter().all(|row| row.status != "200"));
}
