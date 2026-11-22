//! End to end with the real crawler and the crawler's link data (`--link-details`, on by default):
//! a site on `127.0.0.1` (`link_server`, which logs every request) whose three pages link to a
//! missing page and to a partner on another host, whose start page links to a page that answers
//! 500 and whose stylesheet references a missing image.
//!
//! *Found on* comes from the crawler for every status (the 500 too), as each URL arrives; after the
//! crawl every referring page with its link text and line; the external link has all its pages;
//! the URLs tab has Depth and In links and lists the pages linking to a URL. No page is requested
//! again after the crawl. With the switch off, the app reads the pages itself as before.

mod core_support;
mod link_server;
mod ui_common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use core_support::crawler_binary;
use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use link_server::LinkServer;
use siteone_crawler_gui::app::{App, Page};
use siteone_crawler_gui::model::config::Value;
use siteone_crawler_gui::model::run_state::RunOutcome;
use siteone_crawler_gui::ui::run::broken::{ShownRow, Where};
use siteone_crawler_gui::ui::run::urls::linking_pages;
use siteone_crawler_gui::ui::run::{Tab, broken_rows};
use ui_common::{Env, LARGE, harness, settle};

/// Runs frames until the crawl, the check and the link-text fetch have all ended.
fn wait_until_idle(harness: &mut Harness<'_, App>) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let app = harness.state();
        let busy =
            app.has_active_run() || app.link_check.is_some() || app.text_job.is_some() || app.pending_tasks() > 0;
        if !busy {
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
fn found_on_link_texts_and_positions_come_from_the_crawler_without_a_second_request() {
    let server = LinkServer::start();
    let site = server.url("/ld/");
    let env = Env::new();
    let mut app = env.app_with(Some(crawler_binary().to_path_buf()), Vec::new());
    app.fetch_link_texts = true;
    app.session.config.url = site.clone();
    let schema = app.schema.clone();
    app.session
        .config
        .set(&schema, "--max-reqs-per-sec", Value::Text("100".to_string()))
        .expect("set");
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    let run_dir = start(&mut harness);
    // The command says what the run does: link details are passed (the app's default).
    let argv = &harness.state().current_run.as_ref().expect("run").plan.argv;
    assert!(argv.iter().any(|arg| arg == "--link-details=true"), "{argv:?}");
    wait_until_idle(&mut harness);
    let run = harness.state().current_run.as_ref().expect("the run");
    assert_eq!(
        run.state.outcome(),
        RunOutcome::Finished,
        "{}",
        run.state.console.plain_text()
    );
    let ended = run.ended.expect("ended");

    // Every page was requested once — by the crawler; nothing after the crawl, nothing on the
    // partner's host.
    let counts = server.counts();
    for path in [
        "/ld/",
        "/ld/a",
        "/ld/b",
        "/ld/c",
        "/ld/style.css",
        "/ld/error",
        "/ld/missing",
    ] {
        assert_eq!(counts.get(path), Some(&1), "{path}: {counts:?}");
    }
    let requests = server.requests();
    assert!(
        requests.iter().all(|request| request.at <= ended),
        "a request after the crawl: {requests:?}"
    );
    assert!(
        requests.iter().all(|request| request.host() == "127.0.0.1"),
        "{requests:?}"
    );
    assert!(!run_dir.join("link-texts.json").exists(), "no page was read again");

    // Live, from the `url` event: the page that queued the 500 (not a 404) and where it links.
    let error = run
        .state
        .urls
        .iter()
        .find(|row| row.url.ends_with("/ld/error"))
        .expect("the 500");
    let link = error.link.as_deref().expect("the event names the page");
    assert_eq!(
        (&*link.source, &*link.attr, link.text.as_deref()),
        (site.as_str(), "<a href>", Some("Broken page"))
    );
    assert_eq!(link.line, Some(9));
    assert_eq!(error.depth, Some(1));

    // The Link checker: every status with its page; every page of the missing one, each with its
    // line, and the link text — all from the crawler.
    harness.state_mut().run_ui.tab = Tab::Broken;
    settle(&mut harness);
    let rows = broken_rows(harness.state());
    let broken = row(&rows, "/ld/error");
    assert_eq!((broken.code, broken.place), (500, Where::InternalPage));
    assert_eq!(broken.sources, std::slice::from_ref(&site));
    assert_eq!(broken.positions, [":9"]);
    assert_eq!(broken.link_text, "Broken page");
    let missing = row(&rows, "/ld/missing");
    assert_eq!(missing.code, 404);
    assert_eq!(missing.sources.len(), 3, "{missing:?}");
    assert!(missing.sources.iter().all(|page| page.contains("/ld/")));
    assert_eq!(missing.positions, [":5", ":5", ":5"]);
    assert_eq!((missing.link_text.as_str(), missing.more), ("the missing page", 0));
    let image = row(&rows, "/ld/img/missing.png");
    assert_eq!(image.sources, [server.url("/ld/style.css")]);
    assert_eq!(image.positions, [":2"]);
    assert_eq!(image.link_text, "in a stylesheet");
    // By page: the missing page is under each of its three pages.
    harness.state_mut().run_ui.broken.by_page = true;
    settle(&mut harness);
    let pages = siteone_crawler_gui::ui::run::broken_pages(harness.state());
    let under = pages
        .iter()
        .filter(|(_, rows)| rows.iter().any(|row| row.url.ends_with("/ld/missing")))
        .count();
    assert_eq!(under, 3, "{pages:#?}");
    assert!(
        pages.iter().all(|(page, _)| page.is_some()),
        "every link has its page: {pages:#?}"
    );

    // The external link: all three pages it is on, from the crawler's `links` table.
    let run = harness.state().current_run.as_ref().expect("the run");
    let list = run.links.list.as_ref().expect("the link list");
    let partner = list
        .links
        .iter()
        .find(|link| link.url.ends_with("/partner"))
        .expect("the partner link");
    assert_eq!((partner.sources.len(), partner.count), (3, 3), "{partner:?}");

    // The URLs tab: Depth and In links, and the pages linking to a URL.
    let graph = run.links.graph.clone().expect("the crawler's links table");
    let pages = linking_pages(&server.url("/ld/missing"), Some(&graph), None, false).expect("pages");
    assert_eq!(pages.lines.len(), 3);
    assert!(
        pages.lines[0].ends_with(":5 · <a href> · “the missing page”"),
        "{:?}",
        pages.lines
    );
    harness.state_mut().run_ui.tab = Tab::Urls;
    settle(&mut harness);
    let urls = &harness.state().run_ui.urls;
    let columns = urls.links.as_ref().expect("built");
    assert!(columns.has_in_links);
    assert!(harness.query_by_label("Sort by Depth").is_some() && harness.query_by_label("Sort by In links").is_some());
}

#[test]
fn without_link_details_the_app_reads_the_pages_itself_as_before() {
    let server = LinkServer::start();
    let site = server.url("/ld/");
    let env = Env::new();
    let mut app = env.app_with(Some(crawler_binary().to_path_buf()), Vec::new());
    app.fetch_link_texts = true;
    app.session.config.url = site.clone();
    let schema = app.schema.clone();
    for (name, value) in [
        ("--max-reqs-per-sec", Value::Text("100".to_string())),
        ("--link-details", Value::Bool(false)),
    ] {
        app.session.config.set(&schema, name, value).expect("set");
    }
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    let run_dir = start(&mut harness);
    assert!(
        harness
            .state()
            .current_run
            .as_ref()
            .expect("run")
            .plan
            .argv
            .iter()
            .any(|arg| arg == "--link-details=false")
    );
    wait_until_idle(&mut harness);
    let run = harness.state().current_run.as_ref().expect("the run");
    assert_eq!(run.state.outcome(), RunOutcome::Finished);
    assert!(run.links.graph.is_none(), "no links table without the option");
    // Found on from the event still (the first page of each URL), for the 500 too.
    harness.state_mut().run_ui.tab = Tab::Broken;
    settle(&mut harness);
    let rows = broken_rows(harness.state());
    assert_eq!(row(&rows, "/ld/error").sources, std::slice::from_ref(&site));
    let missing = row(&rows, "/ld/missing");
    assert_eq!(missing.sources.len(), 1, "only the page that queued it");
    // The link texts and lines were read by the app on those pages, after the crawl.
    assert_eq!(missing.link_text, "the missing page");
    assert_eq!(missing.positions, [":5"]);
    assert_eq!(row(&rows, "/ld/error").link_text, "Broken page");
    assert!(run_dir.join("link-texts.json").is_file());
    let counts = server.counts();
    assert_eq!(
        counts.get("/ld/"),
        Some(&2),
        "crawled, then read for the link texts: {counts:?}"
    );
    assert_eq!(counts.get("/ld/style.css"), Some(&2), "{counts:?}");
}
