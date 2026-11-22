//! End to end with the real crawler and two local sites on different hosts: the crawled site on
//! `127.0.0.1` (the crawler's `--serve-offline`) links to a second site on `localhost`
//! (`link_server`). The crawl makes no request to the second site; the app's check after it
//! requests each external link once (and nothing the pages reference); the dead link and the
//! working one (with its title) show up in Link checker with the page and the link text they were
//! found with; the URLs tab shows page titles; everything reloads from History after a restart;
//! *Check external links now* works on a run crawled with the switch off.

mod core_support;
mod link_server;
mod ui_common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use core_support::{crawler_binary, serve, write_site};
use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use link_server::LinkServer;
use siteone_crawler_gui::app::{App, Page};
use siteone_crawler_gui::model::config::Value;
use siteone_crawler_gui::model::run_state::RunOutcome;
use siteone_crawler_gui::ui::run::broken::{Scope, ShownRow, Where};
use siteone_crawler_gui::ui::run::{Tab, broken_rows};
use ui_common::{Env, LARGE, harness, settle, shows_text};

/// Runs frames until the crawl, the check and the link-text fetch have all ended.
fn wait_until_idle(harness: &mut Harness<'_, App>) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let app = harness.state();
        let busy =
            app.has_active_run() || app.link_check.is_some() || app.text_job.is_some() || app.pending_tasks() > 0;
        if !busy {
            // One more round: a finished result may start the check or the fetch.
            settle(harness);
            let app = harness.state();
            if !(app.link_check.is_some() || app.text_job.is_some() || app.has_active_run()) {
                return;
            }
        }
        assert!(Instant::now() < deadline, "the run did not settle");
        harness.step();
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn start(harness: &mut Harness<'_, App>) -> PathBuf {
    harness.state_mut().page = Page::Setup;
    settle(harness);
    harness.get_by_role_and_label(Role::Button, "Start crawl").click();
    harness.run_steps(2);
    assert_eq!(
        harness.state().page,
        Page::Run,
        "{:?}",
        harness.state().setup.start_error
    );
    harness
        .state()
        .current_run
        .as_ref()
        .expect("a run")
        .plan
        .run_dir
        .clone()
}

fn row<'a>(rows: &'a [ShownRow], suffix: &str) -> &'a ShownRow {
    rows.iter()
        .find(|row| row.url.ends_with(suffix))
        .unwrap_or_else(|| panic!("no row for {suffix} in {rows:#?}"))
}

