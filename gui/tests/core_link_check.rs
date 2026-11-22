//! The external link check and the link texts against a local HTTP server (`link_server`): one
//! request per checked link and nothing the page references, the classification of every kind of
//! answer, limits, politeness, cancelling — and the source pages of link texts (HTML pages and
//! stylesheets) fetched once, with the site's credentials sent only to the site, and the position
//! of each reference read from them. No network: everything listens on 127.0.0.1 / ::1.

mod link_server;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use link_server::{BIG_BODY, LinkServer, minified_stylesheet};
use siteone_crawler_gui::backend::link_check::{self, CheckMsg, CheckSettings};
use siteone_crawler_gui::backend::link_text::{self, SourceSettings, TextMsg};
use siteone_crawler_gui::model::config::{CrawlConfig, Value};
use siteone_crawler_gui::model::link_check::{ExternalLink, Failure, LinkResult, Outcome};
use siteone_crawler_gui::model::link_text::{PageTexts, url_key};
use siteone_crawler_gui::model::schema::OptionSchema;

fn link(url: String) -> ExternalLink {
    ExternalLink {
        url,
        sources: vec!["https://site.example/".to_string()],
        count: 1,
    }
}

/// The settings of a run with credentials and headers set, with short test timings.
fn settings(timeout: Duration, gap: Duration) -> CheckSettings {
    let schema = OptionSchema::load();
    let mut config = CrawlConfig::new("https://site.example/");
    config
        .set(
            &schema,
            "--header",
            Value::List(vec![
                "Cookie: session=site-secret".to_string(),
                "X-Site-Token: t0ken".to_string(),
            ]),
        )
        .expect("header");
    config
        .set(&schema, "--user-agent", Value::Text("LinkTester/1.0 !".to_string()))
        .expect("user agent");
    CheckSettings {
        timeout,
        host_gap: gap,
        ..CheckSettings::from_config(&config, &schema)
    }
}

/// Runs a check to its end and returns the results by URL.
fn run(links: Vec<ExternalLink>, settings: CheckSettings) -> HashMap<String, LinkResult> {
    let handle = link_check::start(links, settings, || {}).expect("started");
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut results = HashMap::new();
    loop {
        match handle.receiver().recv_timeout(Duration::from_secs(1)) {
            Ok(CheckMsg::Checked(result)) => {
                results.insert(result.url.clone(), result);
            }
            Ok(CheckMsg::Failed(error)) => panic!("the check failed: {error}"),
            Ok(CheckMsg::Finished) => return results,
            Err(_) => assert!(Instant::now() < deadline, "the check did not end"),
        }
    }
}

/// A port on `localhost` where nothing listens.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("address").port()
}

#[test]
fn a_host_that_does_not_answer_is_asked_twice_and_its_other_links_fail_without_a_request() {
    let server = LinkServer::start();
    let dead = closed_port();
    let dead_url = |path: &str| format!("http://localhost:{dead}{path}");
    let mut links: Vec<ExternalLink> = ["/a", "/b", "/c", "/d", "/e"]
        .iter()
        .map(|path| link(dead_url(path)))
        .collect();
    links.push(link(server.url("/ok")));
    let results = run(links, settings(Duration::from_secs(5), Duration::from_millis(20)));
    assert_eq!(results.len(), 6);
    assert_eq!(results[&server.url("/ok")].outcome, Outcome::Ok);
    let dead_results: Vec<&LinkResult> = ["/a", "/b", "/c", "/d", "/e"]
        .iter()
        .map(|path| &results[&dead_url(path)])
        .collect();
    for result in &dead_results {
        assert_eq!(result.outcome, Outcome::Broken, "{result:?}");
        assert_eq!(result.failure, Some(Failure::Connect), "{result:?}");
        assert_eq!(result.status_label(), "Connection failed");
    }
    let not_requested = dead_results
        .iter()
        .filter(|result| {
            result
                .error
                .as_deref()
                .is_some_and(|error| error.starts_with("Not requested: localhost did not answer"))
        })
        .count();
    assert_eq!(
        not_requested, 3,
        "two requests, then the host is given up: {dead_results:?}"
    );
}

