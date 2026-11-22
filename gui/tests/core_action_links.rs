//! *Skip cart and action links* with the real crawler (GUI-SPEC §5.2): a shop page on a local
//! server that logs every request links to its products and to WooCommerce and WordPress action
//! links (`?add-to-cart=`, `?remove_item=`, `?undo_item=`, `wp-login.php?action=logout`). With the
//! switch on (the default) the crawler requests the product and none of the action links, and
//! reports them nowhere — the link report's *Not checked* names them instead; with it off it
//! requests them all. No network: everything listens on 127.0.0.1.

mod core_support;
mod link_server;

use std::time::Duration;

use core_support::{crawler_binary, drive, plan_of};
use link_server::LinkServer;
use siteone_crawler_gui::backend::runner::{self, RunRequest};
use siteone_crawler_gui::model::config::{ACTION_LINK_PATTERNS, CrawlConfig, Value};
use siteone_crawler_gui::model::coverage::{CrawlScope, Gap, GapInput, gaps};
use siteone_crawler_gui::model::results::{ResultLoad, load_result};
use siteone_crawler_gui::model::run_state::{RunOutcome, RunState};
use siteone_crawler_gui::model::schema::OptionSchema;

/// The action links of the `/shop` page, as the server sees them requested.
const ACTION_LINKS: &[&str] = &[
    "/product/red/?add-to-cart=31",
    "/shop?orderby=price&add-to-cart=32",
    "/cart/?remove_item=0c74b7f7&_wpnonce=1a2b",
    "/cart/?undo_item=0c74b7f7&_wpnonce=1a2b",
    "/wp-login.php?action=logout&_wpnonce=1a2b",
];

/// Crawls `/shop` of `server` with `skip` and returns the run's state and result.
fn crawl(server: &LinkServer, root: &std::path::Path, skip: bool) -> (RunState, ResultLoad, Vec<String>) {
    let schema = OptionSchema::load();
    let mut config = CrawlConfig::new(server.url("/shop"));
    config.skip_action_links = skip;
    config
        .set(&schema, "--max-reqs-per-sec", Value::Text("100".to_string()))
        .expect("set");
    let plan = plan_of(root, &config);
    let mut handle =
        runner::start(RunRequest::from_plan(crawler_binary().to_path_buf(), &plan), || {}).expect("the crawler starts");
    let mut state = RunState::new();
    drive(&mut handle, &mut state, Duration::from_secs(120), |_, _, _| {});
    assert_eq!(state.outcome(), RunOutcome::Finished, "{}", state.console.plain_text());
    (state, load_result(&plan.result_json), plan.argv)
}

#[test]
fn cart_and_action_links_are_not_requested_and_named_as_not_checked() {
    let server = LinkServer::start();
    let tmp = tempfile::tempdir().expect("temp dir");

    // On (the default): the argv carries the patterns, the crawler requests no action link.
    let (state, result, argv) = crawl(&server, &tmp.path().join("on"), true);
    let passed: Vec<&str> = argv
        .iter()
        .filter_map(|arg| arg.strip_prefix("--ignore-regex:="))
        .collect();
    assert_eq!(passed, ACTION_LINK_PATTERNS);
    let requested: Vec<String> = server.requests().into_iter().map(|request| request.path).collect();
    assert!(
        requested.iter().any(|path| path == "/product/red/"),
        "the product page is crawled: {requested:?}"
    );
    // A crawler trap that changes nothing is not an action link: it is crawled.
    assert!(
        requested.iter().any(|path| path == "/blog/?replytocom=5"),
        "{requested:?}"
    );
    for action in ACTION_LINKS {
        assert!(
            !requested.iter().any(|path| path == action),
            "{action} was requested: {requested:?}"
        );
    }
    assert!(
        state
            .urls
            .iter()
            .all(|row| !row.url.contains("add-to-cart") && !row.url.contains("action=logout")),
        "{:?}",
        state.urls
    );
    // The crawler reports them nowhere (not in `skipped`): the app names them, without a count.
    let ResultLoad::Loaded(data) = result else {
        panic!("no result: {result:?}");
    };
    let skipped = data.table("skipped").map_or(0, |table| table.rows.len());
    assert_eq!(skipped, 0, "{:?}", data.table("skipped"));
    let scope = CrawlScope::from_result(&data);
    assert!(scope.skips_action_links());
    assert!(scope.own_skip_patterns().is_empty());
    let found = gaps(&GapInput {
        scope: Some(&scope),
        outcome: &state.outcome(),
        crawled: state.urls.len() as u64,
        external_unchecked: 0,
        external_over_cap: 0,
        external_files: 0,
    });
    assert_eq!(found, [Gap::ActionLinks]);

    // Off: the same page makes the crawler request every action link.
    let before = server.requests().len();
    let (_, _, argv) = crawl(&server, &tmp.path().join("off"), false);
    assert!(!argv.iter().any(|arg| arg.starts_with("--ignore-regex")), "{argv:?}");
    let requested: Vec<String> = server
        .requests()
        .into_iter()
        .skip(before)
        .map(|request| request.path)
        .collect();
    for action in ACTION_LINKS {
        assert!(
            requested.iter().any(|path| path == action),
            "{action} not requested: {requested:?}"
        );
    }
}
