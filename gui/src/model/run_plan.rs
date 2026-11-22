//! The plan of a run (GUI-SPEC §4.2): from a configuration, its secrets and the app's paths, the
//! child's full argv with the managed options, its environment, and the redacted argv for History.
//!
//! [`build_plan`] is pure: the run directory is created beforehand by
//! [`super::paths::create_run_dir`], which resolves id collisions on disk.

use std::fmt;
use std::path::{Path, PathBuf};

use siteone_crawler::utils::{redact_header_value, redact_url_userinfo};

use super::config::{AI_API_KEY, CrawlConfig, MASK, Value, check_argv, config_file_arg};
use super::schema::OptionSchema;
use super::secrets::SecretValues;

/// The environment variable a run passes the stored API key in.
pub const API_KEY_ENV: &str = "SITEONE_GUI_AI_API_KEY";

pub const EVENTS_FILE: &str = "events.ndjson";
pub const CONSOLE_LOG: &str = "console.log";
pub const RESULT_JSON: &str = "result.json";
pub const REPORT_HTML: &str = "report.html";
pub const OUTPUT_TEXT: &str = "output.txt";

/// The console width the GUI asks for.
pub const CONSOLE_WIDTH: u32 = 160;

/// The AI options that make up a connection, passed to `--ai-list-models` / `--ai-check`.
const AI_CONNECTION: &[&str] = &[
    "--ai-provider",
    "--ai-endpoint",
    "--ai-model",
    "--ai-api-key-env",
    "--ai-api-key-file",
    "--ai-max-tokens",
    "--ai-use-max-completion-tokens",
    "--ai-temperature",
    "--ai-extra-body",
    "--ai-timeout",
    "--ai-max-reqs-per-sec",
];

/// Child environment variables. `Debug` shows the names only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ChildEnv(pub Vec<(String, String)>);

impl fmt::Debug for ChildEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.iter().map(|(name, _)| name)).finish()
    }
}

/// Where a run's files go and which shared files it uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPaths {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub http_cache_dir: PathBuf,
    pub ai_cache_dir: PathBuf,
    /// The app's empty config file ([`super::paths::AppPaths::empty_config_file`]).
    pub config_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlan {
    pub run_id: String,
    /// Also the child's working directory.
    pub run_dir: PathBuf,
    pub events_file: PathBuf,
    pub console_log: PathBuf,
    pub result_json: PathBuf,
    /// The child's arguments, without the program name.
    pub argv: Vec<String>,
    pub env: ChildEnv,
    /// [`Self::argv`] with secrets masked, userinfo and header values redacted — for History.
    pub redacted_argv: Vec<String>,
}

impl RunPlan {
    /// Validates the full argv with the CLI's parser (GUI-SPEC §5.2).
    pub fn validate(&self) -> Result<(), String> {
        check_argv(&self.argv)
    }
}

/// Builds the plan of a run (GUI-SPEC §4.2). The managed options are added after the
/// configuration's options; `--output-html-report`, `--output-text-file`, the cache directories
/// and the API key variable only where the configuration does not set them.
pub fn build_plan(config: &CrawlConfig, schema: &OptionSchema, secrets: &SecretValues, paths: &RunPaths) -> RunPlan {
    let run_dir = paths.run_dir.clone();
    let file = |name: &str| run_dir.join(name);
    let events_file = file(EVENTS_FILE);
    let result_json = file(RESULT_JSON);
    let user_sets = |name: &str| config.values.contains_key(name);
    let path_arg = |name: &str, path: &Path| format!("{name}={}", path.display());

    let mut argv = config.to_argv(schema, secrets);
    argv.push(config_file_arg(&paths.config_file));
    argv.push(path_arg("--events-file", &events_file));
    argv.push("--control-stdin=true".to_string());
    argv.push("--output=text".to_string());
    argv.push("--force-color=true".to_string());
    argv.push(format!("--console-width={CONSOLE_WIDTH}"));
    argv.push("--hide-progress-bar=true".to_string());
    argv.push(path_arg("--output-json-file", &result_json));
    if !user_sets("--output-html-report") {
        argv.push(path_arg("--output-html-report", &file(REPORT_HTML)));
    }
    if !user_sets("--output-text-file") {
        argv.push(path_arg("--output-text-file", &file(OUTPUT_TEXT)));
    }
    // `--no-cache` only switches the HTTP cache off; the AI cache is separate in the CLI.
    let no_cache = config.values.get("--no-cache") == Some(&Value::Bool(true));
    if !user_sets("--http-cache-dir") && !no_cache {
        argv.push(path_arg("--http-cache-dir", &paths.http_cache_dir));
    }
    let mut env = ChildEnv::default();
    if config.ai_enabled {
        // An AI option outside an AI run would turn AI on in the CLI.
        if !user_sets("--ai-cache-dir") {
            argv.push(path_arg("--ai-cache-dir", &paths.ai_cache_dir));
        }
        if config.passes_api_key_by_env(secrets)
            && let Some(key) = secrets.get(AI_API_KEY)
        {
            argv.push(format!("--ai-api-key-env={API_KEY_ENV}"));
            env.0.push((API_KEY_ENV.to_string(), key.to_string()));
        }
    }

    RunPlan {
        run_id: paths.run_id.clone(),
        events_file,
        console_log: file(CONSOLE_LOG),
        result_json,
        redacted_argv: redact_argv(schema, &argv),
        argv,
        env,
        run_dir,
    }
}

