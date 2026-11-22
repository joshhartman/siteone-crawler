//! Credentials of a run and of an import (GUI-SPEC §4.8, §4.9, §5.6):
//!
//! - follow-up requests of the current run (link texts, retries of crawled URLs) and its *Run
//!   again* use the credentials the run was started with — never what Setup holds now;
//! - a run from History keeps no password and no headers: its follow-ups that would need them are
//!   not made, *Run again* clears Setup's passwords, and no value of this session is borrowed;
//! - an import replaces the configuration's passwords (the AI API key aside) and leaves the saved
//!   profiles' passwords alone.
//!
//! Requests go to a local server (`link_server`) only. Credentials are dummies, and the assertions
//! never print one.

mod link_server;
mod ui_common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable;
use link_server::LinkServer;
use siteone_crawler_gui::app::{App, Page, PastLoad};
use siteone_crawler_gui::backend::retry::RetryTarget;
use siteone_crawler_gui::model::config::{self, AI_API_KEY, Value};
use siteone_crawler_gui::model::history::RunRecord;
use siteone_crawler_gui::model::link_text::PageTexts;
use siteone_crawler_gui::ui::setup::ops;
use ui_common::runs::{self, PassScript};
use ui_common::{Env, LARGE, harness, settle};

const HTTP_AUTH: &str = "--http-auth";
const ALICE: &str = "alice:site-a-password";
const BOB: &str = "bob:site-b-password";
/// `Basic` + base64 of [`ALICE`].
const ALICE_BASIC: &str = "Basic YWxpY2U6c2l0ZS1hLXBhc3N3b3Jk";

