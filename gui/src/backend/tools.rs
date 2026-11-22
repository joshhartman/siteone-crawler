//! One-shot calls of the crawler binary (GUI-SPEC §4.1, §4.6): `--version`, `--ai-list-models`,
//! `--ai-check`. Each call is blocking with a timeout (the process is killed when it runs out) and
//! runs without a console window, stdin closed — call it on a worker thread.
//!
//! Both AI modes print exactly one JSON object on stdout, also on failure
//! (`{"ok":false,"error":…}`); the error is shown verbatim. Their arguments and environment come
//! from [`crate::model::run_plan::ai_tool_args`].

use std::fmt;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::model::run_plan::ChildEnv;

/// A sensible timeout for `--version`.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// The binary could not be started.
    Spawn(String),
    Timeout(Duration),
    /// The output is not what the mode prints.
    BadOutput(String),
    /// The mode answered `{"ok":false,"error":…}`; the CLI's message, verbatim.
    Failed(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::Spawn(error) => write!(f, "The crawler could not be started: {error}"),
            ToolError::Timeout(after) => write!(f, "The crawler did not answer within {} s.", after.as_secs()),
            ToolError::BadOutput(detail) => write!(f, "The crawler gave an unexpected answer: {detail}"),
            ToolError::Failed(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for ToolError {}

/// A model of `--ai-list-models`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: Option<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

/// The answer of `--ai-list-models`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelList {
    pub provider: String,
    /// The endpoint asked, without credentials.
    pub endpoint: String,
    pub models: Vec<ModelInfo>,
}

/// The answer of `--ai-check`. What the provider did not report is `None`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CheckResult {
    pub provider: String,
    pub model: String,
    pub ms: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens_per_second: Option<f64>,
    pub finish_reason: Option<String>,
    pub reply: String,
}

/// The most output of a tool that is kept per stream.
const MAX_OUTPUT: u64 = 4 * 1024 * 1024;

/// Runs `<bin> --version` (exit code 2 is normal) and returns the version.
pub fn crawler_version(bin: &Path, timeout: Duration) -> Result<String, ToolError> {
    let output = run_tool(bin, &["--version".to_string()], &ChildEnv::default(), timeout)?;
    parse_version_output(&output.stdout)
        .or_else(|| parse_version_output(&output.stderr))
        .ok_or_else(|| {
            ToolError::BadOutput(format!(
                "no version in the output of --version (exit code {})",
                output.code.map_or_else(|| "none".to_string(), |code| code.to_string())
            ))
        })
}

/// Runs `<bin> --ai-list-models <args>` with `env` added to the environment.
pub fn ai_list_models(bin: &Path, args: &[String], env: &ChildEnv, timeout: Duration) -> Result<ModelList, ToolError> {
    ai_tool("--ai-list-models", bin, args, env, timeout)
}

/// Runs `<bin> --ai-check <args>` with `env` added to the environment.
pub fn ai_check(bin: &Path, args: &[String], env: &ChildEnv, timeout: Duration) -> Result<CheckResult, ToolError> {
    ai_tool("--ai-check", bin, args, env, timeout)
}

fn ai_tool<T: DeserializeOwned>(
    mode: &str,
    bin: &Path,
    args: &[String],
    env: &ChildEnv,
    timeout: Duration,
) -> Result<T, ToolError> {
    let mut argv = vec![mode.to_string()];
    argv.extend_from_slice(args);
    let output = run_tool(bin, &argv, env, timeout)?;
    parse_tool_answer(&output.stdout).map_err(|error| match error {
        // Without a JSON answer, the last line on stderr usually says why.
        ToolError::BadOutput(detail) => match strip_ansi(&output.stderr)
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
        {
            Some(line) => ToolError::BadOutput(format!("{detail}; {line}")),
            None => ToolError::BadOutput(detail),
        },
        other => other,
    })
}

struct ToolOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs the binary to its end or until `timeout` (then it is killed).
fn run_tool(bin: &Path, args: &[String], env: &ChildEnv, timeout: Duration) -> Result<ToolOutput, ToolError> {
    let mut command = Command::new(bin);
    command
        .args(args)
        .envs(env.0.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    super::hide_console_window(&mut command);
    let mut child = command.spawn().map_err(|error| ToolError::Spawn(error.to_string()))?;
    let stdout = child.stdout.take().map(collect);
    let stderr = child.stderr.take().map(collect);

    let deadline = Instant::now() + timeout;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ToolError::Timeout(timeout));
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ToolError::Spawn(error.to_string()));
            }
        }
    };
    let text = |reader: Option<thread::JoinHandle<Vec<u8>>>| {
        reader
            .and_then(|reader| reader.join().ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    };
    Ok(ToolOutput {
        code,
        stdout: text(stdout),
        stderr: text(stderr),
    })
}

