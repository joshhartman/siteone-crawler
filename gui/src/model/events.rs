//! The NDJSON event stream of `--events-file` (protocol 1, `docs/EVENTS.md`), as `serde` types.
//!
//! Following the protocol's rules: unknown event types become [`Event::Unknown`], unknown fields
//! are ignored, and a value that is absent takes its default — every field is optional here, so a
//! line is only "malformed" when it is not a JSON object with a string `type`.

use serde::Deserialize;

/// The protocol version this GUI understands.
pub const PROTOCOL: u64 = 1;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Event {
    RunStarted(RunStarted),
    Phase(Phase),
    Url(UrlEvent),
    /// Boxed: far larger than the other events.
    AiRequest(Box<AiRequest>),
    AiProgress(AiProgress),
    AiUsage(AiUsage),
    Artifact(Artifact),
    Issue(Issue),
    RunFinished(RunFinished),
    /// An event type this version does not know.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunStarted {
    pub protocol: u64,
    pub version: String,
    pub executed_at: String,
    pub url: String,
    /// Relative artifact paths resolve against it.
    pub working_dir: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Phase {
    /// `crawl`, `ai` or `analysis`.
    pub name: String,
    /// `started`, `finished` or `failed`.
    pub state: String,
    pub ms: Option<u64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UrlEvent {
    pub url: String,
    /// HTTP status; negative for a failure without one (-1 connection, -2 timeout, -4 send error).
    pub status: i64,
    /// `HTML`, `JS`, `CSS`, `Image`, …
    pub content_type: String,
    pub time_ms: u64,
    pub size: u64,
    pub cached: bool,
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiRequest {
    pub seq: u64,
    pub task: Option<String>,
    pub label: Option<String>,
    pub done: Option<u64>,
    pub total: Option<u64>,
    pub category: String,
    pub subject: Option<String>,
    pub provider: String,
    pub model: String,
    pub attempt: u64,
    pub max_attempts: u64,
    /// `ok`, `retry`, `error` or `cacheHit`.
    pub outcome: String,
    pub status: Option<u64>,
    pub error: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_chars: Option<u64>,
    pub ms: Option<u64>,
    pub output_tokens_per_second: Option<f64>,
    pub total_tokens_per_second: Option<f64>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiProgress {
    pub task: String,
    pub label: String,
    pub done: u64,
    pub total: u64,
    /// `started`, `progress` or `finished`.
    pub state: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiUsage {
    pub provider: String,
    pub model: String,
    pub calls: u64,
    pub cache_hits: u64,
    pub http_attempts: u64,
    pub retries: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cached_input_tokens: u64,
    pub calls_without_usage: u64,
    pub network_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Artifact {
    /// `html`, `json`, `text`, `offline`, `ai-report-json`, … (new kinds may appear).
    pub kind: String,
    pub label: String,
    /// A path (relative ones resolve against `RunStarted::working_dir`), or a URL for `online`.
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Issue {
    /// `upload`, `mail`, `offline`, `markdown`, `ai`, `other`.
    pub kind: String,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunFinished {
    /// `success`, `cancelled`, `qualityGate` or `failed`.
    pub outcome: String,
    pub exit_code: i64,
    pub ms: u64,
    pub interrupted: bool,
    pub error: Option<String>,
}

/// Parses one line of the stream (without its `\n`).
pub fn parse_line(line: &str) -> Result<Event, serde_json::Error> {
    serde_json::from_str(line.trim_end_matches('\r'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::test_support::DOC_LINES;

    #[test]
    fn the_documented_example_lines_parse_into_the_expected_events() {
        let events: Vec<Event> = DOC_LINES
            .iter()
            .map(|line| parse_line(line).unwrap_or_else(|error| panic!("{line}: {error}")))
            .collect();
        assert_eq!(
            events[0],
            Event::RunStarted(RunStarted {
                protocol: 1,
                version: "2.6.1.20260914-gui-review.3".to_string(),
                executed_at: "2026-09-25T19:06:20.727600099+02:00".to_string(),
                url: "http://127.0.0.1:8888/".to_string(),
                working_dir: "/tmp/evdoc".to_string(),
            })
        );
        assert_eq!(
            events[1],
            Event::Phase(Phase {
                name: "crawl".to_string(),
                state: "finished".to_string(),
                ms: Some(113),
                detail: None,
            })
        );
        assert_eq!(
            events[2],
            Event::Url(UrlEvent {
                url: "http://127.0.0.1:8888/about.html".to_string(),
                status: 200,
                content_type: "HTML".to_string(),
                time_ms: 1,
                size: 172,
                cached: false,
                done: 2,
                total: 2,
            })
        );
        let Event::AiRequest(request) = &events[3] else {
            panic!("{:?}", events[3])
        };
        assert_eq!(request.seq, 3);
        assert_eq!(request.task.as_deref(), Some("seo"));
        assert_eq!(request.reasoning_tokens, Some(33));
        assert_eq!(request.output_tokens_per_second, Some(92.3));
        assert_eq!(request.finish_reason.as_deref(), Some("stop"));
        let Event::AiRequest(retry) = &events[4] else { panic!() };
        assert_eq!(
            (retry.outcome.as_str(), retry.status, retry.input_tokens),
            ("retry", Some(429), None)
        );
        let Event::AiRequest(hit) = &events[5] else { panic!() };
        assert_eq!((hit.outcome.as_str(), hit.ms), ("cacheHit", None));
        assert_eq!(
            events[8],
            Event::AiProgress(AiProgress {
                task: "seo".to_string(),
                label: "SEO".to_string(),
                done: 2,
                total: 2,
                state: "finished".to_string(),
            })
        );
        let Event::AiUsage(usage) = &events[9] else { panic!() };
        assert_eq!((usage.calls, usage.http_attempts, usage.network_ms), (4, 5, 3011));
        let Event::Artifact(artifact) = &events[10] else {
            panic!()
        };
        assert_eq!(artifact.kind, "ai-report-json");
        let Event::Issue(issue) = &events[11] else { panic!() };
        assert_eq!((issue.kind.as_str(), issue.label.as_str()), ("ai", "AI phase skipped"));
        assert_eq!(
            events[12],
            Event::RunFinished(RunFinished {
                outcome: "success".to_string(),
                exit_code: 0,
                ms: 3375,
                interrupted: false,
                error: None,
            })
        );
    }

    #[test]
    fn unknown_types_and_fields_are_ignored_and_absent_values_tolerated() {
        assert_eq!(
            parse_line(r#"{"type":"somethingNew","x":1}"#).expect("parses"),
            Event::Unknown
        );
        let event = parse_line(r#"{"type":"url","url":"/a","newField":{"deep":[1]},"status":-2}"#).expect("parses");
        let Event::Url(url) = event else { panic!() };
        assert_eq!((url.status, url.done, url.content_type.as_str()), (-2, 0, ""));
        assert_eq!(
            parse_line("{\"type\":\"phase\",\"name\":\"ai\",\"state\":\"started\"}\r").expect("CRLF"),
            Event::Phase(Phase {
                name: "ai".to_string(),
                state: "started".to_string(),
                ..Phase::default()
            })
        );
    }

    #[test]
    fn malformed_lines_are_errors() {
        for line in [
            "",
            "not json",
            "[1]",
            r#"{"no":"type"}"#,
            r#"{"type":1}"#,
            r#"{"type":"url""#,
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }
}
