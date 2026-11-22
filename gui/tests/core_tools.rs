//! One-shot calls of the real crawler binary (GUI-SPEC §4.1, §4.6): `--version` detection and the
//! failure paths of `--ai-list-models` / `--ai-check` against local endpoints (no network).

mod core_support;

use std::net::TcpListener;
use std::time::{Duration, Instant};

use core_support::{crawler_binary, shared_empty_config_file};
use siteone_crawler_gui::backend::tools::{self, ToolError, VERSION_TIMEOUT};
use siteone_crawler_gui::model::config::{CrawlConfig, Value};
use siteone_crawler_gui::model::run_plan::{ChildEnv, ai_tool_args};
use siteone_crawler_gui::model::schema::OptionSchema;
use siteone_crawler_gui::model::secrets::SecretValues;

const TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// The connection arguments of the GUI for an OpenAI-compatible endpoint.
fn connection(endpoint: &str) -> (Vec<String>, ChildEnv) {
    let schema = OptionSchema::load();
    let mut config = CrawlConfig::new("https://example.com/");
    config
        .set(&schema, "--ai-endpoint", Value::Text(endpoint.to_string()))
        .expect("endpoint");
    config
        .set(&schema, "--ai-model", Value::Text("test-model".to_string()))
        .expect("model");
    let mut secrets = SecretValues::default();
    secrets.set(siteone_crawler_gui::model::config::AI_API_KEY, "sk-test-not-a-key");
    ai_tool_args(&config, &schema, &secrets, shared_empty_config_file())
}

/// A local port nothing listens on.
fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("address").port()
}

#[test]
fn the_version_of_the_real_binary_is_detected() {
    let version = tools::crawler_version(crawler_binary(), VERSION_TIMEOUT).expect("version");
    // Built from the same source as the linked library.
    assert_eq!(version, siteone_crawler::version::CODE);
}

#[test]
fn an_unreachable_endpoint_surfaces_the_cli_error() {
    let (args, env) = connection(&format!("http://127.0.0.1:{}/v1", closed_port()));
    assert!(
        env.0
            .iter()
            .any(|(name, _)| name == siteone_crawler_gui::model::run_plan::API_KEY_ENV)
    );

    match tools::ai_list_models(crawler_binary(), &args, &env, TOOL_TIMEOUT) {
        Err(ToolError::Failed(message)) => {
            assert!(!message.is_empty());
            assert!(!message.contains("sk-test-not-a-key"), "{message}");
        }
        other => panic!("--ai-list-models: {other:?}"),
    }
    match tools::ai_check(crawler_binary(), &args, &env, TOOL_TIMEOUT) {
        Err(ToolError::Failed(message)) => assert!(!message.is_empty()),
        other => panic!("--ai-check: {other:?}"),
    }
}

#[test]
fn a_configuration_error_of_an_ai_mode_is_answered_in_json() {
    let (mut args, env) = connection("http://127.0.0.1:9/v1");
    args.push("--ai-provider=no-such-provider".to_string());
    match tools::ai_check(crawler_binary(), &args, &env, TOOL_TIMEOUT) {
        Err(ToolError::Failed(message)) => assert!(message.contains("no-such-provider"), "{message}"),
        other => panic!("--ai-check: {other:?}"),
    }
}

#[test]
fn a_tool_that_does_not_answer_is_killed_at_the_timeout() {
    // Accepts the connection and never answers.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    let holder = std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().take(4).flatten() {
            held.push(stream);
        }
        std::thread::sleep(Duration::from_secs(30));
    });
    let (args, env) = connection(&format!("http://127.0.0.1:{port}/v1"));
    let timeout = Duration::from_secs(3);
    // Built (or found up to date) before the clock starts.
    let binary = crawler_binary();
    let started = Instant::now();
    let result = tools::ai_check(binary, &args, &env, timeout);
    assert_eq!(result, Err(ToolError::Timeout(timeout)));
    assert!(
        started.elapsed() < timeout + Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    drop(holder);
}
