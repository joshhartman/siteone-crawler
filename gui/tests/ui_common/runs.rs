//! Runs for the UI tests without a crawler process: a scripted child ([`FakeChild`]) attached to
//! the app through the same seam as the real one, fed with event lines of `docs/EVENTS.md`'s shape,
//! console lines and the exit; and History records written directly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use siteone_crawler_gui::app::{App, ChildControl};
use siteone_crawler_gui::backend::ansi::AnsiParser;
use siteone_crawler_gui::backend::runner::RunMsg;
use siteone_crawler_gui::model::console::Stream;
use siteone_crawler_gui::model::events;
use siteone_crawler_gui::model::history::{RecordedOutcome, RunRecord, RunSummary};
use siteone_crawler_gui::model::link_check::{CheckReport, Failure, LinkList, LinkResult, Outcome};
use siteone_crawler_gui::model::link_text::{LinkTexts, PageTexts, RefPosition, url_key};
use siteone_crawler_gui::model::paths::{create_run_dir, run_id_base};
use siteone_crawler_gui::model::run_plan::{RunPaths, build_plan};

/// The result JSON of a real crawl of crawler.siteone.io (73 URLs, scores, 27 tables).
pub const FIXTURE: &str = include_str!("../../../docs/OUTPUT-crawler.siteone.io.json");

pub const SITE: &str = "https://crawler.siteone.io/";

/// The 404 URL of the fixture (its `404` table says where it was found).
pub const FIXTURE_404: &str = "https://crawler.siteone.io/docs/features/content-type-analysis";
pub const FIXTURE_404_SOURCE: &str = "https://crawler.siteone.io/features/performance-metrics/";

/// The child of a run as the test drives it.
pub struct FakeChild {
    receiver: Receiver<RunMsg>,
    sender: Sender<RunMsg>,
    calls: Arc<Mutex<Vec<&'static str>>>,
    running: Arc<AtomicBool>,
}

impl ChildControl for FakeChild {
    fn try_next(&mut self) -> Option<RunMsg> {
        self.receiver.try_recv().ok()
    }

    fn stop(&mut self) -> std::io::Result<()> {
        self.calls.lock().expect("calls").push("stop");
        Ok(())
    }

    fn kill(&mut self) -> std::io::Result<()> {
        self.calls.lock().expect("calls").push("kill");
        if self.running.swap(false, Ordering::SeqCst) {
            // Like the real runner: the exit follows the kill.
            let _ = self.sender.send(RunMsg::Exited { code: Some(1) });
        }
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

/// The test's side of a [`FakeChild`].
#[derive(Clone)]
pub struct Control {
    sender: Sender<RunMsg>,
    calls: Arc<Mutex<Vec<&'static str>>>,
    running: Arc<AtomicBool>,
    pub run_dir: PathBuf,
}

impl Control {
    pub fn send(&self, msg: RunMsg) {
        self.sender.send(msg).expect("the app keeps the child");
    }

    /// An event: appended to `events.ndjson` (as the crawler writes it) and sent.
    pub fn event(&self, line: &str) {
        let event = events::parse_line(line).unwrap_or_else(|error| panic!("{line}: {error}"));
        append(
            &self.file("events.ndjson"),
            &format!(
                "{line}
"
            ),
        );
        self.send(RunMsg::Event(event));
    }

    /// Console output with ANSI colours, as the crawler writes it.
    pub fn console(&self, text: &str) {
        append(&self.file("console.log"), text);
        let mut parser = AnsiParser::new(Stream::Stdout);
        let mut lines = parser.feed(text.as_bytes());
        lines.extend(parser.finish());
        self.send(RunMsg::Console {
            stream: Stream::Stdout,
            lines,
        });
    }

    pub fn exit(&self, code: Option<i32>) {
        self.running.store(false, Ordering::SeqCst);
        self.send(RunMsg::Exited { code });
    }

    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().expect("calls").clone()
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.run_dir.join(name)
    }
}

fn append(path: &Path, text: &str) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("run file");
    file.write_all(text.as_bytes()).expect("written");
}

/// Starts a run of `url` with a scripted child: the run folder and plan are real, no process.
pub fn attach(app: &mut App, url: &str) -> Control {
    app.session.config.url = url.to_string();
    let runs_dir = app.settings.runs_dir(&app.paths);
    let now = chrono::Local::now();
    let (run_id, run_dir) = create_run_dir(&runs_dir, &run_id_base(&now, url)).expect("run dir");
    let plan = build_plan(
        &app.session.config,
        &app.schema,
        &app.secrets,
        &RunPaths {
            run_id,
            run_dir: run_dir.clone(),
            http_cache_dir: app.paths.http_cache_dir(),
            ai_cache_dir: app.paths.ai_cache_dir(),
            config_file: app.paths.empty_config_file(),
        },
    );
    let (sender, receiver) = mpsc::channel();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let running = Arc::new(AtomicBool::new(true));
    let child = FakeChild {
        receiver,
        sender: sender.clone(),
        calls: Arc::clone(&calls),
        running: Arc::clone(&running),
    };
    let config = app.session.config.clone();
    app.attach_run(Box::new(child), plan, config, now);
    Control {
        sender,
        calls,
        running,
        run_dir,
    }
}