/// A finished run of the local site `server` (the fixture's result, its URLs moved there).
fn finish_local_run(app: &mut App, server: &LinkServer) -> PathBuf {
    let site = server.url("/");
    let control = runs::attach(app, &site);
    runs::run_started(&control, &site);
    let fixture = runs::FIXTURE.replace("https://crawler.siteone.io", site.trim_end_matches('/'));
    std::fs::write(control.file("result.json"), fixture).expect("result.json");
    control.event(r#"{"type":"runFinished","outcome":"success","exitCode":0,"ms":1,"interrupted":false}"#);
    control.exit(Some(0));
    let ctx = egui::Context::default();
    app.logic(&ctx);
    assert!(app.wait_for_background(Duration::from_secs(5)));
    control.run_dir.clone()
}

/// Runs the app's logic until `done` or 5 s.
fn pump_until(app: &mut App, mut done: impl FnMut(&App) -> bool) {
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(app) && Instant::now() < deadline {
        app.logic(&ctx);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn set_header(app: &mut App, header: &str) {
    let schema = app.schema.clone();
    app.session
        .config
        .set(&schema, "--header", Value::List(vec![header.to_string()]))
        .expect("header");
}

// ---------------------------------------------------------------------------------------------
// The current run
// ---------------------------------------------------------------------------------------------

#[test]
fn source_pages_use_the_original_runs_password() {
    let server = LinkServer::start();
    let env = Env::new();
    let mut app = env.app();
    app.secrets.set(HTTP_AUTH, ALICE);
    set_header(&mut app, "X-Site-Token: site-a-token");
    finish_local_run(&mut app, &server);
    // Editing Setup or switching profiles after the start (here: before the fetch begins).
    app.secrets.set(HTTP_AUTH, BOB);
    set_header(&mut app, "X-Site-Token: site-b-token");
    app.fetch_link_texts = true;
    pump_until(&mut app, |_| !server.requests().is_empty());
    let requests = server.requests();
    assert!(!requests.is_empty(), "the source-page request did not arrive");
    for request in &requests {
        assert!(
            request.header("authorization") == Some(ALICE_BASIC),
            "{}: not the run's own password",
            request.path
        );
        assert!(
            request.header("x-site-token") == Some("site-a-token"),
            "{}: not the run's own header",
            request.path
        );
    }
}

#[test]
fn a_run_started_without_a_password_sends_none_that_setup_got_later() {
    let server = LinkServer::start();
    let env = Env::new();
    let mut app = env.app();
    finish_local_run(&mut app, &server);
    app.secrets.set(HTTP_AUTH, BOB);
    app.fetch_link_texts = true;
    pump_until(&mut app, |_| !server.requests().is_empty());
    let requests = server.requests();
    assert!(!requests.is_empty(), "the source-page request did not arrive");
    assert!(
        requests.iter().all(|request| request.header("authorization").is_none()),
        "a password the run did not have was sent"
    );
}

#[test]
fn a_retry_and_run_again_of_the_current_run_use_its_own_password() {
    let env = Env::new();
    let mut app = env.app();
    app.secrets.set(HTTP_AUTH, ALICE);
    runs::finished(&mut app);
    let passes = runs::scripted_passes(&mut app, PassScript::Answer);
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    let app = harness.state_mut();
    app.secrets.set(HTTP_AUTH, BOB);
    app.secrets.set(AI_API_KEY, "sk-setup-key");

    app.retry_now(&[runs::FIXTURE_404.to_string()]);
    let pass = passes.lock().expect("passes").pop().expect("a pass");
    let RetryTarget::Crawled { options } = &pass[0].target else {
        panic!("the crawled URL was not retried");
    };
    assert!(
        options.contains(&format!("{HTTP_AUTH}={ALICE}")),
        "not the run's password"
    );
    assert!(!options.iter().any(|arg| arg.contains("bob")), "Setup's password");
    settle(&mut harness);

    // Run again restores the run's password; the API key is Setup's (a provider key).
    harness.state_mut().run_again_current();
    settle(&mut harness);
    assert!(
        harness.state().dialog.is_some(),
        "replacing Setup's passwords is confirmed first"
    );
    harness.get_by_label("Replace").click();
    settle(&mut harness);
    let app = harness.state();
    assert_eq!(app.page, Page::Setup);
    assert!(app.secrets.get(HTTP_AUTH) == Some(ALICE), "not the run's password");
    assert_eq!(app.secrets.get(AI_API_KEY), Some("sk-setup-key"));
}

// ---------------------------------------------------------------------------------------------
// A run from History
// ---------------------------------------------------------------------------------------------

#[test]
fn a_run_from_history_borrows_no_password_for_its_source_pages() {
    let server = LinkServer::start();
    let env = Env::new();
    let mut app = env.app();
    app.secrets.set(HTTP_AUTH, ALICE);
    let run_dir = finish_local_run(&mut app, &server);
    drop(app);

    // After a restart, Setup holds another site's password.
    let mut app = env.app();
    app.secrets.set(HTTP_AUTH, BOB);
    app.open_past_run(run_dir.clone());
    assert!(app.wait_for_background(Duration::from_secs(5)));
    // Its external links were checked now: their source pages are wanted.
    let Some(PastLoad::Ready(data)) = app.past_run.as_mut().map(|past| &mut past.load) else {
        panic!("the past run did not load");
    };
    data.links.texts_external = true;
    data.links.texts_settled = false;
    app.fetch_link_texts = true;
    pump_until(
        &mut app,
        |app| matches!(app.past_run.as_ref().map(|past| &past.load), Some(PastLoad::Ready(data)) if !data.links.texts.pages.is_empty()),
    );
    assert!(app.wait_for_background(Duration::from_secs(5)));
    assert!(server.requests().is_empty(), "a source page was requested");
    let Some(PastLoad::Ready(data)) = app.past_run.as_ref().map(|past| &past.load) else {
        panic!("not loaded");
    };
    let site = server.url("/");
    let site_pages: Vec<&PageTexts> = data
        .links
        .texts
        .pages
        .iter()
        .filter(|(page, _)| page.starts_with(&site) && !page.ends_with(".css"))
        .map(|(_, texts)| texts)
        .collect();
    assert!(!site_pages.is_empty(), "no source page of the site was settled");
    for texts in site_pages {
        assert!(
            matches!(texts, PageTexts::NotFetched { reason } if reason.contains(HTTP_AUTH)),
            "{texts:?}"
        );
    }
    let missing = app.retry_missing(&run_dir).expect("incomplete");
    assert!(missing.contains(HTTP_AUTH), "{missing}");
}

#[test]
fn a_run_from_history_retries_no_crawled_url_with_setups_password() {
    let env = Env::new();
    let mut app = env.app();
    app.secrets.set(HTTP_AUTH, ALICE);
    let control = runs::finished(&mut app);
    let ctx = egui::Context::default();
    app.logic(&ctx);
    assert!(app.wait_for_background(Duration::from_secs(5)));
    drop(app);

    let mut app = env.app();
    app.secrets.set(HTTP_AUTH, BOB);
    let passes = runs::scripted_passes(&mut app, PassScript::Answer);
    app.open_past_run(control.run_dir.clone());
    assert!(app.wait_for_background(Duration::from_secs(5)));
    let missing = app.retry_missing(&control.run_dir).expect("incomplete");
    assert!(missing.contains(HTTP_AUTH), "{missing}");
    app.retry_now(&[runs::FIXTURE_404.to_string()]);
    let pass = passes.lock().expect("passes").pop().expect("a pass");
    assert!(
        matches!(&pass[0].target, RetryTarget::Refused(reason) if reason.contains(HTTP_AUTH)),
        "{:?}",
        pass[0].url
    );
}

#[test]
fn run_again_from_history_clears_setups_passwords_and_keeps_the_api_key() {
    let env = Env::new();
    let dir = runs::record(
        &env.runs_dir(),
        "20261118-090507-crawler.siteone.io",
        runs::SITE,
        Some(siteone_crawler_gui::model::history::RecordedOutcome::Finished),
        Some(8.3),
        73,
    );
    let mut record = RunRecord::read(&dir).expect("record");
    record.argv.push(format!("{HTTP_AUTH}={}", config::MASK));
    record.write(&dir).expect("written");
    let mut app = env.app();
    app.secrets.set(HTTP_AUTH, BOB);
    app.secrets.set("--upload-password", "other-upload");
    app.secrets.set(AI_API_KEY, "sk-setup-key");
    let mut harness = harness(app, LARGE, egui::Theme::Light);
    settle(&mut harness);
    harness.state_mut().run_again_record(&record);
    settle(&mut harness);
    assert!(
        harness.state().dialog.is_some(),
        "replacing Setup's passwords is confirmed first"
    );
    harness.get_by_label("Replace").click();
    settle(&mut harness);
    let app = harness.state();
    assert_eq!(app.page, Page::Setup);
    assert!(
        app.secrets.get(HTTP_AUTH).is_none(),
        "Setup's password stayed for this run"
    );
    assert!(app.secrets.get("--upload-password").is_none());
    assert_eq!(app.secrets.get(AI_API_KEY), Some("sk-setup-key"));
    let argv = app.session.config.to_argv(&app.schema, &app.secrets);
    assert!(!argv.iter().any(|arg| arg.starts_with(HTTP_AUTH)));
    let notice = app
        .toasts
        .items()
        .last()
        .map(|toast| toast.text.clone())
        .unwrap_or_default();
    assert!(notice.contains(HTTP_AUTH), "{notice}");
}

// ---------------------------------------------------------------------------------------------
// Imports
// ---------------------------------------------------------------------------------------------

#[test]
fn import_of_another_site_does_not_keep_old_authentication() {
    let env = Env::new();
    let mut app = env.app();
    app.session.config.url = "https://a.invalid/".to_string();
    app.secrets.set(HTTP_AUTH, ALICE);
    app.secrets.set("--mail-smtp-pass", "site-a-smtp");
    app.secrets.set("--upload-password", "site-a-upload");
    app.secrets.set(AI_API_KEY, "sk-provider-key");
    let args: Vec<String> = [
        "--url=https://b.invalid/",
        "--mail-to=ops@b.invalid",
        "--upload-to=https://upload.b.invalid/",
    ]
    .map(str::to_string)
    .to_vec();
    let imported = config::import_args(&app.schema, &args);
    ops::apply_import(&mut app, imported, "the command line");
    assert_eq!(app.session.config.url, "https://b.invalid/");
    for option in [HTTP_AUTH, "--mail-smtp-pass", "--upload-password"] {
        assert!(
            app.secrets.get(option).is_none(),
            "{option} of site A survived the import"
        );
    }
    assert_eq!(
        app.secrets.get(AI_API_KEY),
        Some("sk-provider-key"),
        "a provider key stays"
    );
    let argv = app.session.config.to_argv(&app.schema, &app.secrets);
    assert!(
        !argv.iter().any(|arg| arg.contains("site-a")),
        "site A's password reaches the argv"
    );
}

#[test]
fn imported_credentials_are_used_and_saved_profiles_keep_theirs() {
    let env = Env::new();
    let mut app = env.app();
    // A saved profile of site A with its password in the credential store.
    app.session.config.url = "https://a.invalid/".to_string();
    app.secrets.set(HTTP_AUTH, ALICE);
    app.secret_store
        .set("site-a/--http-auth", ALICE)
        .expect("profile password");
    let args: Vec<String> = [
        "--url=https://b.invalid/",
        &format!("{HTTP_AUTH}={BOB}"),
        "--ai-api-key=sk-imported",
    ]
    .map(str::to_string)
    .to_vec();
    let imported = config::import_args(&app.schema, &args);
    ops::apply_import(&mut app, imported, "site-b.conf");
    assert!(
        app.secrets.get(HTTP_AUTH) == Some(BOB),
        "the imported password is not used"
    );
    assert_eq!(app.secrets.get(AI_API_KEY), Some("sk-imported"));
    let argv = app.session.config.to_argv(&app.schema, &app.secrets);
    assert!(
        argv.contains(&format!("{HTTP_AUTH}={BOB}")),
        "the imported password is not passed"
    );
    assert!(
        app.secret_store.get("site-a/--http-auth").expect("store").as_deref() == Some(ALICE),
        "the import changed a saved profile's password"
    );
}