#[test]
fn each_link_is_requested_once_and_nothing_the_page_references() {
    let server = LinkServer::start();
    let paths = [
        "/ok",
        "/404",
        "/403",
        "/500",
        "/redirect/1",
        "/loop",
        "/slow",
        "/close",
        "/garbage",
        "/big",
        "/notitle",
    ];
    let mut links: Vec<ExternalLink> = paths.iter().map(|path| link(server.url(path))).collect();
    // The same server over TLS: no TLS there, so the handshake fails.
    let tls = format!("https://127.0.0.1:{}/ok", server.port);
    links.push(link(tls.clone()));
    let started = Instant::now();
    let results = run(links, settings(Duration::from_secs(2), Duration::from_millis(20)));
    assert_eq!(results.len(), paths.len() + 1);
    let result = |path: &str| {
        results
            .get(&server.url(path))
            .unwrap_or_else(|| panic!("no result for {path}"))
    };

    let ok = result("/ok");
    assert_eq!((ok.outcome, ok.status), (Outcome::Ok, Some(200)));
    assert_eq!(ok.title.as_deref(), Some("OK & Alive"));
    assert_eq!(ok.final_url, None);
    assert_eq!(ok.sources, ["https://site.example/"]);
    assert_eq!(
        (result("/404").outcome, result("/404").status),
        (Outcome::Broken, Some(404))
    );
    assert_eq!(result("/404").title, None, "an error page's title is not read");
    assert_eq!(result("/403").outcome, Outcome::Refused);
    assert_eq!(result("/403").status_label(), "Refused 403");
    assert_eq!(
        (result("/500").outcome, result("/500").status),
        (Outcome::Broken, Some(500))
    );
    let redirected = result("/redirect/1");
    assert_eq!((redirected.outcome, redirected.status), (Outcome::Ok, Some(200)));
    assert_eq!(redirected.final_url, Some(server.url("/final")));
    assert_eq!(redirected.title.as_deref(), Some("Final page"));
    let looped = result("/loop");
    assert_eq!(
        (looped.outcome, looped.failure),
        (Outcome::Broken, Some(Failure::TooManyRedirects))
    );
    let slow = result("/slow");
    assert_eq!((slow.outcome, slow.failure), (Outcome::Broken, Some(Failure::Timeout)));
    let closed = result("/close");
    assert_eq!(closed.outcome, Outcome::Broken);
    assert!(
        matches!(closed.failure, Some(Failure::Connect | Failure::Other)),
        "{closed:?}"
    );
    let garbage = result("/garbage");
    assert_eq!(garbage.outcome, Outcome::Broken, "{garbage:?}");
    assert!(garbage.status.is_none() && garbage.failure.is_some(), "{garbage:?}");
    let big = result("/big");
    assert_eq!(
        (big.outcome, big.status, big.title.as_deref()),
        (Outcome::Ok, Some(200), None)
    );
    assert!(big.ms < 5_000, "{big:?}");
    let plain = result("/notitle");
    assert_eq!((plain.outcome, plain.title.as_deref()), (Outcome::Ok, None));
    let tls_result = results.get(&tls).expect("the TLS link");
    assert_eq!(
        (tls_result.outcome, tls_result.failure),
        (Outcome::Broken, Some(Failure::Tls)),
        "{tls_result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());

    // Exactly one request per checked link (a redirect chain is that one check) and none for what
    // the pages reference.
    let counts = server.counts();
    for path in [
        "/ok", "/404", "/403", "/500", "/slow", "/close", "/garbage", "/big", "/notitle",
    ] {
        assert_eq!(counts.get(path), Some(&1), "{path}: {counts:?}");
    }
    for path in ["/redirect/1", "/redirect/2", "/redirect/3", "/final"] {
        assert_eq!(counts.get(path), Some(&1), "{path}: {counts:?}");
    }
    assert!(
        counts.get("/loop").is_some_and(|count| (10..=12).contains(count)),
        "{counts:?}"
    );
    for path in ["/img.png", "/app.js", "/style.css", "/other"] {
        assert_eq!(counts.get(path), None, "{path} was requested: {counts:?}");
    }
    // The client stopped reading the 5 MB body at its limit.
    // The server's write fails once the client has closed; under load that takes a moment.
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.big_written().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let written = server.big_written();
    assert!(written.len() == 1 && written[0] < BIG_BODY, "{written:?}");

    // Only the User-Agent and Accept of the check: never the site's headers or cookies.
    for request in server.requests() {
        assert_eq!(request.header("user-agent"), Some("LinkTester/1.0"), "{request:?}");
        assert_eq!(request.header("accept"), Some("text/html,*/*"));
        for name in ["cookie", "authorization", "x-site-token"] {
            assert_eq!(request.header(name), None, "{name} sent: {request:?}");
        }
    }
}

#[test]
fn requests_to_one_host_are_one_at_a_time_and_apart() {
    let server = LinkServer::start();
    let gap = Duration::from_millis(300);
    let mut links = Vec::new();
    for n in 0..3 {
        links.push(link(server.url(&format!("/a{n}"))));
        links.push(link(server.url_on("localhost", &format!("/b{n}"))));
    }
    let results = run(links, settings(Duration::from_secs(5), gap));
    assert_eq!(results.len(), 6);
    assert!(
        results.values().all(|result| result.outcome == Outcome::Ok),
        "{results:?}"
    );
    let requests = server.requests();
    for host in ["127.0.0.1", "localhost"] {
        let times: Vec<Instant> = requests
            .iter()
            .filter(|request| request.host() == host)
            .map(|request| request.at)
            .collect();
        assert_eq!(times.len(), 3, "{host}: {requests:?}");
        for pair in times.windows(2) {
            let apart = pair[1].saturating_duration_since(pair[0]);
            assert!(apart >= gap - Duration::from_millis(20), "{host}: {apart:?} apart");
        }
    }
}

#[test]
fn a_cancelled_check_starts_no_new_request_and_sends_nothing_more() {
    let server = LinkServer::start();
    let links: Vec<ExternalLink> = (0..12).map(|n| link(server.url(&format!("/slow-1s?{n}")))).collect();
    let handle = link_check::start(links, settings(Duration::from_secs(5), Duration::ZERO), || {}).expect("started");
    // One host: one request at a time. Cancel during the first.
    std::thread::sleep(Duration::from_millis(300));
    handle.cancel();
    std::thread::sleep(Duration::from_millis(1_800));
    assert!(server.requests().len() <= 2, "{:?}", server.requests());
    assert!(handle.try_next().is_none(), "nothing is sent after a cancel");
}

#[test]
fn source_pages_are_fetched_once_with_the_sites_credentials_only_on_the_site() {
    let server = LinkServer::start();
    let schema = OptionSchema::load();
    let site = server.url("/");
    let mut config = CrawlConfig::new(&site);
    config
        .set(
            &schema,
            "--header",
            Value::List(vec!["X-Site-Token: t0ken".to_string()]),
        )
        .expect("header");
    let mut settings = SourceSettings::from_config(&config, &schema, &site, Some("owner:pa55".to_string()));
    settings.gap = Duration::from_millis(20);
    let other_host = server.url_on("localhost", "/src/a");
    let pages = vec![
        server.url("/src/a"),
        server.url("/src/b"),
        server.url("/src/redirect"),
        other_host.clone(),
    ];
    let targets = vec![
        server.url("/404"),
        server.url_on("localhost", "/ok"),
        server.url("/gone"),
    ];
    let handle = link_text::start(pages.clone(), targets, settings, || {}).expect("started");
    let mut found = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match handle.try_next() {
            Some(TextMsg::Page(page, texts)) => {
                found.insert(page, texts);
            }
            Some(TextMsg::Finished) => break,
            None => {
                assert!(Instant::now() < deadline, "the fetch did not end");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    assert_eq!(found.len(), 4);
    let PageTexts::Fetched { texts, .. } = &found[&server.url("/src/a")] else {
        panic!("{:?}", found[&server.url("/src/a")]);
    };
    assert_eq!(
        texts.get(&url_key(&server.url("/404"))).map(String::as_str),
        Some("Broken page")
    );
    assert_eq!(
        texts
            .get(&url_key(&server.url_on("localhost", "/ok")))
            .map(String::as_str),
        Some("[image: Partner logo]")
    );
    assert_eq!(texts.get(&url_key(&server.url("/gone"))), None);
    assert!(matches!(&found[&server.url("/src/b")], PageTexts::Fetched { texts, .. } if texts.is_empty()));
    assert!(matches!(
        &found[&server.url("/src/redirect")],
        PageTexts::NotFetched { .. }
    ));
    assert!(matches!(&found[&other_host], PageTexts::NotFetched { .. }));

    // One request per page; nothing to the other host (not directly, not by the redirect); the
    // site's credentials and headers on every request to the site.
    let requests = server.requests();
    let paths: Vec<(&str, String)> = requests.iter().map(|r| (r.path.as_str(), r.host())).collect();
    assert_eq!(requests.len(), 3, "{paths:?}");
    for request in &requests {
        assert_eq!(request.host(), "127.0.0.1", "{paths:?}");
        assert_eq!(request.header("authorization"), Some("Basic b3duZXI6cGE1NQ=="));
        assert_eq!(request.header("x-site-token"), Some("t0ken"));
    }
    for path in ["/src/a", "/src/b", "/src/redirect"] {
        assert_eq!(server.count(path), 1, "{path}: {paths:?}");
    }
}

/// Fetches `pages` through the real fetch and returns what became of each.
fn fetch_texts(server: &LinkServer, pages: &[String], targets: Vec<String>) -> HashMap<String, PageTexts> {
    let schema = OptionSchema::load();
    let site = server.url("/");
    let mut settings = SourceSettings::from_config(&CrawlConfig::new(&site), &schema, &site, None);
    settings.gap = Duration::from_millis(20);
    let handle = link_text::start(pages.to_vec(), targets, settings, || {}).expect("started");
    let mut found = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match handle.try_next() {
            Some(TextMsg::Page(page, texts)) => {
                found.insert(page, texts);
            }
            Some(TextMsg::Finished) => return found,
            None => {
                assert!(Instant::now() < deadline, "the fetch did not end");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

#[test]
fn stylesheets_are_fetched_and_every_reference_is_located_in_the_file() {
    let server = LinkServer::start();
    let pages = vec![
        server.url("/src/css/page-styles.css"),
        server.url("/src/css/style.min.css"),
        server.url("/src/a"),
    ];
    let font = server.url("/src/fonts/missing.woff2");
    let image = server.url("/src/img/missing.png");
    let background = server.url("/img/missing-bg.png");
    let broken_page = server.url("/404");
    let found = fetch_texts(
        &server,
        &pages,
        vec![font.clone(), image.clone(), background.clone(), broken_page.clone()],
    );
    let position = |page: &str, target: &str| match &found[&server.url(page)] {
        PageTexts::Css { positions } | PageTexts::Fetched { positions, .. } => positions
            .get(&url_key(target))
            .map(|position| (position.line, position.column, position.count, position.long_line)),
        other => panic!("{page}: {other:?}"),
    };
    // A stylesheet: `url()` resolved against the stylesheet's own URL; the first reference and
    // how many there are.
    assert!(matches!(
        &found[&server.url("/src/css/page-styles.css")],
        PageTexts::Css { .. }
    ));
    assert_eq!(position("/src/css/page-styles.css", &font), Some((4, 46, 1, false)));
    assert_eq!(position("/src/css/page-styles.css", &image), Some((6, 25, 2, false)));
    assert_eq!(position("/src/css/page-styles.css", &broken_page), None);
    // A minified stylesheet: line 1 and the column.
    let column = minified_stylesheet().len() as u32 - "/img/missing-bg.png)}".len() as u32 + 1;
    assert_eq!(
        position("/src/css/style.min.css", &background),
        Some((1, column, 1, true))
    );
    // An HTML page: the position next to its link text.
    assert_eq!(position("/src/a", &broken_page), Some((1, 80, 1, false)));
    let PageTexts::Fetched { texts, .. } = &found[&server.url("/src/a")] else {
        panic!("{:?}", found[&server.url("/src/a")]);
    };
    assert_eq!(
        texts.get(&url_key(&broken_page)).map(String::as_str),
        Some("Broken page")
    );
    // Each source once; nothing the stylesheets reference is requested.
    for page in ["/src/css/page-styles.css", "/src/css/style.min.css", "/src/a"] {
        assert_eq!(server.count(page), 1, "{page}");
    }
    assert_eq!(server.requests().len(), 3, "{:?}", server.counts());
}
