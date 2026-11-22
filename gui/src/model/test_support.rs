//! Helpers shared by the unit tests of the model.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// An empty config file. Tests pass it as `--config-file` so that the CLI's parser does not merge
/// a `$HOME/.siteone-crawler.conf` of the machine the tests run on.
pub fn empty_config_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let file = tempfile::Builder::new()
            .prefix("siteone-gui-empty-")
            .suffix(".conf")
            .tempfile()
            .expect("an empty config file");
        file.into_temp_path().keep().expect("kept")
    })
}

/// A complete argv (without the program name) for the CLI's parser: a URL, `args`, and the empty
/// config file.
pub fn crawler_argv<S: AsRef<str>>(args: &[S]) -> Vec<String> {
    let mut argv = vec!["--url=https://example.com/".to_string()];
    argv.extend(args.iter().map(|arg| arg.as_ref().to_string()));
    argv.push(format!("--config-file={}", empty_config_file().display()));
    argv
}

/// Runs the CLI's parser on an argv from [`crawler_argv`].
pub fn check(argv: &[String]) -> Result<(), String> {
    super::config::check_argv(argv)
}

/// The example lines of `docs/EVENTS.md`, verbatim.
pub const DOC_LINES: &[&str] = &[
    r#"{"type":"runStarted","protocol":1,"version":"2.6.1.20260914-gui-review.3","executedAt":"2026-09-25T19:06:20.727600099+02:00","url":"http://127.0.0.1:8888/","workingDir":"/tmp/evdoc"}"#,
    r#"{"type":"phase","name":"crawl","state":"finished","ms":113}"#,
    r#"{"type":"url","url":"http://127.0.0.1:8888/about.html","status":200,"contentType":"HTML","timeMs":1,"size":172,"cached":false,"done":2,"total":2}"#,
    r#"{"type":"aiRequest","seq":3,"task":"seo","label":"SEO","done":1,"total":2,"category":"SEO analysis","subject":"/about.html","provider":"openai-compatible","model":"qwen3.8","attempt":1,"maxAttempts":3,"outcome":"ok","status":200,"inputTokens":17,"outputTokens":37,"reasoningTokens":33,"cachedInputTokens":0,"reasoningChars":148,"ms":401,"outputTokensPerSecond":92.3,"totalTokensPerSecond":134.7,"finishReason":"stop"}"#,
    r#"{"type":"aiRequest","seq":1,"task":"seo","label":"SEO","done":0,"total":2,"category":"SEO analysis","subject":"/","provider":"openai-compatible","model":"qwen3.8","attempt":1,"maxAttempts":3,"outcome":"retry","status":429,"error":"HTTP 429","ms":402}"#,
    r#"{"type":"aiRequest","seq":1,"task":"seo","label":"SEO","done":0,"total":2,"category":"SEO analysis","subject":"/","provider":"openai-compatible","model":"qwen3.8","attempt":1,"maxAttempts":3,"outcome":"cacheHit","inputTokens":17,"outputTokens":37,"reasoningTokens":33,"cachedInputTokens":0,"finishReason":"stop"}"#,
    r#"{"type":"aiProgress","task":"seo","label":"SEO","done":0,"total":2,"state":"started"}"#,
    r#"{"type":"aiProgress","task":"seo","label":"SEO","done":1,"total":2,"state":"progress"}"#,
    r#"{"type":"aiProgress","task":"seo","label":"SEO","done":2,"total":2,"state":"finished"}"#,
    r#"{"type":"aiUsage","provider":"openai-compatible","model":"qwen3.8","calls":4,"cacheHits":0,"httpAttempts":5,"retries":1,"inputTokens":68,"outputTokens":148,"reasoningTokens":132,"cachedInputTokens":0,"callsWithoutUsage":0,"networkMs":3011}"#,
    r#"{"type":"artifact","kind":"ai-report-json","label":"AI report (JSON)","path":"/tmp/evdoc/out/ai-report.ia.127-0-0-1.2026-09-25-19-06-25-763-2411710.json"}"#,
    r#"{"type":"issue","kind":"ai","label":"AI phase skipped","detail":"AI phase skipped: AI is enabled but no API key resolved for provider 'anthropic'. Set ANTHROPIC_API_KEY or use --ai-api-key-file."}"#,
    r#"{"type":"runFinished","outcome":"success","exitCode":0,"ms":3375,"interrupted":false}"#,
];