#[test]
fn external_links_are_checked_by_the_app_after_the_crawl_and_not_by_the_crawler() {
    let external = LinkServer::start();
    let tmp = tempfile::tempdir().expect("temp dir");
    let site = tmp.path().join("site");
    write_site(&site, 3);
    let index = site.join("index.html");
    let html = std::fs::read_to_string(&index).expect("index");
    let links = format!(
        "<li><a href=\"/missing-page.html\">Gone page</a></li>\
         <li><a href=\"{ok}\">Partner site</a></li>\
         <li><a href=\"{dead}\">Dead partner</a></li></ul>\
         <img src=\"{image}\" alt=\"Remote image\"><script src=\"{script}\"></script>",
        ok = external.url_on("localhost", "/ok"),
        dead = external.url_on("localhost", "/404"),
        image = external.url_on("localhost", "/img.png"),
        script = external.url_on("localhost", "/app.js"),
    );
    std::fs::write(&index, html.replace("</ul>", &links)).expect("index");
    let server = serve(&site, &tmp.path().join("server"));

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

    // 1. Switch off: the crawl requests nothing on the other site, and the app does not either.
    let first = start(&mut harness);
    wait_until_idle(&mut harness);
    let run = harness.state().current_run.as_ref().expect("the run");
    assert_eq!(
        run.state.outcome(),
        RunOutcome::Finished,
        "{}",
        run.state.console.plain_text()
    );
    assert!(external.requests().is_empty(), "{:?}", external.requests());
    // The page titles of the crawled pages are on the URLs tab.
    harness.get_by_role_and_label(Role::Tab, "URLs").click();
    settle(&mut harness);
    assert!(shows_text(&harness, "Page 1"), "no page title on the URLs tab");
    // The internal 404 has its link text, read from the page it was found on (the crawled site).
    harness.get_by_role_and_label(Role::Tab, "Link checker").click();
    settle(&mut harness);
    let rows = broken_rows(harness.state());
    let gone = row(&rows, "/missing-page.html");
    assert_eq!((gone.code, gone.place), (404, Where::InternalPage));
    assert_eq!(gone.link_text, "Gone page");
    assert!(shows_text(&harness, "2 external links were found but not checked."));
    assert!(shows_text(&harness, "2 external files"));

    // Check now on this run: one request per link, none for the image or the script.
    harness
        .get_by_role_and_label(Role::Button, "Check external links now")
        .click();
    harness.run_steps(2);
    assert!(harness.state().link_check.is_some(), "the check started");
    wait_until_idle(&mut harness);
    let counts = external.counts();
    assert_eq!(counts.get("/ok"), Some(&1), "{counts:?}");
    assert_eq!(counts.get("/404"), Some(&1), "{counts:?}");
    assert_eq!(counts.len(), 2, "only the two links: {counts:?}");
    let rows = broken_rows(harness.state());
    let dead = row(&rows, "/404");
    assert_eq!(
        (dead.status.as_str(), dead.place, dead.checked),
        ("404", Where::External, true)
    );
    assert_eq!(dead.found_on(), Some(server.url().as_str()));
    assert_eq!(dead.link_text, "Dead partner");
    assert!(
        rows.iter().all(|row| !row.url.ends_with("/ok")),
        "a working link is not broken"
    );
    harness.state_mut().run_ui.broken.filter.scope = Scope::AllExternal;
    settle(&mut harness);
    let all = broken_rows(harness.state());
    let ok = row(&all, "/ok");
    assert_eq!((ok.status.as_str(), ok.title.as_deref()), ("200", Some("OK & Alive")));
    assert_eq!(ok.link_text, "Partner site");
    assert!(first.join("external-links.json").is_file());
    // The crawler reported every link text and line (`--link-details`): no page was read again.
    assert!(!first.join("link-texts.json").exists());

    // 2. Switch on: the check runs by itself after the crawl — never during it.
    harness.state_mut().run_ui.broken.filter.scope = Scope::AllBroken;
    harness.state_mut().session.config.check_external_links = true;
    let before = external.requests().len();
    let second = start(&mut harness);
    assert_ne!(first, second);
    wait_until_idle(&mut harness);
    let run = harness.state().current_run.as_ref().expect("the run");
    let ended = run.ended.expect("the crawl ended");
    let requests = external.requests();
    let new: Vec<_> = requests.iter().skip(before).collect();
    assert_eq!(new.len(), 2, "{new:?}");
    assert!(
        new.iter().all(|request| request.at >= ended),
        "a request during the crawl"
    );
    let report = run.links.report.as_ref().expect("the check ran");
    assert!(report.complete && report.results.len() == 2, "{report:?}");

    // 3. After a restart the run reopens from History with its check and link texts.
    drop(harness);
    let mut app = env.app_with(Some(crawler_binary().to_path_buf()), Vec::new());
    app.fetch_link_texts = true;
    let mut harness = ui_common::harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.state_mut().open_past_run(second.clone());
    settle(&mut harness);
    harness.state_mut().run_ui.tab = Tab::Broken;
    settle(&mut harness);
    let rows = broken_rows(harness.state());
    let dead = row(&rows, "/404");
    assert_eq!((dead.status.as_str(), dead.link_text.as_str()), ("404", "Dead partner"));
    assert!(shows_text(&harness, "2 external links checked: 1 OK, 1 broken"));
    assert_eq!(external.requests().len(), before + 2, "reopening requests nothing");
}
