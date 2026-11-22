//! Runs of the real crawler against a generated site served by its own `--serve-offline`
//! (GUI-SPEC §8): a full run, stop, kill, drop, and a configuration error.

mod core_support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use core_support::{
    counting_waker, crawler_binary, drive, events, plan, process_exists, serve, wait_until_gone, write_site,
};
use siteone_crawler_gui::backend::runner::{self, RunMsg, RunRequest};
use siteone_crawler_gui::model::events::Event;
use siteone_crawler_gui::model::results::{ResultLoad, load_result};
use siteone_crawler_gui::model::run_state::{RunOutcome, RunState};

const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// A site and a crawl that takes long enough to be stopped or killed after its first URL:
/// one worker, two requests per second, 40 pages.
const SLOW: &[(&str, &str)] = &[("--workers", "1"), ("--max-reqs-per-sec", "2")];
const SLOW_PAGES: usize = 40;

#[test]
fn a_full_run_reports_events_urls_artifacts_and_results() {
    let tmp = tempfile::tempdir().expect("temp dir");
    write_site(&tmp.path().join("site"), 5);
    let server = serve(&tmp.path().join("site"), &tmp.path().join("server"));
    let plan = plan(tmp.path(), &server.url(), &[("--max-reqs-per-sec", "100")]);

    let (wakes, waker) = counting_waker();
    let mut handle =
        runner::start(RunRequest::from_plan(crawler_binary().to_path_buf(), &plan), waker).expect("the crawler starts");
    let mut state = RunState::new();
    let messages = drive(&mut handle, &mut state, RUN_TIMEOUT, |_, _, _| {});

    // `Exited` is the last message: nothing follows it.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(handle.try_iter().count(), 0, "messages after Exited");
    assert!(!handle.is_running());

    let events = events(&messages);
    assert!(matches!(events.first(), Some(Event::RunStarted(_))), "{events:?}");
    match events.last() {
        Some(Event::RunFinished(finished)) => {
            assert_eq!((finished.outcome.as_str(), finished.exit_code), ("success", 0))
        }
        other => panic!("last event: {other:?}"),
    }
    assert_eq!(messages.last(), Some(&RunMsg::Exited { code: Some(0) }));
    assert_eq!(
        state.outcome(),
        RunOutcome::Finished,
        "console: {}",
        state.console.plain_text()
    );
    assert_eq!(state.malformed_events, 0);
    assert!(
        !messages.iter().any(|msg| matches!(msg, RunMsg::WorkerError { .. })),
        "{messages:?}"
    );

    // index, 5 pages, the stylesheet; all served.
    let urls: Vec<&str> = state.urls.iter().map(|row| &*row.url).collect();
    assert!(state.urls.len() >= 7, "{urls:?}");
    for page in ["/page-1.html", "/page-5.html", "/style.css"] {
        assert!(urls.iter().any(|url| url.ends_with(page)), "{page} not in {urls:?}");
    }
    assert!(
        state
            .urls
            .iter()
            .all(|row| row.status == 200 || row.url.ends_with("robots.txt")),
        "{:?}",
        state.urls
    );
    assert_eq!(
        state.started.as_ref().map(|started| started.url.as_str()),
        Some(server.url().as_str())
    );

    for kind in ["json", "html"] {
        let artifact = state
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == kind)
            .unwrap_or_else(|| panic!("no {kind} artifact in {:?}", state.artifacts));
        let path = artifact.path.as_ref().expect("a file");
        assert!(path.is_file(), "{} does not exist", path.display());
    }
    match load_result(&plan.result_json) {
        ResultLoad::Loaded(data) => {
            let scores = data.scores.as_ref().expect("quality scores");
            assert!((0.0..=10.0).contains(&scores.overall.score), "{scores:?}");
            assert!(!data.tables.is_empty());
        }
        other => panic!("result JSON: {other:?}"),
    }

    // The console arrived parsed (no escape sequences left) and was logged raw.
    assert!(!state.console.is_empty());
    assert!(!state.console.plain_text().contains('\u{1b}'));
    let log = std::fs::read(&plan.console_log).expect("console.log");
    assert!(log.contains(&0x1b), "console.log keeps the raw colour codes");
    assert!(wakes.load(Ordering::SeqCst) >= 1);
}