/// The arguments and environment for `--ai-list-models` / `--ai-check` (GUI-SPEC §4.6): the
/// connection options of the configuration (`--ai-provider` always), the key as in a run, and the
/// app's empty config file (`config_file`) as in a run.
pub fn ai_tool_args(
    config: &CrawlConfig,
    schema: &OptionSchema,
    secrets: &SecretValues,
    config_file: &Path,
) -> (Vec<String>, ChildEnv) {
    let mut enabled = config.clone();
    enabled.ai_enabled = true;
    let mut args: Vec<String> = enabled
        .to_argv(schema, secrets)
        .into_iter()
        .filter(|arg| {
            let name = arg.split('=').next().unwrap_or(arg);
            AI_CONNECTION.contains(&name.strip_suffix(':').unwrap_or(name))
        })
        .collect();
    let mut env = ChildEnv::default();
    if enabled.passes_api_key_by_env(secrets)
        && let Some(key) = secrets.get(AI_API_KEY)
    {
        args.push(format!("--ai-api-key-env={API_KEY_ENV}"));
        env.0.push((API_KEY_ENV.to_string(), key.to_string()));
    }
    args.push(config_file_arg(config_file));
    (args, env)
}

/// An argv safe to store: values of secret options masked, userinfo removed from URL options,
/// header values replaced by `***` — the crawler's own redaction rules for the last two.
pub fn redact_argv(schema: &OptionSchema, argv: &[String]) -> Vec<String> {
    argv.iter()
        .map(|arg| {
            let Some((name_part, value)) = arg.split_once('=') else {
                return arg.clone();
            };
            let name = name_part.strip_suffix(':').unwrap_or(name_part);
            let Some(spec) = schema.get(name) else {
                return arg.clone();
            };
            if spec.secret {
                format!("{name_part}={MASK}")
            } else if spec.kind == siteone_crawler::options::option_type::OptionType::HttpHeader {
                format!("{name_part}={}", redact_header_value(value))
            } else if matches!(
                spec.name.as_str(),
                "--url" | "--proxy" | "--upload-to" | "--ai-endpoint"
            ) {
                format!("{name_part}={}", redact_url_userinfo(value))
            } else {
                arg.clone()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> RunPaths {
        RunPaths {
            run_id: "20261118-090507-example.com".to_string(),
            run_dir: dir.join("runs").join("20261118-090507-example.com"),
            http_cache_dir: dir.join("cache").join("http"),
            ai_cache_dir: dir.join("cache").join("ai"),
            config_file: crate::model::test_support::empty_config_file().to_path_buf(),
        }
    }

    fn has(argv: &[String], prefix: &str) -> bool {
        argv.iter().any(|arg| arg.starts_with(prefix))
    }

    #[test]
    fn a_plan_adds_the_managed_options_and_validates() {
        let schema = OptionSchema::load();
        let tmp = tempfile::tempdir().expect("temp dir");
        let paths = paths(tmp.path());
        let config = CrawlConfig::new("https://example.com/");
        let plan = build_plan(&config, &schema, &SecretValues::default(), &paths);
        let dir = &paths.run_dir;
        let expected = [
            format!("--config-file={}", paths.config_file.display()),
            format!("--events-file={}", dir.join(EVENTS_FILE).display()),
            "--control-stdin=true".to_string(),
            "--output=text".to_string(),
            "--force-color=true".to_string(),
            "--console-width=160".to_string(),
            "--hide-progress-bar=true".to_string(),
            format!("--output-json-file={}", dir.join(RESULT_JSON).display()),
            format!("--output-html-report={}", dir.join(REPORT_HTML).display()),
            format!("--output-text-file={}", dir.join(OUTPUT_TEXT).display()),
            format!("--http-cache-dir={}", paths.http_cache_dir.display()),
        ];
        assert_eq!(plan.argv[0], "--url=https://example.com/");
        // The app's default, passed because it is not the CLI's.
        assert_eq!(plan.argv[1], "--timeout=30");
        // Cart and action links are skipped by default.
        let skip = crate::model::config::ACTION_LINK_PATTERNS.len();
        assert!(
            plan.argv[2..2 + skip]
                .iter()
                .all(|arg| arg.starts_with("--ignore-regex:=")),
            "{:?}",
            plan.argv
        );
        // Link details are on by default (an app default too).
        assert_eq!(plan.argv[2 + skip], "--link-details=true");
        assert_eq!(plan.argv[3 + skip..], expected[..]);
        assert!(!has(&plan.argv, "--ai-"), "no AI option without AI");
        assert_eq!(plan.events_file, dir.join(EVENTS_FILE));
        assert_eq!(plan.console_log, dir.join(CONSOLE_LOG));
        assert!(plan.env.0.is_empty());

        plan.validate().expect("valid");
    }

    #[test]
    fn user_set_outputs_and_caches_are_kept() {
        let schema = OptionSchema::load();
        let tmp = tempfile::tempdir().expect("temp dir");
        let mut config = CrawlConfig::new("https://example.com/");
        config
            .set(&schema, "--output-html-report", Value::Text(String::new()))
            .expect("set");
        config
            .set(&schema, "--output-text-file", Value::Text("my.txt".to_string()))
            .expect("set");
        config.set(&schema, "--no-cache", Value::Bool(true)).expect("set");
        let plan = build_plan(&config, &schema, &SecretValues::default(), &paths(tmp.path()));
        assert_eq!(
            plan.argv
                .iter()
                .filter(|arg| arg.starts_with("--output-html-report"))
                .count(),
            1
        );
        assert!(plan.argv.contains(&"--output-html-report=".to_string()));
        assert!(plan.argv.contains(&"--output-text-file=my.txt".to_string()));
        assert!(!has(&plan.argv, "--http-cache-dir"));

        let mut config = CrawlConfig::new("https://example.com/");
        config
            .set(&schema, "--http-cache-dir", Value::Text("off".to_string()))
            .expect("set");
        let plan = build_plan(&config, &schema, &SecretValues::default(), &paths(tmp.path()));
        assert_eq!(
            plan.argv
                .iter()
                .filter(|arg| arg.starts_with("--http-cache-dir"))
                .count(),
            1
        );
    }

    #[test]
    fn the_api_key_goes_through_the_environment_and_never_into_an_argv() {
        let schema = OptionSchema::load();
        let tmp = tempfile::tempdir().expect("temp dir");
        let mut config = CrawlConfig::new("https://user:pw@example.com/");
        config.ai_enabled = true;
        config
            .set(
                &schema,
                "--ai-endpoint",
                Value::Text("http://k:s@127.0.0.1:9/v1".to_string()),
            )
            .expect("set");
        config
            .set(&schema, "--ai-model", Value::Text("m".to_string()))
            .expect("set");
        config
            .set(
                &schema,
                "--header",
                Value::List(vec!["Cookie: session=abc".to_string()]),
            )
            .expect("set");
        let mut secrets = SecretValues::default();
        secrets.set(AI_API_KEY, "sk-secret");
        secrets.set("--http-auth", "user:pass");
        let plan = build_plan(&config, &schema, &secrets, &paths(tmp.path()));

        assert!(plan.argv.contains(&format!("--ai-api-key-env={API_KEY_ENV}")));
        assert!(has(&plan.argv, "--ai-cache-dir="));
        assert_eq!(plan.env.0, vec![(API_KEY_ENV.to_string(), "sk-secret".to_string())]);
        assert_eq!(format!("{:?}", plan.env), format!("[\"{API_KEY_ENV}\"]"));
        assert!(!plan.argv.iter().any(|arg| arg.contains("sk-secret")));
        assert!(plan.argv.contains(&"--http-auth=user:pass".to_string()));

        let redacted = plan.redacted_argv.join(" ");
        for secret in ["sk-secret", "user:pass", "session=abc", ":pw@", "k:s@"] {
            assert!(!redacted.contains(secret), "{secret} in {redacted}");
        }
        assert!(plan.redacted_argv.contains(&format!("--http-auth={MASK}")));
        assert!(plan.redacted_argv.contains(&"--header:=Cookie: ***".to_string()));

        // A key file of the user's wins: no variable, no environment.
        config
            .set(&schema, "--ai-api-key-file", Value::Text("C:\\k.txt".to_string()))
            .expect("set");
        let plan = build_plan(&config, &schema, &secrets, &paths(tmp.path()));
        assert!(!has(&plan.argv, "--ai-api-key-env"));
        assert!(plan.env.0.is_empty());
    }

    #[test]
    fn ai_tool_args_carry_only_the_connection() {
        let schema = OptionSchema::load();
        let mut config = CrawlConfig::new("https://example.com/");
        config
            .set(
                &schema,
                "--ai-endpoint",
                Value::Text("http://127.0.0.1:9/v1".to_string()),
            )
            .expect("set");
        config
            .set(&schema, "--ai-actions", Value::List(vec!["seo".to_string()]))
            .expect("set");
        config
            .set(&schema, "--workers", Value::Text("9".to_string()))
            .expect("set");
        let mut secrets = SecretValues::default();
        secrets.set(AI_API_KEY, "sk-x");
        let config_file = crate::model::test_support::empty_config_file();
        let (args, env) = ai_tool_args(&config, &schema, &secrets, config_file);
        assert_eq!(
            args,
            vec![
                "--ai-provider=openai-compatible".to_string(),
                "--ai-endpoint=http://127.0.0.1:9/v1".to_string(),
                format!("--ai-api-key-env={API_KEY_ENV}"),
                format!("--config-file={}", config_file.display()),
            ]
        );
        assert_eq!(env.0.len(), 1);
    }
}
