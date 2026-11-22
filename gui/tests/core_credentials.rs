//! The credential boundary of source-page requests (link texts, GUI-SPEC §4.8) against local HTTP
//! servers (`link_server`): `--http-auth` and the `--header` values go only where the crawler would
//! send them (`ParsedUrl::may_send_credentials`: not from https to http, for an IP address or
//! `localhost` only to the start port), on the first request and on every redirect. A page of the
//! site outside the boundary is fetched without them; a page of another host is not fetched. No
//! network: everything listens on 127.0.0.1 / ::1. The assertions never print a credential.

mod link_server;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use link_server::{LinkServer, Request};
use siteone_crawler_gui::backend::link_text::{self, SourceSettings, TextMsg};
use siteone_crawler_gui::model::config::{CrawlConfig, Value};
use siteone_crawler_gui::model::link_text::PageTexts;
use siteone_crawler_gui::model::retry::may_send_credentials;
use siteone_crawler_gui::model::schema::OptionSchema;

/// The settings of a run that started at `site` with a password and a custom token header.
fn settings(site: &str) -> SourceSettings {
    let schema = OptionSchema::load();
    let mut config = CrawlConfig::new(site);
    config
        .set(
            &schema,
            "--header",
            Value::List(vec!["X-Site-Token: dummy-token".to_string()]),
        )
        .expect("header");
    let mut settings = SourceSettings::from_config(&config, &schema, site, Some("owner:dummy-pass".to_string()));
    settings.gap = Duration::ZERO;
    settings
}

/// Fetches `pages` to the end; what each page gave.
fn fetch(settings: SourceSettings, pages: &[String]) -> HashMap<String, PageTexts> {
    let handle = link_text::start(pages.to_vec(), Vec::new(), settings, || {}).expect("started");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut found = HashMap::new();
    loop {
        match handle.try_next() {
            Some(TextMsg::Page(page, texts)) => {
                found.insert(page, texts);
            }
            Some(TextMsg::Finished) => return found,
            None => {
                assert!(Instant::now() < deadline, "the fetch did not end");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// Whether a request carried the password, the token header — each, without showing either.
fn carried(request: &Request) -> (bool, bool) {
    (
        request.header("authorization").is_some(),
        request.header("x-site-token").is_some(),
    )
}

fn paths(server: &LinkServer) -> Vec<(String, String)> {
    server.requests().iter().map(|r| (r.path.clone(), r.host())).collect()
}

fn fetched(found: &HashMap<String, PageTexts>, page: &str) -> bool {
    matches!(found.get(page), Some(PageTexts::Fetched { .. }))
}

#[test]
fn source_pages_preserve_the_cli_credential_boundary() {
    // The reviewer's probe: an https start on another port; the source page is plain http.
    let server = LinkServer::start();
    let site = "https://127.0.0.1:8443/";
    let target = server.url("/ok");
    assert!(!may_send_credentials(site, &target));
    let found = fetch(settings(site), std::slice::from_ref(&target));
    assert!(fetched(&found, &target), "a page of the site by host is still read");
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "{:?}", paths(&server));
    assert_eq!(
        carried(&requests[0]),
        (false, false),
        "credentials crossed an https-to-http and port boundary the CLI refuses"
    );
}

#[test]
fn a_downgrade_from_https_to_http_sends_neither_password_nor_headers() {
    // Same host and port, only the scheme differs.
    let server = LinkServer::start();
    let site = format!("https://127.0.0.1:{}/", server.port);
    let page = server.url("/src/b");
    let found = fetch(settings(&site), std::slice::from_ref(&page));
    assert!(fetched(&found, &page));
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "{:?}", paths(&server));
    assert_eq!(carried(&requests[0]), (false, false));
}

#[test]
fn another_port_of_an_ip_address_or_localhost_gets_no_credentials() {
    let site_server = LinkServer::start();
    let other = LinkServer::start();
    for host in ["127.0.0.1", "localhost"] {
        let site = site_server.url_on(host, "/");
        let page = other.url_on(host, &format!("/src/b?{host}"));
        let found = fetch(settings(&site), std::slice::from_ref(&page));
        assert!(fetched(&found, &page), "{host}: {:?}", found.get(&page));
    }
    let requests = other.requests();
    assert_eq!(requests.len(), 2, "{:?}", paths(&other));
    for request in &requests {
        assert_eq!(carried(request), (false, false), "{} {}", request.path, request.host());
    }
    assert!(site_server.requests().is_empty(), "{:?}", paths(&site_server));
}

#[test]
fn another_host_is_not_requested_and_the_site_itself_gets_its_credentials() {
    let server = LinkServer::start();
    let site = server.url("/");
    let same = server.url("/src/b");
    let other_host = server.url_on("localhost", "/src/b");
    let found = fetch(settings(&site), &[same.clone(), other_host.clone()]);
    assert!(fetched(&found, &same));
    assert!(matches!(found.get(&other_host), Some(PageTexts::NotFetched { .. })));
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "{:?}", paths(&server));
    assert_eq!(requests[0].host(), "127.0.0.1");
    assert_eq!(carried(&requests[0]), (true, true), "the site's own page gets both");
    assert_eq!(
        requests[0].header("authorization"),
        Some("Basic b3duZXI6ZHVtbXktcGFzcw==")
    );
    assert_eq!(requests[0].header("x-site-token"), Some("dummy-token"));
}

#[test]
fn every_redirect_is_judged_on_its_own_url() {
    let site_server = LinkServer::start();
    let other = LinkServer::start();
    let site = site_server.url("/");
    // To another port of the IP: followed (a page of the site), without credentials.
    let to_port = site_server.url(&format!("/redirect-to?{}", other.url("/src/b")));
    // To another host: not followed, nothing requested there.
    let to_host = site_server.url(&format!("/redirect-to?{}", site_server.url_on("localhost", "/src/b")));
    // Within the start origin: credentials on both requests.
    let same = site_server.url(&format!("/redirect-to?{}", site_server.url("/src/a")));
    let found = fetch(settings(&site), &[to_port.clone(), to_host.clone(), same.clone()]);
    assert!(fetched(&found, &to_port), "{:?}", found.get(&to_port));
    assert!(
        matches!(found.get(&to_host), Some(PageTexts::NotFetched { reason }) if reason.contains("outside")),
        "{:?}",
        found.get(&to_host)
    );
    assert!(fetched(&found, &same), "{:?}", found.get(&same));

    let site_requests = site_server.requests();
    assert_eq!(
        site_requests.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        [
            to_port.trim_start_matches(&site_server.url("")),
            to_host.trim_start_matches(&site_server.url("")),
            same.trim_start_matches(&site_server.url("")),
            "/src/a",
        ],
        "nothing went to localhost: {:?}",
        paths(&site_server)
    );
    for request in &site_requests {
        assert_eq!(request.host(), "127.0.0.1");
        assert_eq!(carried(request), (true, true), "{}", request.path);
    }
    let other_requests = other.requests();
    assert_eq!(other_requests.len(), 1, "{:?}", paths(&other));
    assert_eq!(
        carried(&other_requests[0]),
        (false, false),
        "neither the password nor the custom header followed the redirect to another port"
    );
}
