//! Retry passes (GUI-SPEC §4.9) with the real crawler binary against the local link server: a
//! crawled URL is requested exactly once per retry (no robots.txt, no asset, no followed link or
//! redirect); a URL repaired since the crawl is reported with its new answer although the HTTP
//! cache holds the old one, and the retry's answer is stored for the next crawl; Stop ends a pass
//! at once; a missing crawler is an error on the URL; an external link is checked again once.
//! No network: everything listens on 127.0.0.1.

mod core_support;
mod link_server;

use std::path::Path;
use std::time::{Duration, Instant};

use core_support::{crawler_binary, drive, empty_config_file, events, plan, shared_empty_config_file};
use link_server::LinkServer;
use siteone_crawler_gui::backend::link_check::CheckSettings;
use siteone_crawler_gui::backend::retry::{self, PassSettings, RetryItem, RetryMsg, RetryTarget};
use siteone_crawler_gui::backend::runner::{self, RunRequest};
use siteone_crawler_gui::model::config::CrawlConfig;
use siteone_crawler_gui::model::events::Event;
use siteone_crawler_gui::model::link_check::{ExternalLink, Outcome};
use siteone_crawler_gui::model::retry::{Answer, request_options};
use siteone_crawler_gui::model::run_state::RunState;
use siteone_crawler_gui::model::schema::OptionSchema;

fn settings(run_dir: &Path, binary: Option<&Path>) -> PassSettings {
    PassSettings {
        binary: binary.map(Path::to_path_buf),
        run_dir: run_dir.to_path_buf(),
        config_file: shared_empty_config_file().to_path_buf(),
        watchdog: Duration::from_secs(60),
        host_gap: Duration::from_millis(50),
        check: CheckSettings {
            timeout: Duration::from_secs(10),
            ..CheckSettings::from_config(&CrawlConfig::new("http://127.0.0.1/"), &OptionSchema::load())
        },
    }
}

/// A crawled item with the request options of a run whose argv is `run_argv` (and an empty config
/// file, so that the machine's `~/.siteone-crawler.conf` is not merged).
fn crawled(url: &str, run_argv: &[String], root: &Path) -> RetryItem {
    let mut options = request_options(run_argv, url, url);
    options.push(format!("--config-file={}", empty_config_file(root).display()));
    RetryItem {
        url: url.to_string(),
        target: RetryTarget::Crawled { options },
    }
}

/// Runs a pass to its end; the messages in order.
fn pass(items: Vec<RetryItem>, settings: PassSettings) -> Vec<RetryMsg> {
    let handle = retry::start(items, settings, || {}).expect("started");
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut messages = Vec::new();
    loop {
        match handle.receiver().recv_timeout(Duration::from_secs(1)) {
            Ok(RetryMsg::Finished) => return messages,
            Ok(message) => messages.push(message),
            Err(_) => assert!(Instant::now() < deadline, "the pass did not end: {messages:?}"),
        }
    }
}