/// Moves the start of the current run back by `seconds` (a realistic elapsed time).
pub fn age(app: &mut App, seconds: u64) {
    if let Some(run) = &mut app.current_run {
        run.started = Instant::now() - Duration::from_secs(seconds);
    }
}

fn json_string(text: &str) -> String {
    serde_json::to_string(text).expect("json")
}

pub fn run_started(control: &Control, url: &str) {
    control.event(&format!(
        r#"{{"type":"runStarted","protocol":1,"version":"2.6.1.20260914","executedAt":"2026-11-18T09:05:07+01:00","url":{},"workingDir":{}}}"#,
        json_string(url),
        json_string(&control.run_dir.display().to_string())
    ));
}

pub fn phase(control: &Control, name: &str, state: &str, ms: Option<u64>) {
    let ms = ms.map(|ms| format!(r#","ms":{ms}"#)).unwrap_or_default();
    control.event(&format!(r#"{{"type":"phase","name":"{name}","state":"{state}"{ms}}}"#));
}

#[allow(clippy::too_many_arguments)]
pub fn url_line(
    url: &str,
    status: i64,
    kind: &str,
    time_ms: u64,
    size: u64,
    cached: bool,
    done: u64,
    total: u64,
) -> String {
    format!(
        r#"{{"type":"url","url":{},"status":{status},"contentType":"{kind}","timeMs":{time_ms},"size":{size},"cached":{cached},"done":{done},"total":{total}}}"#,
        json_string(url)
    )
}

/// The URLs of a crawl of crawler.siteone.io: pages, assets, one 404 (the fixture's), three
/// 429s, a broken stylesheet image and a dead external link. `count` rows, `total` known.
pub fn site_urls(count: usize, total: u64) -> Vec<String> {
    let pages = [
        "",
        "introduction/overview/",
        "introduction/key-features/",
        "introduction/thanks/",
        "introduction/support-us/",
        "getting-started/quick-start-guide/",
        "getting-started/basic-usage/",
        "getting-started/advanced-usage/",
        "configuration/command-line-options/",
        "configuration/examples/",
        "features/seo-and-opengraph/",
        "features/performance-metrics/",
        "features/security-analysis/",
        "features/offline-website-generator/",
        "features/markdown-export/",
        "features/sitemap-generator/",
        "features/ai-analysis/",
        "docs/changelog/",
        "docs/faq/",
        "docs/contributing/",
    ];
    let assets = [
        ("_astro/index.B2x8m7jK.css", "CSS", 48_210),
        ("_astro/hoisted.Cf3a91Lz.js", "JS", 132_904),
        ("_astro/page.D1w9Q2xX.js", "JS", 2_310),
        ("fonts/inter-var.woff2", "Font", 98_116),
        ("images/logo.svg", "Image", 4_206),
        ("images/screenshot-report.png", "Image", 412_880),
        ("images/screenshot-console.png", "Image", 288_017),
        ("favicon.ico", "Image", 15_086),
        ("sitemap.xml", "XML", 9_204),
        ("robots.txt", "Other", 61),
    ];
    let mut urls = Vec::new();
    let mut done = 0u64;
    let mut push =
        |urls: &mut Vec<String>, url: String, status: i64, kind: &str, time: u64, size: u64, cached: bool| {
            done += 1;
            urls.push(url_line(&url, status, kind, time, size, cached, done, total.max(done)));
        };
    let mut n = 0usize;
    while urls.len() < count {
        let index = n;
        n += 1;
        match index {
            7 => push(&mut urls, FIXTURE_404.to_string(), 404, "HTML", 88, 3_120, false),
            15 => push(
                &mut urls,
                format!("{SITE}images/hero-background.webp"),
                404,
                "Image",
                41,
                512,
                false,
            ),
            23 => push(
                &mut urls,
                "https://twitter.example.net/siteone".to_string(),
                -1,
                "Other",
                3_002,
                0,
                false,
            ),
            31..=33 => push(
                &mut urls,
                format!("{SITE}docs/api/v{index}/"),
                429,
                "HTML",
                1_412,
                280,
                false,
            ),
            _ if index % 3 == 1 => {
                let (path, kind, size) = assets[(index / 3) % assets.len()];
                let url = if index >= 30 * assets.len() {
                    format!("{SITE}{path}?v={index}")
                } else {
                    format!("{SITE}{path}")
                };
                push(
                    &mut urls,
                    url,
                    200,
                    kind,
                    12 + (index as u64 * 7) % 90,
                    size,
                    index.is_multiple_of(5),
                )
            }
            _ => {
                let page = pages[index % pages.len()];
                let url = if index >= pages.len() * 3 {
                    format!("{SITE}{page}?page={index}")
                } else {
                    format!("{SITE}{page}")
                };
                let time = 120 + (index as u64 * 37) % 900 + if index.is_multiple_of(17) { 1_800 } else { 0 };
                push(
                    &mut urls,
                    url,
                    if index % 29 == 5 { 301 } else { 200 },
                    if index % 29 == 5 { "Redirect" } else { "HTML" },
                    time,
                    18_000 + (index as u64 * 911) % 40_000,
                    false,
                )
            }
        }
    }
    urls
}

/// The console output of a crawl, with colours.
pub const CONSOLE: &str = "\u{1b}[1m\u{1b}[36mSiteOne Crawler\u{1b}[0m v2.6.1 — https://crawler.siteone.io/\n\
\u{1b}[90m===============================================================\u{1b}[0m\n\
Progress  | URL                                              | Status | Type  | Time   | Size\n\
1/310     | https://crawler.siteone.io/                      | \u{1b}[32m200\u{1b}[0m    | HTML  | 312 ms | 38 kB\n\
2/310     | https://crawler.siteone.io/_astro/index.css      | \u{1b}[32m200\u{1b}[0m    | CSS   | 21 ms  | 47 kB\n\
8/310     | https://crawler.siteone.io/docs/features/content | \u{1b}[33m404\u{1b}[0m    | HTML  | 88 ms  | 3 kB\n\
24/310    | https://twitter.example.net/siteone              | \u{1b}[31m-1\u{1b}[0m     | Other | 3.0 s  | 0 B\n\
\u{1b}[33mWARNING:\u{1b}[0m 3 URLs answered 429 Too Many Requests; consider --max-reqs-per-sec.\n";

/// A live crawl: started, crawling, 248 of 310 URLs.
pub fn live(app: &mut App) -> Control {
    let control = attach(app, SITE);
    run_started(&control, SITE);
    phase(&control, "crawl", "started", None);
    for line in site_urls(248, 310) {
        control.event(&line);
    }
    control.console(CONSOLE);
    age(app, 125);
    control
}

/// Writes the fixture as the run's result JSON and a small HTML report; announces both.
pub fn write_outputs(control: &Control, result_json: &str) {
    std::fs::write(control.file("result.json"), result_json).expect("result.json");
    std::fs::write(control.file("report.html"), "<!DOCTYPE html><title>Report</title>").expect("report.html");
    std::fs::write(control.file("output.txt"), "SiteOne Crawler output").expect("output.txt");
    for (kind, label, name) in [
        ("html", "HTML report", "report.html"),
        ("json", "JSON output", "result.json"),
        ("text", "Text output", "output.txt"),
    ] {
        control.event(&format!(
            r#"{{"type":"artifact","kind":"{kind}","label":"{label}","path":{}}}"#,
            json_string(&control.file(name).display().to_string())
        ));
    }
    control.event(r#"{"type":"artifact","kind":"offline","label":"Offline website","path":"offline-export"}"#);
}

/// A finished crawl of 73 URLs with the fixture's results.
pub fn finished(app: &mut App) -> Control {
    finished_with(app, 73, FIXTURE)
}

pub fn finished_with(app: &mut App, urls: usize, result_json: &str) -> Control {
    let control = attach(app, SITE);
    run_started(&control, SITE);
    phase(&control, "crawl", "started", None);
    for line in site_urls(urls, urls as u64) {
        control.event(&line);
    }
    phase(&control, "crawl", "finished", Some(23_410));
    phase(&control, "analysis", "started", None);
    phase(&control, "analysis", "finished", Some(1_208));
    write_outputs(&control, result_json);
    control.console(CONSOLE);
    control.event(r#"{"type":"runFinished","outcome":"success","exitCode":0,"ms":25132,"interrupted":false}"#);
    control.exit(Some(0));
    control
}

/// Only 200 responses (no broken link), the fixture's results (with unchecked external links).
pub fn finished_clean(app: &mut App) -> Control {
    let control = attach(app, SITE);
    run_started(&control, SITE);
    phase(&control, "crawl", "started", None);
    for done in 1..=40u64 {
        control.event(&url_line(
            &format!("{SITE}page-{done}/"),
            200,
            "HTML",
            140,
            21_000,
            false,
            done,
            40,
        ));
    }
    phase(&control, "crawl", "finished", Some(9_812));
    phase(&control, "analysis", "started", None);
    phase(&control, "analysis", "finished", Some(904));
    write_outputs(&control, FIXTURE);
    control.event(r#"{"type":"runFinished","outcome":"success","exitCode":0,"ms":10950,"interrupted":false}"#);
    control.exit(Some(0));
    control
}

/// The AI events of `docs/EVENTS.md`.
pub const AI_LINES: &[&str] = &[
    r#"{"type":"phase","name":"ai","state":"started"}"#,
    r#"{"type":"aiProgress","task":"seo","label":"SEO","done":0,"total":2,"state":"started"}"#,
    r#"{"type":"aiRequest","seq":1,"task":"seo","label":"SEO","done":0,"total":2,"category":"SEO analysis","subject":"/","provider":"openai-compatible","model":"qwen3.8","attempt":1,"maxAttempts":3,"outcome":"retry","status":429,"error":"HTTP 429","ms":402}"#,
    r#"{"type":"aiRequest","seq":1,"task":"seo","label":"SEO","done":0,"total":2,"category":"SEO analysis","subject":"/","provider":"openai-compatible","model":"qwen3.8","attempt":1,"maxAttempts":3,"outcome":"cacheHit","inputTokens":17,"outputTokens":37,"reasoningTokens":33,"cachedInputTokens":0,"finishReason":"stop"}"#,
    r#"{"type":"aiProgress","task":"seo","label":"SEO","done":1,"total":2,"state":"progress"}"#,
    r#"{"type":"aiRequest","seq":3,"task":"seo","label":"SEO","done":1,"total":2,"category":"SEO analysis","subject":"/about.html","provider":"openai-compatible","model":"qwen3.8","attempt":1,"maxAttempts":3,"outcome":"ok","status":200,"inputTokens":17,"outputTokens":37,"reasoningTokens":33,"cachedInputTokens":0,"reasoningChars":148,"ms":401,"outputTokensPerSecond":92.3,"totalTokensPerSecond":134.7,"finishReason":"stop"}"#,
    r#"{"type":"aiProgress","task":"seo","label":"SEO","done":2,"total":2,"state":"finished"}"#,
    r#"{"type":"aiProgress","task":"summary","label":"Site summary","done":3,"total":8,"state":"progress"}"#,
    r#"{"type":"aiUsage","provider":"openai-compatible","model":"qwen3.8","calls":4,"cacheHits":1,"httpAttempts":5,"retries":1,"inputTokens":68,"outputTokens":148,"reasoningTokens":132,"cachedInputTokens":0,"callsWithoutUsage":0,"networkMs":3011}"#,
    r#"{"type":"phase","name":"ai","state":"finished","ms":3011}"#,
    r#"{"type":"issue","kind":"ai","label":"AI phase partly skipped","detail":"The summary task stopped after 3 of 8 pages: the model's context window is too small for the remaining pages."}"#,
];

/// A finished crawl with an AI phase.
pub fn finished_ai(app: &mut App) -> Control {
    let control = attach(app, SITE);
    run_started(&control, SITE);
    phase(&control, "crawl", "started", None);
    for line in site_urls(73, 73) {
        control.event(&line);
    }
    phase(&control, "crawl", "finished", Some(23_410));
    for line in AI_LINES {
        control.event(line);
    }
    phase(&control, "analysis", "started", None);
    phase(&control, "analysis", "finished", Some(1_208));
    write_outputs(&control, FIXTURE);
    control.event(r#"{"type":"runFinished","outcome":"success","exitCode":0,"ms":28132,"interrupted":false}"#);
    control.exit(Some(0));
    control
}

pub fn failed(app: &mut App) -> Control {
    let control = attach(app, SITE);
    age(app, 4);
    run_started(&control, SITE);
    phase(&control, "crawl", "started", None);
    for line in site_urls(12, 310) {
        control.event(&line);
    }
    phase(&control, "crawl", "failed", None);
    control.console("\u{1b}[31mERROR:\u{1b}[0m Upload of the HTML report failed: connection reset by peer.\n");
    control.event(r#"{"type":"runFinished","outcome":"failed","exitCode":1,"ms":4210,"interrupted":false,"error":"Upload of the HTML report failed: connection reset by peer"}"#);
    control.exit(Some(1));
    control
}

pub fn crashed(app: &mut App) -> Control {
    let control = attach(app, SITE);
    age(app, 74);
    run_started(&control, SITE);
    phase(&control, "crawl", "started", None);
    for line in site_urls(57, 310) {
        control.event(&line);
    }
    control.console(CONSOLE);
    control.console("thread 'main' panicked at src/engine/crawler.rs:912:40:\nindex out of bounds: the len is 3 but the index is 3\n");
    control.exit(Some(-1073740791));
    control
}

pub fn no_pages(app: &mut App) -> Control {
    let control = attach(app, "https://intranet.invalid/");
    run_started(&control, "https://intranet.invalid/");
    phase(&control, "crawl", "started", None);
    control.event(&url_line(
        "https://intranet.invalid/",
        -1,
        "HTML",
        2_004,
        0,
        false,
        1,
        1,
    ));
    phase(&control, "crawl", "finished", Some(2_010));
    control.console(
        "\u{1b}[31mERROR:\u{1b}[0m No pages were crawled: https://intranet.invalid/ could not be resolved (DNS).\n",
    );
    control.event(r#"{"type":"runFinished","outcome":"failed","exitCode":3,"ms":2212,"interrupted":false,"error":"No pages crawled"}"#);
    control.exit(Some(3));
    control
}

/// The CLI rejected the configuration: no events, an `ERROR:` line, exit 101.
pub fn config_error(app: &mut App) -> Control {
    let control = attach(app, SITE);
    control.console("\u{1b}[31mERROR: Option --max-depth (deep) must be a non-negative integer\u{1b}[0m\nUsage: siteone-crawler --url=<url> [options]\n");
    control.exit(Some(101));
    control
}

/// Writes a run folder with a record (History).
pub fn record(
    runs_dir: &Path,
    id: &str,
    url: &str,
    outcome: Option<RecordedOutcome>,
    score: Option<f64>,
    urls: u64,
) -> PathBuf {
    let dir = runs_dir.join(id);
    std::fs::create_dir_all(&dir).expect("run dir");
    let started = format!(
        "{}-{}-{}T{}:{}:{}+01:00",
        &id[0..4],
        &id[4..6],
        &id[6..8],
        &id[9..11],
        &id[11..13],
        &id[13..15]
    );
    let record = RunRecord {
        run_id: id.to_string(),
        url: url.to_string(),
        started_at: started,
        finished_at: None,
        duration_ms: outcome.map(|_| 12_000 + urls * 310),
        outcome,
        exit_code: outcome.map(|outcome| match outcome {
            RecordedOutcome::Finished | RecordedOutcome::Stopped => 0,
            RecordedOutcome::QualityGateFailed => 10,
            RecordedOutcome::NoPagesCrawled => 3,
            _ => 1,
        }),
        argv: vec![format!("--url={url}")],
        config: siteone_crawler_gui::model::config::CrawlConfig::new(url),
        summary: RunSummary {
            urls,
            overall_score: score,
            overall_label: score.map(|score| {
                if score >= 9.0 {
                    "Excellent"
                } else if score >= 7.0 {
                    "Good"
                } else if score >= 5.0 {
                    "Fair"
                } else {
                    "Poor"
                }
                .to_string()
            }),
            ..RunSummary::default()
        },
        ..RunRecord::default()
    };
    record.write(&dir).expect("run.json");
    std::fs::write(dir.join("console.log"), "SiteOne Crawler\n").expect("console.log");
    dir
}

/// A History of several runs.
pub fn history(runs_dir: &Path) {
    record(
        runs_dir,
        "20261118-090507-crawler.siteone.io",
        SITE,
        Some(RecordedOutcome::Finished),
        Some(8.3),
        73,
    );
    record(
        runs_dir,
        "20261117-163012-www.example.com",
        "https://www.example.com/",
        Some(RecordedOutcome::Stopped),
        Some(6.4),
        1_204,
    );
    record(
        runs_dir,
        "20261117-101544-shop.example.org",
        "https://shop.example.org/catalog/",
        Some(RecordedOutcome::QualityGateFailed),
        Some(4.8),
        12_480,
    );
    record(
        runs_dir,
        "20261116-084005-intranet.invalid",
        "https://intranet.invalid/",
        Some(RecordedOutcome::NoPagesCrawled),
        None,
        1,
    );
    record(
        runs_dir,
        "20261115-221930-docs.example.com",
        "https://docs.example.com/guide/getting-started/installation/windows/",
        Some(RecordedOutcome::Crashed),
        None,
        312,
    );
    record(
        runs_dir,
        "20261114-110203-blog.example.com",
        "https://blog.example.com/",
        None,
        None,
        88,
    );
    record(
        runs_dir,
        "20261113-075500-example.com",
        "https://example.com/",
        Some(RecordedOutcome::Finished),
        Some(9.4),
        42,
    );
}

/// A run folder whose record is damaged.
pub fn damaged(runs_dir: &Path) -> PathBuf {
    let dir = runs_dir.join("20261118-120000-damaged.example.com");
    std::fs::create_dir_all(&dir).expect("run dir");
    std::fs::write(dir.join("run.json"), "{\"schemaVersion\":1,\"runId\":").expect("run.json");
    dir
}

// ---------------------------------------------------------------------------------------------
// External link check
// ---------------------------------------------------------------------------------------------

/// A check that never ends by itself (the test fills its results).
pub struct FakeCheck {
    pub cancelled: Arc<AtomicBool>,
}

impl siteone_crawler_gui::app::CheckControl for FakeCheck {
    fn try_next(&mut self) -> Option<siteone_crawler_gui::backend::link_check::CheckMsg> {
        None
    }

    fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

/// Mixed results for the links of `list`: mostly OK with a title, some broken (404, a DNS failure,
/// a timeout), some refused (403), some redirected.
pub fn check_results(list: &LinkList, count: usize) -> Vec<LinkResult> {
    list.links
        .iter()
        .take(count)
        .enumerate()
        .map(|(index, link)| {
            let host = url::Url::parse(&link.url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_string))
                .unwrap_or_default();
            let base = LinkResult {
                url: link.url.clone(),
                sources: link.sources.clone(),
                count: link.count,
                ms: 180 + (index as u64 * 37) % 900,
                ..LinkResult::default()
            };
            match index % 12 {
                3 => LinkResult {
                    outcome: Outcome::Broken,
                    status: Some(404),
                    ..base
                },
                5 => LinkResult {
                    outcome: Outcome::Refused,
                    status: Some(403),
                    ..base
                },
                7 if index < 30 => LinkResult {
                    outcome: Outcome::Broken,
                    failure: Some(Failure::Dns),
                    error: Some("dns error: No such host is known. (os error 11001)".to_string()),
                    ..base
                },
                9 if index < 30 => LinkResult {
                    outcome: Outcome::Broken,
                    failure: Some(Failure::Timeout),
                    error: Some("operation timed out".to_string()),
                    ..base
                },
                other => LinkResult {
                    outcome: Outcome::Ok,
                    status: Some(200),
                    title: (other != 10).then(|| {
                        format!(
                            "{} — {}",
                            title_case(&host),
                            ["Home", "About us", "Documentation", "Blog"][index % 4]
                        )
                    }),
                    final_url: (other == 1).then(|| format!("{}/en/", link.url.trim_end_matches('/'))),
                    ..base
                },
            }
        })
        .collect()
}

fn title_case(host: &str) -> String {
    let name = host.trim_start_matches("www.").split('.').next().unwrap_or(host);
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

/// A position of a reference: `line`, `column`, `count`; a long line past column 500.
pub fn position(line: u32, column: u32, count: u32) -> RefPosition {
    RefPosition {
        line,
        column,
        count,
        long_line: column > 500,
        crawled: false,
    }
}

/// Link texts for the 404 of the fixture (line 212, twice on its page) and for most of the links
/// of `list` (one not found; some with the position of the link on a minified page).
pub fn link_texts(list: &LinkList) -> LinkTexts {
    let mut texts = LinkTexts::default();
    let mut add = |page: &str, target: &str, text: &str, at: Option<RefPosition>| {
        if let PageTexts::Fetched { texts, positions } = texts
            .pages
            .entry(page.to_string())
            .or_insert_with(|| PageTexts::texts(BTreeMap::new()))
            && !text.is_empty()
        {
            texts.insert(url_key(target), text.to_string());
            if let Some(at) = at {
                positions.insert(url_key(target), at);
            }
        }
    };
    add(
        FIXTURE_404_SOURCE,
        FIXTURE_404,
        "Content type analysis",
        Some(position(212, 17, 2)),
    );
    for (index, link) in list.links.iter().enumerate() {
        let Some(source) = link.sources.first() else { continue };
        let text = match index % 6 {
            0 => title_case(
                url::Url::parse(&link.url)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_string))
                    .as_deref()
                    .unwrap_or(""),
            ),
            1 => "GitHub repository".to_string(),
            2 => "[image: Sponsor logo]".to_string(),
            3 => "Read the full article on their blog".to_string(),
            4 => String::new(),
            _ => "Join the community".to_string(),
        };
        let at = match index % 6 {
            1 => Some(position(1, 20_345 + index as u32, 1)),
            4 => None,
            _ => Some(position(40 + index as u32 * 3, 9, 1)),
        };
        add(source, &link.url, &text, at);
    }
    texts
}

/// The fixture's result JSON with one more row in its `404` table: the missing hero image, found
/// on the site's stylesheet.
pub fn fixture_with_a_stylesheet_404() -> String {
    // The row of the `404` table (the fixture's line ends depend on the checkout).
    let table_row = FIXTURE
        .find("\"statusCode\": \"404\",")
        .expect("the fixture's 404 table");
    let end = FIXTURE[table_row..].find('}').expect("the end of the row") + table_row + 1;
    let row = format!(r#", {{"sourceUqId": "{STYLESHEET}", "statusCode": "404", "url": "{HERO_IMAGE}"}}"#);
    format!("{}{row}{}", &FIXTURE[..end], &FIXTURE[end..])
}

/// The site's stylesheet (crawled) and the missing image it references.
pub const STYLESHEET: &str = "https://crawler.siteone.io/_astro/index.B2x8m7jK.css";
pub const HERO_IMAGE: &str = "https://crawler.siteone.io/images/hero-background.webp";

/// A finished crawl whose two 404s were found on a page and on a stylesheet, with the link texts
/// read after the crawl (`link-texts.json`): the page's link at line 212 (twice), the
/// stylesheet's `url()` at line 1929.
pub fn finished_with_positions(app: &mut App) -> Control {
    let control = finished_with(app, 73, &fixture_with_a_stylesheet_404());
    let mut texts = LinkTexts::default();
    texts.pages.insert(
        FIXTURE_404_SOURCE.to_string(),
        PageTexts::Fetched {
            texts: BTreeMap::from([(url_key(FIXTURE_404), "Content type analysis".to_string())]),
            positions: BTreeMap::from([(url_key(FIXTURE_404), position(212, 17, 2))]),
        },
    );
    texts.pages.insert(
        STYLESHEET.to_string(),
        PageTexts::Css {
            positions: BTreeMap::from([(url_key(HERO_IMAGE), position(1929, 22, 1))]),
        },
    );
    let run_dir = app.current_run.as_ref().expect("run").plan.run_dir.clone();
    texts.write(&run_dir).expect("link-texts.json");
    control
}

/// The referring page the crawler reports for URL row `index` of [`site_urls`] (`--link-details`):
/// `(source, attr, link text, line, column, depth)`; `None` for the start page.
/// `(source, attr, link text, line, column, depth)`.
type CrawlerSource = (String, &'static str, Option<String>, u32, u32, u32);

fn crawler_source(index: usize, url: &str) -> Option<CrawlerSource> {
    if url == SITE {
        return None;
    }
    Some(if url == FIXTURE_404 {
        (
            FIXTURE_404_SOURCE.to_string(),
            "<a href>",
            Some("Content type analysis".to_string()),
            212,
            17,
            2,
        )
    } else if url == HERO_IMAGE {
        (STYLESHEET.to_string(), "css url()", None, 1, 20_345, 2)
    } else if url.contains("/docs/api/") {
        let version = url.trim_end_matches('/').rsplit('/').next().unwrap_or("");
        (
            format!("{SITE}docs/changelog/"),
            "<a href>",
            Some(format!("API {version}")),
            40 + index as u32,
            9,
            2,
        )
    } else if url.contains("twitter.example.net") {
        (SITE.to_string(), "<a href>", Some("Follow us".to_string()), 88, 13, 1)
    } else if url.ends_with(".css") || url.contains(".css?") {
        (SITE.to_string(), "<link href>", None, 6, 29, 1)
    } else if url.contains(".js") {
        (SITE.to_string(), "<script src>", None, 7, 15, 1)
    } else if url.contains("/images/") || url.contains("favicon") || url.contains("/fonts/") {
        (
            SITE.to_string(),
            "<img src>",
            Some("Screenshot".to_string()),
            30 + index as u32,
            11,
            1,
        )
    } else {
        (
            SITE.to_string(),
            "<a href>",
            Some("Read more".to_string()),
            10 + index as u32,
            21,
            1,
        )
    })
}

/// A finished crawl of 73 URLs by a crawler with link data (`--link-details`): every `url` event
/// names its page, link text and line; the result JSON (the fixture) has a `links` table — the 404
/// on 57 pages (three listed), every external link of the fixture on one page more than its
/// `external-urls` row says.
pub fn finished_with_link_data(app: &mut App) -> Control {
    let control = attach(app, SITE);
    run_started(&control, SITE);
    phase(&control, "crawl", "started", None);
    let mut links: Vec<serde_json::Value> = Vec::new();
    let link_row =
        |url: &str, source: &str, attr: &str, text: Option<&str>, line: u32, column: u32, in_links: usize| {
            serde_json::json!({
                "url": url, "sourceUrl": source, "sourceAttr": attr, "linkText": text.unwrap_or(""),
                "sourceLine": line.to_string(), "sourceColumn": column.to_string(),
                "inLinks": in_links.to_string(), "external": if url.starts_with(SITE) { "0" } else { "1" },
            })
        };
    for (index, line) in site_urls(73, 73).into_iter().enumerate() {
        let mut event: serde_json::Value = serde_json::from_str(&line).expect("url line");
        let url = event["url"].as_str().unwrap_or("").to_string();
        event["external"] = serde_json::json!(!url.starts_with(SITE));
        match crawler_source(index, &url) {
            Some((source, attr, text, line, column, depth)) => {
                event["source"] = serde_json::json!(source);
                event["sourceAttr"] = serde_json::json!(attr);
                if let Some(text) = &text {
                    event["linkText"] = serde_json::json!(text);
                }
                event["sourceLine"] = serde_json::json!(line);
                event["sourceColumn"] = serde_json::json!(column);
                event["depth"] = serde_json::json!(depth);
                let in_links = if url == FIXTURE_404 { 57 } else { index % 7 + 1 };
                links.push(link_row(&url, &source, attr, text.as_deref(), line, column, in_links));
                if url == FIXTURE_404 {
                    for (page, line) in [
                        (format!("{SITE}docs/faq/"), 64),
                        (format!("{SITE}introduction/overview/"), 118),
                    ] {
                        links.push(link_row(&url, &page, attr, text.as_deref(), line, 17, in_links));
                    }
                }
            }
            None => event["depth"] = serde_json::json!(0),
        }
        control.event(&event.to_string());
    }
    // The external links of the fixture, each on its pages and one more.
    let legacy = siteone_crawler_gui::model::link_check::link_list(
        &siteone_crawler_gui::model::results::parse_result(FIXTURE).expect("fixture"),
        SITE,
    );
    for (index, link) in legacy.links.iter().enumerate() {
        let mut pages = link.sources.clone();
        pages.push(format!("{SITE}docs/faq/"));
        for page in &pages {
            links.push(link_row(
                &link.url,
                page,
                "<a href>",
                Some("Partner"),
                50 + index as u32,
                5,
                pages.len(),
            ));
        }
    }
    links.push(link_row(
        "https://cdn.example.net/analytics.js",
        SITE,
        "<script src>",
        None,
        8,
        15,
        1,
    ));
    let table = serde_json::json!({
        "aplCode": "links", "title": "Links", "columns": {"url": {"name": "URL"}}, "rows": links,
    });
    let at = FIXTURE.find("\"tables\": {").expect("the fixture's tables") + "\"tables\": {".len();
    let result = format!("{}\"links\": {table},{}", &FIXTURE[..at], &FIXTURE[at..]);
    phase(&control, "crawl", "finished", Some(23_410));
    phase(&control, "analysis", "started", None);
    phase(&control, "analysis", "finished", Some(1_208));
    write_outputs(&control, &result);
    control.console(CONSOLE);
    control.event(r#"{"type":"runFinished","outcome":"success","exitCode":0,"ms":25132,"interrupted":false}"#);
    control.exit(Some(0));
    control
}

/// The current run's external links as if a check ran: `done` of them checked; with `running`
/// the check is still going (a scripted one), else it is complete. Call after the result loaded.
pub fn with_check(app: &mut App, done: usize, running: bool) {
    let run = app.current_run.as_mut().expect("run");
    let list = run.links.list.clone().expect("the result JSON is loaded");
    run.links.texts = link_texts(&list);
    let results = check_results(&list, done);
    let run_dir = run.plan.run_dir.clone();
    if running {
        app.attach_link_check(
            run_dir,
            Box::new(FakeCheck {
                cancelled: Arc::new(AtomicBool::new(false)),
            }),
            &list,
        );
        if let Some(active) = &mut app.link_check {
            active.results = results;
        }
    } else {
        let run = app.current_run.as_mut().expect("run");
        run.auto_check = true;
        run.links.report = Some(CheckReport {
            complete: true,
            checked_at: "2026-11-18T10:00:00+01:00".to_string(),
            total: list.links.len(),
            files_not_checked: list.files,
            truncated: 0,
            results,
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Retry passes
// ---------------------------------------------------------------------------------------------

/// How a scripted pass behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassScript {
    /// Every URL is answered at once: a crawled URL 200 (HTML, "Retried page"), an external link
    /// OK; a refused one fails with its reason.
    Answer,
    /// As [`PassScript::Answer`], but this crawled URL answers 404 again (it is still broken).
    AnswerBut(&'static str),
    /// The pass starts on its first URL and never goes on (a pass in progress).
    Hold,
}

/// A scripted pass: its messages are queued when it starts.
pub struct FakePass {
    messages: std::collections::VecDeque<siteone_crawler_gui::backend::retry::RetryMsg>,
    pub cancelled: Arc<AtomicBool>,
}

impl siteone_crawler_gui::app::RetryControl for FakePass {
    fn try_next(&mut self) -> Option<siteone_crawler_gui::backend::retry::RetryMsg> {
        if self.cancelled.load(Ordering::SeqCst) {
            return None;
        }
        self.messages.pop_front()
    }

    fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

/// Makes the app's passes scripted; returns the items of every pass started.
pub fn scripted_passes(
    app: &mut App,
    script: PassScript,
) -> Arc<Mutex<Vec<Vec<siteone_crawler_gui::backend::retry::RetryItem>>>> {
    use siteone_crawler_gui::backend::retry::{RetryMsg, RetryTarget};
    use siteone_crawler_gui::model::retry::{Answer, CrawledAnswer};
    let passes = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&passes);
    app.retry_spawner = Some(Box::new(move |items, _settings| {
        let mut messages = std::collections::VecDeque::new();
        for item in &items {
            messages.push_back(RetryMsg::Started(item.url.clone()));
            if script == PassScript::Hold {
                break;
            }
            messages.push_back(match &item.target {
                RetryTarget::Crawled { .. } if matches!(script, PassScript::AnswerBut(url) if url == item.url) => {
                    RetryMsg::Answered {
                        url: item.url.clone(),
                        answer: Answer::Crawled(CrawledAnswer {
                            status: 404,
                            content_type: "HTML".to_string(),
                            time_ms: 90,
                            size: 3_120,
                            cached: false,
                            title: None,
                        }),
                    }
                }
                RetryTarget::Crawled { .. } => RetryMsg::Answered {
                    url: item.url.clone(),
                    answer: Answer::Crawled(CrawledAnswer {
                        status: 200,
                        content_type: "HTML".to_string(),
                        time_ms: 140,
                        size: 18_200,
                        cached: false,
                        title: Some("Retried page".to_string()),
                    }),
                },
                RetryTarget::External(link) => RetryMsg::Answered {
                    url: item.url.clone(),
                    answer: Answer::External(LinkResult {
                        url: link.url.clone(),
                        sources: link.sources.clone(),
                        count: link.count,
                        outcome: Outcome::Ok,
                        status: Some(200),
                        title: Some("Back online".to_string()),
                        ms: 210,
                        ..LinkResult::default()
                    }),
                },
                RetryTarget::Refused(reason) => RetryMsg::Failed {
                    url: item.url.clone(),
                    reason: reason.clone(),
                },
            });
        }
        if script != PassScript::Hold {
            messages.push_back(RetryMsg::Finished);
        }
        recorded.lock().expect("passes").push(items);
        Box::new(FakePass {
            messages,
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }));
    passes
}