/// Reads a pipe to its end on a thread of its own, keeping the first [`MAX_OUTPUT`] bytes.
fn collect(pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut pipe = pipe;
        let mut bytes = Vec::new();
        let _ = pipe.by_ref().take(MAX_OUTPUT).read_to_end(&mut bytes);
        // Drain the rest so that the child never blocks on a full pipe.
        let _ = io::copy(&mut pipe, &mut io::sink());
        bytes
    })
}

/// The version in the output of `--version` (`Version: 2.7.0.20260925`, possibly coloured).
pub fn parse_version_output(stdout: &str) -> Option<String> {
    let plain = strip_ansi(stdout);
    plain
        .lines()
        .find_map(|line| line.trim().strip_prefix("Version:"))
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty())
}

/// The one JSON answer of an AI utility mode: its last non-empty stdout line.
pub fn parse_tool_answer<T: DeserializeOwned>(stdout: &str) -> Result<T, ToolError> {
    #[derive(Deserialize)]
    struct Status {
        ok: bool,
        #[serde(default)]
        error: Option<String>,
    }
    let line = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| ToolError::BadOutput("no output".to_string()))?;
    let status: Status =
        serde_json::from_str(line).map_err(|error| ToolError::BadOutput(format!("not a JSON answer ({error})")))?;
    if !status.ok {
        return Err(ToolError::Failed(status.error.unwrap_or_else(|| {
            "The crawler reported a failure without a message.".to_string()
        })));
    }
    serde_json::from_str(line).map_err(|error| ToolError::BadOutput(error.to_string()))
}

/// Removes ANSI escape sequences (CSI `ESC [ … letter`).
fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        plain.push(c);
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_read_from_plain_and_coloured_output() {
        assert_eq!(
            parse_version_output("\u{1b}[34mVersion: 2.7.0.20260925-dev\u{1b}[0m\n").as_deref(),
            Some("2.7.0.20260925-dev")
        );
        assert_eq!(parse_version_output("Version: 2.6.1\r\n").as_deref(), Some("2.6.1"));
        assert_eq!(parse_version_output("something else"), None);
    }

    #[test]
    fn ai_answers_parse_and_failures_carry_the_cli_message() {
        // The exact shapes `siteone_crawler::ai::tools` prints.
        let list: ModelList = parse_tool_answer(
            r#"{"ok":true,"provider":"openai-compatible","endpoint":"http://gpu:7999/v1","models":[{"id":"m","displayName":null,"contextWindow":262144,"maxOutputTokens":null}]}"#,
        )
        .expect("list");
        assert_eq!(list.models[0].context_window, Some(262_144));
        assert_eq!(list.models[0].display_name, None);
        let check: CheckResult = parse_tool_answer(
            "progress on stdout?\n{\"ok\":true,\"provider\":\"openai-compatible\",\"model\":\"m\",\"ms\":401,\"inputTokens\":17,\"outputTokens\":37,\"outputTokensPerSecond\":92.3,\"reply\":\"OK\"}\n",
        )
        .expect("check");
        assert_eq!(
            (check.ms, check.output_tokens_per_second, check.reply.as_str()),
            (Some(401), Some(92.3), "OK")
        );
        let failed = parse_tool_answer::<CheckResult>(r#"{"ok":false,"error":"HTTP 401: invalid key"}"#);
        assert_eq!(failed, Err(ToolError::Failed("HTTP 401: invalid key".to_string())));
        assert!(matches!(
            parse_tool_answer::<CheckResult>(""),
            Err(ToolError::BadOutput(_))
        ));
        assert!(matches!(
            parse_tool_answer::<CheckResult>("Usage: …"),
            Err(ToolError::BadOutput(_))
        ));
    }

    #[test]
    fn a_missing_binary_is_a_spawn_error() {
        let bin = Path::new("./definitely-not-a-crawler-binary.exe");
        assert!(matches!(
            crawler_version(bin, VERSION_TIMEOUT),
            Err(ToolError::Spawn(_))
        ));
        assert!(matches!(
            ai_check(bin, &[], &ChildEnv::default(), VERSION_TIMEOUT),
            Err(ToolError::Spawn(_))
        ));
    }
}