fn answer_of<'a>(messages: &'a [RetryMsg], url: &str) -> &'a Answer {
    messages
        .iter()
        .find_map(|message| match message {
            RetryMsg::Answered { url: answered, answer } if answered == url => Some(answer),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no answer for {url}: {messages:?}"))
}

fn status(answer: &Answer) -> i64 {
    answer.code()
}

#[test]
fn a_retry_sends_exactly_one_request_and_a_second_retry_another_one() {
    let server = LinkServer::start();
    let tmp = tempfile::tempdir().expect("temp dir");
    let run_argv = vec![format!(
        "--http-cache-dir={}",
        tmp.path().join("cache").join("http").display()
    )];
    let run_dir = tmp.path().join("run");
    std::fs::create_dir_all(&run_dir).expect("run dir");
    let items = vec![
        // A page with an image, a script, a stylesheet and a link.
        crawled(&server.url("/ok"), &run_argv, tmp.path()),
        // A redirect: the answer, not followed.
        crawled(&server.url("/redirect/1"), &run_argv, tmp.path()),
        crawled(&server.url("/404"), &run_argv, tmp.path()),
        crawled(&server.url("/404"), &run_argv, tmp.path()),
    ];
    let messages = pass(items, settings(&run_dir, Some(crawler_binary())));
    let counts = server.counts();
    assert_eq!(counts.get("/ok"), Some(&1), "{counts:?}");
    assert_eq!(counts.get("/redirect/1"), Some(&1), "{counts:?}");
    assert_eq!(counts.get("/404"), Some(&2), "two retries, two requests: {counts:?}");
    assert_eq!(
        counts.len(),
        3,
        "no robots.txt, asset, link or redirect target: {counts:?}"
    );

    let ok = answer_of(&messages, &server.url("/ok"));
    assert_eq!(status(ok), 200);
    assert_eq!(ok.title(), Some("OK & Alive"), "the title of an OK page");
    assert_eq!(status(answer_of(&messages, &server.url("/redirect/1"))), 301);
    let not_found: Vec<i64> = messages
        .iter()
        .filter_map(|message| match message {
            RetryMsg::Answered { url, answer } if url.ends_with("/404") => Some(status(answer)),
            _ => None,
        })
        .collect();
    assert_eq!(not_found, [404, 404]);
    assert!(
        matches!(&messages[0], RetryMsg::Started(url) if url.ends_with("/ok")),
        "{messages:?}"
    );
    assert!(!run_dir.join(retry::WORK_DIR).exists(), "the child's files are removed");
}

/// Crawls `url` with the run options of `plan` (a fresh run folder, the shared cache of `root`).
fn crawl(root: &Path, url: &str) -> (RunState, Vec<String>) {
    let plan = plan(root, url, &[("--max-reqs-per-sec", "100")]);
    let request = RunRequest::from_plan(crawler_binary().to_path_buf(), &plan);
    let mut handle = runner::start(request, || {}).expect("crawl starts");
    let mut state = RunState::new();
    let messages = drive(&mut handle, &mut state, Duration::from_secs(120), |_, _, _| {});
    assert!(
        events(&messages)
            .iter()
            .any(|event| matches!(event, Event::RunFinished(_))),
        "the crawl finished"
    );
    (state, plan.argv)
}

fn row_status(state: &RunState, suffix: &str) -> (i32, bool) {
    let row = state
        .urls
        .iter()
        .find(|row| row.url.ends_with(suffix))
        .unwrap_or_else(|| panic!("no row {suffix}"));
    (row.status, row.cached)
}

#[test]
fn a_repaired_url_gets_its_new_answer_and_it_is_stored_for_the_next_crawl() {
    let server = LinkServer::start();
    let tmp = tempfile::tempdir().expect("temp dir");
    let site = server.url("/site");
    let (first, run_argv) = crawl(tmp.path(), &site);
    assert_eq!(row_status(&first, "/flaky-page"), (404, false));
    assert_eq!(row_status(&first, "/flaky.png"), (404, false));
    // A stored 404 is reused by the next crawl (the crawler never reuses a 429/5xx or a failure).
    server.fix();
    let before = server.counts();

    let run_dir = tmp.path().join("retry-run");
    std::fs::create_dir_all(&run_dir).expect("run dir");
    let items = ["/flaky-page", "/flaky.png"]
        .iter()
        .map(|path| {
            let url = server.url(path);
            let mut options = request_options(&run_argv, &site, &url);
            options.push(format!("--config-file={}", empty_config_file(tmp.path()).display()));
            RetryItem {
                url,
                target: RetryTarget::Crawled { options },
            }
        })
        .collect();
    let messages = pass(items, settings(&run_dir, Some(crawler_binary())));
    let page = answer_of(&messages, &server.url("/flaky-page"));
    assert_eq!((status(page), page.title()), (200, Some("Back again")));
    let image = answer_of(&messages, &server.url("/flaky.png"));
    assert_eq!(status(image), 200);
    assert!(matches!(image, Answer::Crawled(answer) if !answer.cached));
    let after_retry = server.counts();
    for path in ["/flaky-page", "/flaky.png"] {
        assert_eq!(
            after_retry.get(path).copied(),
            before.get(path).map(|n| n + 1),
            "{path}"
        );
    }

    // The next crawl with the same settings: the image's retried 200 comes from the cache without a
    // request. The page was found as a link: depending on timing the crawler requests it with or
    // without an `Origin` header, which is part of the cache key, in either crawl. So it gets the
    // retried 200 from the cache, requests the page again, or reuses its own stored 404 (GUI-SPEC
    // §4.9) — never anything else.
    let (second, _) = crawl(tmp.path(), &site);
    assert_eq!(row_status(&second, "/flaky.png"), (200, true));
    let after_crawl = server.counts();
    assert_eq!(after_crawl.get("/flaky.png"), after_retry.get("/flaky.png"));
    let page = row_status(&second, "/flaky-page");
    let requested = after_crawl.get("/flaky-page") != after_retry.get("/flaky-page");
    assert!(
        matches!(
            (page, requested),
            ((200, true), false) | ((200, false), true) | ((404, true), false)
        ),
        "{page:?}, requested again: {requested}"
    );
}

#[test]
fn stop_ends_the_pass_at_once_and_requests_nothing_more() {
    let server = LinkServer::start();
    let tmp = tempfile::tempdir().expect("temp dir");
    let run_dir = tmp.path().join("run");
    std::fs::create_dir_all(&run_dir).expect("run dir");
    let items = vec![
        crawled(&server.url("/slow"), &[], tmp.path()),
        crawled(&server.url("/ok"), &[], tmp.path()),
        crawled(&server.url("/404"), &[], tmp.path()),
    ];
    let handle = retry::start(items, settings(&run_dir, Some(crawler_binary())), || {}).expect("started");
    let first = handle
        .receiver()
        .recv_timeout(Duration::from_secs(30))
        .expect("a message");
    assert!(
        matches!(&first, RetryMsg::Started(url) if url.ends_with("/slow")),
        "{first:?}"
    );
    // Wait until the child's request has reached the server.
    let deadline = Instant::now() + Duration::from_secs(30);
    while server.count("/slow") == 0 {
        assert!(Instant::now() < deadline, "the child did not request /slow");
        std::thread::sleep(Duration::from_millis(20));
    }
    let stopped = Instant::now();
    handle.cancel();
    // The worker ends (its sender is dropped) without another message.
    match handle.receiver().recv_timeout(Duration::from_secs(10)) {
        Ok(message) => panic!("a message after Stop: {message:?}"),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("the pass did not end"),
    }
    assert!(
        stopped.elapsed() < Duration::from_secs(3),
        "Stop took {:?}",
        stopped.elapsed()
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!((server.count("/ok"), server.count("/404")), (0, 0));
}

#[test]
fn a_missing_crawler_is_an_error_on_the_url_and_external_links_are_checked_again() {
    let server = LinkServer::start();
    let tmp = tempfile::tempdir().expect("temp dir");
    let run_dir = tmp.path().join("run");
    std::fs::create_dir_all(&run_dir).expect("run dir");
    let missing = tmp.path().join("no-such-crawler.exe");
    let external = ExternalLink {
        url: server.url_on("localhost", "/404"),
        sources: vec!["https://site.example/".to_string()],
        count: 1,
    };
    let items = vec![
        crawled(&server.url("/ok"), &[], tmp.path()),
        RetryItem {
            url: external.url.clone(),
            target: RetryTarget::External(external.clone()),
        },
        RetryItem {
            url: server.url("/403"),
            target: RetryTarget::Refused("The run's HTTP headers are not stored.".to_string()),
        },
    ];
    let messages = pass(items, settings(&run_dir, Some(&missing)));
    assert!(
        messages.iter().any(|message| matches!(message,
            RetryMsg::Failed { url, reason } if url.ends_with("/ok") && reason.starts_with("The crawler could not be started"))),
        "{messages:?}"
    );
    let checked = answer_of(&messages, &external.url);
    assert!(matches!(checked, Answer::External(link) if link.outcome == Outcome::Broken && link.status == Some(404)));
    assert!(messages.iter().any(|message| matches!(message,
        RetryMsg::Failed { reason, .. } if reason == "The run's HTTP headers are not stored.")));
    let counts = server.counts();
    assert_eq!(counts.get("/404"), Some(&1), "{counts:?}");
    assert_eq!(
        counts.len(),
        1,
        "nothing for the crawled or the refused URL: {counts:?}"
    );

    // No crawler at all.
    let messages = pass(
        vec![crawled(&server.url("/ok"), &[], tmp.path())],
        settings(&run_dir, None),
    );
    assert!(
        matches!(&messages[..], [RetryMsg::Started(_), RetryMsg::Failed { reason, .. }]
        if reason.contains("was not found")),
        "{messages:?}"
    );
}
