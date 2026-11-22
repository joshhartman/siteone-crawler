//! The credential boundary of source-page requests (link texts, GUI-SPEC §4.8) across `https` and
//! `http`, end to end against a local server that speaks both on one port (`tls_server`, with a
//! self-signed certificate the fetcher accepts through `--accept-invalid-certs`): `--http-auth` and
//! the `--header` values go only where the crawler would send them
//! (`ParsedUrl::may_send_credentials`: never from an https start to http, for an IP address only to
//! the start port), on the first request and on every redirect hop. Because one port serves both
//! schemes, an https page that redirects to http on the same host *and port* is tested too. No
//! network: everything listens on 127.0.0.1 / ::1. The assertions never print a credential.

mod tls_server;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use siteone_crawler_gui::backend::link_text::{self, SourceSettings, TextMsg};
use siteone_crawler_gui::model::config::{CrawlConfig, Value};
use siteone_crawler_gui::model::link_text::PageTexts;
use siteone_crawler_gui::model::retry::may_send_credentials;
use siteone_crawler_gui::model::schema::OptionSchema;
use tls_server::{DualServer, Request};

/// The settings of a run that started at `site` with a password, a custom token header and
/// `--accept-invalid-certs` (the test certificate is self-signed).
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
    config
        .set(&schema, "--accept-invalid-certs", Value::Bool(true))
        .expect("accept invalid certs");
    let mut settings = SourceSettings::from_config(&config, &schema, site, Some("owner:dummy-pass".to_string()));
    assert!(settings.accept_invalid_certs, "the option reaches the fetcher");
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

/// The log without credentials: scheme, host, path, what was carried.
fn log(server: &DualServer) -> Vec<(bool, String, String, (bool, bool))> {
    server
        .requests()
        .iter()
        .map(|r| (r.tls, r.host(), r.path.clone(), carried(r)))
        .collect()
}

fn fetched(found: &HashMap<String, PageTexts>, page: &str) -> bool {
    matches!(found.get(page), Some(PageTexts::Fetched { .. }))
}

/// `/redirect-to?<target>` on `server`.
fn redirect(server: &DualServer, scheme: &str, target: &str) -> String {
    server.url(scheme, "127.0.0.1", &format!("/redirect-to?{target}"))
}

#[test]
fn an_https_page_of_the_start_origin_gets_both() {
    let server = DualServer::start();
    let site = server.url("https", "127.0.0.1", "/");
    let page = server.url("https", "127.0.0.1", "/page");
    let found = fetch(settings(&site), std::slice::from_ref(&page));
    assert!(fetched(&found, &page), "{:?}", found.get(&page));
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "{:?}", log(&server));
    assert!(requests[0].tls, "the page was fetched over TLS");
    assert_eq!(carried(&requests[0]), (true, true));
    assert_eq!(
        requests[0].header("authorization"),
        Some("Basic b3duZXI6ZHVtbXktcGFzcw==")
    );
    assert_eq!(requests[0].header("x-site-token"), Some("dummy-token"));
}

#[test]
fn an_https_page_that_redirects_to_http_sends_neither_on_the_http_hop() {
    let site_server = DualServer::start();
    let other = DualServer::start();
    let site = site_server.url("https", "127.0.0.1", "/");
    // To plain http on another port, and on the same port (the server speaks both).
    let to_other_port = other.url("http", "127.0.0.1", "/page");
    let to_same_port = site_server.url("http", "127.0.0.1", "/page");
    for target in [&to_other_port, &to_same_port] {
        assert!(!may_send_credentials(&site, target), "{target}");
    }
    let pages = [
        redirect(&site_server, "https", &to_other_port),
        redirect(&site_server, "https", &to_same_port),
    ];
    let found = fetch(settings(&site), &pages);
    for page in &pages {
        assert!(fetched(&found, page), "{page}: {:?}", found.get(page));
    }

    // The https requests of the start origin carry both; the http hop on the same port neither.
    let site_log = log(&site_server);
    assert_eq!(site_log.len(), 3, "{site_log:?}");
    for (tls, host, path, carried) in &site_log {
        assert_eq!(host, "127.0.0.1");
        if path.starts_with("/redirect-to") {
            assert!(*tls, "{path}");
            assert_eq!(*carried, (true, true), "{path}");
        } else {
            assert_eq!(path, "/page");
            assert!(!*tls, "the downgrade went to plain http");
            assert_eq!(
                *carried,
                (false, false),
                "credentials crossed an https-to-http redirect on the same host and port"
            );
        }
    }
    let other_log = log(&other);
    assert_eq!(other_log.len(), 1, "{other_log:?}");
    assert!(!other_log[0].0, "plain http");
    assert_eq!(
        other_log[0].3,
        (false, false),
        "credentials crossed an https-to-http redirect to another port"
    );
}

#[test]
fn an_http_page_that_redirects_to_https_follows_the_clis_rule() {
    // An http start: the CLI's rule refuses no scheme change upwards, but for an IP address only
    // the start port gets credentials — so the same port over https gets them, another port not.
    let site_server = DualServer::start();
    let other = DualServer::start();
    let site = site_server.url("http", "127.0.0.1", "/");
    let to_same_port = site_server.url("https", "127.0.0.1", "/page");
    let to_other_port = other.url("https", "127.0.0.1", "/page");
    assert!(may_send_credentials(&site, &to_same_port));
    assert!(!may_send_credentials(&site, &to_other_port));
    let pages = [
        redirect(&site_server, "http", &to_same_port),
        redirect(&site_server, "http", &to_other_port),
    ];
    let found = fetch(settings(&site), &pages);
    for page in &pages {
        assert!(fetched(&found, page), "{page}: {:?}", found.get(page));
    }

    let site_log = log(&site_server);
    assert_eq!(site_log.len(), 3, "{site_log:?}");
    for (tls, host, path, carried) in &site_log {
        assert_eq!(host, "127.0.0.1");
        assert_eq!(*tls, path == "/page", "{path}");
        assert_eq!(*carried, (true, true), "{path} (tls: {tls})");
    }
    let other_log = log(&other);
    assert_eq!(other_log.len(), 1, "{other_log:?}");
    assert!(other_log[0].0, "https");
    assert_eq!(other_log[0].3, (false, false), "another port of an IP address");
}

#[test]
fn a_redirect_to_another_host_is_not_followed() {
    let server = DualServer::start();
    let site = server.url("https", "127.0.0.1", "/");
    let pages = [
        redirect(&server, "https", &server.url("https", "localhost", "/page")),
        redirect(&server, "https", &server.url("http", "localhost", "/page")),
    ];
    let found = fetch(settings(&site), &pages);
    for page in &pages {
        assert!(
            matches!(found.get(page), Some(PageTexts::NotFetched { reason }) if reason.contains("outside")),
            "{page}: {:?}",
            found.get(page)
        );
    }
    let requests = log(&server);
    assert_eq!(requests.len(), 2, "only the two redirects: {requests:?}");
    for (tls, host, path, carried) in &requests {
        assert!(
            *tls && host == "127.0.0.1" && path.starts_with("/redirect-to"),
            "{path}"
        );
        assert_eq!(*carried, (true, true));
    }
}