#[test]
fn stop_winds_the_crawl_down_with_partial_results() {
    let tmp = tempfile::tempdir().expect("temp dir");
    write_site(&tmp.path().join("site"), SLOW_PAGES);
    let server = serve(&tmp.path().join("site"), &tmp.path().join("server"));
    let plan = plan(tmp.path(), &server.url(), SLOW);

    let mut handle =
        runner::start(RunRequest::from_plan(crawler_binary().to_path_buf(), &plan), || {}).expect("the crawler starts");
    let mut state = RunState::new();
    let mut stopped = false;
    let messages = drive(&mut handle, &mut state, RUN_TIMEOUT, |msg, handle, state| {
        if !stopped && matches!(msg, RunMsg::Event(Event::Url(_))) {
            handle.stop().expect("stop");
            state.stop_requested = true;
            stopped = true;
        }
    });
    assert!(stopped, "no URL event before the end: {messages:?}");

    match events(&messages).last() {
        Some(Event::RunFinished(finished)) => {
            assert_eq!(finished.outcome, "cancelled");
            assert!(finished.interrupted);
        }
        other => panic!("last event: {other:?}"),
    }
    assert!(matches!(messages.last(), Some(RunMsg::Exited { .. })));
    assert_eq!(state.outcome(), RunOutcome::Stopped);
    assert!(!state.urls.is_empty());
    assert!(
        state.urls.len() < SLOW_PAGES,
        "the crawl was not cut short: {} URLs",
        state.urls.len()
    );
    // What was crawled is still analysed and reported.
    assert!(matches!(load_result(&plan.result_json), ResultLoad::Loaded(_)));
    // Stopping a finished run is a no-op.
    handle.stop().expect("stop after the end");
}

#[test]
fn kill_aborts_the_run_and_leaves_no_process() {
    let tmp = tempfile::tempdir().expect("temp dir");
    write_site(&tmp.path().join("site"), SLOW_PAGES);
    let server = serve(&tmp.path().join("site"), &tmp.path().join("server"));
    let plan = plan(tmp.path(), &server.url(), SLOW);

    let mut handle =
        runner::start(RunRequest::from_plan(crawler_binary().to_path_buf(), &plan), || {}).expect("the crawler starts");
    let pid = handle.id();
    let mut state = RunState::new();
    let mut killed = false;
    let messages = drive(&mut handle, &mut state, RUN_TIMEOUT, |msg, handle, state| {
        if !killed && matches!(msg, RunMsg::Event(Event::Url(_))) {
            handle.kill().expect("kill");
            state.killed = true;
            killed = true;
        }
    });
    assert!(killed, "no URL event before the end: {messages:?}");

    assert!(
        !events(&messages)
            .iter()
            .any(|event| matches!(event, Event::RunFinished(_))),
        "a killed crawler writes no runFinished"
    );
    assert!(matches!(messages.last(), Some(RunMsg::Exited { .. })));
    assert_eq!(state.outcome(), RunOutcome::Aborted);
    assert!(!handle.is_running());
    assert!(!process_exists(pid), "the crawler {pid} still runs");
    handle.kill().expect("kill after the end");
}

#[test]
fn dropping_the_handle_kills_the_crawler() {
    let tmp = tempfile::tempdir().expect("temp dir");
    write_site(&tmp.path().join("site"), SLOW_PAGES);
    let server = serve(&tmp.path().join("site"), &tmp.path().join("server"));
    let plan = plan(tmp.path(), &server.url(), SLOW);

    let handle =
        runner::start(RunRequest::from_plan(crawler_binary().to_path_buf(), &plan), || {}).expect("the crawler starts");
    let pid = handle.id();
    let first_url = std::time::Instant::now() + RUN_TIMEOUT;
    loop {
        let msg = handle
            .receiver()
            .recv_timeout(first_url.saturating_duration_since(std::time::Instant::now()))
            .expect("a URL event");
        match msg {
            RunMsg::Event(Event::Url(_)) => break,
            RunMsg::Exited { .. } => panic!("the crawl ended before its first URL"),
            _ => {}
        }
    }
    assert!(process_exists(pid));
    drop(handle);
    assert!(
        wait_until_gone(pid, Duration::from_secs(10)),
        "the crawler {pid} outlived its handle"
    );
    drop(server);
}

#[test]
fn a_configuration_error_exits_101_with_the_cli_message_and_no_events() {
    let tmp = tempfile::tempdir().expect("temp dir");
    // No server: the run never gets that far.
    let mut plan = plan(tmp.path(), "http://127.0.0.1:9/", &[]);
    plan.argv.push("--workers=many".to_string());

    let mut handle =
        runner::start(RunRequest::from_plan(crawler_binary().to_path_buf(), &plan), || {}).expect("the crawler starts");
    let mut state = RunState::new();
    let messages = drive(&mut handle, &mut state, RUN_TIMEOUT, |_, _, _| {});

    assert!(events(&messages).is_empty(), "{messages:?}");
    assert_eq!(messages.last(), Some(&RunMsg::Exited { code: Some(101) }));
    let message = state.console_error().expect("the CLI's ERROR: message");
    assert!(message.contains("--workers"), "{message}");
    assert_eq!(state.outcome(), RunOutcome::ConfigError { message: Some(message) });
    assert!(!state.outcome().is_recorded());
    let events_file = std::fs::read(&plan.events_file).unwrap_or_default();
    assert!(events_file.is_empty(), "no events are written");
}
