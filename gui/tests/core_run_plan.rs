//! What a crawler started by the app gets (GUI-SPEC §4.2, §5.2): relative input paths name the file
//! the app checked although the child runs in its run folder, and a CLI config file of the user
//! (`~/.siteone-crawler.conf`) changes neither a run, nor a retry, nor a tool, nor the validation.

mod core_support;
mod ui_common;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use core_support::{crawler_binary, drive, plan_of, serve, shared_empty_config_file, write_site};
use siteone_crawler::options::core_options;
use siteone_crawler_gui::app::Action;
use siteone_crawler_gui::backend::runner::{self, RunRequest};
use siteone_crawler_gui::model::config::{self, CrawlConfig, Shell, Value};
use siteone_crawler_gui::model::retry::{ChildFiles, crawl_argv, request_options};
use siteone_crawler_gui::model::run_plan::{RunPlan, ai_tool_args};
use siteone_crawler_gui::model::run_state::{RunOutcome, RunState};
use siteone_crawler_gui::model::schema::OptionSchema;
use siteone_crawler_gui::model::secrets::SecretValues;
use siteone_crawler_gui::ui::setup::ops;

const RUN_TIMEOUT: Duration = Duration::from_secs(120);

fn set(config: &mut CrawlConfig, schema: &OptionSchema, name: &str, value: Value) {
    config.set(schema, name, value).expect(name);
}

fn text(value: &str) -> Value {
    Value::Text(value.to_string())
}

/// Runs a plan to its end with `extra_env` added; the final state.
fn run(plan: &RunPlan, extra_env: &[(&str, &Path)]) -> RunState {
    let mut request = RunRequest::from_plan(crawler_binary().to_path_buf(), plan);
    for (name, value) in extra_env {
        request.env.0.push((name.to_string(), value.display().to_string()));
    }
    let mut handle = runner::start(request, || {}).expect("the crawler starts");
    let mut state = RunState::new();
    drive(&mut handle, &mut state, RUN_TIMEOUT, |_, _, _| {});
    state
}

fn html_pages(state: &RunState) -> usize {
    state
        .urls
        .iter()
        .filter(|row| row.url.ends_with(".html") || row.url.ends_with('/'))
        .count()
}

// ---------------------------------------------------------------------------------------------
// Relative input paths (R7)
// ---------------------------------------------------------------------------------------------

#[test]
fn a_relative_input_path_names_the_checked_file_in_the_child_and_outputs_stay_in_the_run_folder() {
    let tmp = tempfile::tempdir().expect("temp dir");
    write_site(&tmp.path().join("site"), 2);
    let server = serve(&tmp.path().join("site"), &tmp.path().join("server"));
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).expect("data folder");
    std::fs::write(data.join("urls.txt"), format!("{}page-1.html\n", server.url())).expect("url list");

    let schema = OptionSchema::load();
    let secrets = SecretValues::default();
    let mut config = CrawlConfig::default();
    set(&mut config, &schema, "--url-list", text("urls.txt"));
    set(&mut config, &schema, "--output-text-file", text("out.txt"));
    set(&mut config, &schema, "--single-page", Value::Bool(true));
    // The tests run in the crate folder, which has no `urls.txt`: unresolved, the path is refused.
    assert!(config.validate(&schema, &secrets, shared_empty_config_file()).is_err());

    let resolved = config.with_absolute_inputs(&schema, &data);
    resolved
        .validate(&schema, &secrets, shared_empty_config_file())
        .expect("valid");
    let plan = plan_of(tmp.path(), &resolved);
    let list = format!("--url-list={}", data.join("urls.txt").display());
    assert!(plan.argv.contains(&list), "{:?}", plan.argv);
    assert!(plan.argv.contains(&"--output-text-file=out.txt".to_string()));
    plan.validate().expect("valid plan");

    let state = run(&plan, &[]);
    assert_eq!(
        state.outcome(),
        RunOutcome::Finished,
        "console: {}",
        state.console.plain_text()
    );
    assert!(
        state.urls.iter().any(|row| row.url.ends_with("/page-1.html")),
        "{:?}",
        state.urls
    );
    assert!(
        plan.run_dir.join("out.txt").is_file(),
        "a relative output lands in the run folder"
    );
}

#[test]
fn relative_inputs_resolve_against_the_data_folder_or_the_folder_of_an_imported_file() {
    let env = ui_common::Env::new();
    let mut app = env.app();
    let schema = app.schema.clone();
    set(&mut app.session.config, &schema, "--url-list", text("lists/urls.txt"));
    set(&mut app.session.config, &schema, "--output-text-file", text("out.txt"));
    let absolute = env.paths().data.join("lists/urls.txt").display().to_string();

    // Setup keeps what was typed; what a crawler gets — and the command shows — is absolute.
    assert_eq!(
        app.session.config.values.get("--url-list"),
        Some(&text("lists/urls.txt"))
    );
    let run_config = app.run_config();
    assert_eq!(run_config.values.get("--url-list"), Some(&text(&absolute)));
    assert_eq!(run_config.values.get("--output-text-file"), Some(&text("out.txt")));
    for shell in [Shell::PowerShell, Shell::Posix] {
        let line = ops::command_text(&app, shell, false);
        let argv = config::split_command_line(&line, shell).expect("splits");
        assert!(argv.contains(&format!("--url-list={absolute}")), "{line}");
        assert!(argv.contains(&"--output-text-file=out.txt".to_string()), "{line}");
    }
    assert!(
        ops::export_text(&app, false)
            .text
            .contains(&format!("--url-list={absolute}"))
    );

    // An imported config file: its relative inputs name files next to it.
    let folder = env.dir.path().join("shared configs");
    std::fs::create_dir_all(&folder).expect("folder");
    let file = folder.join("site.conf");
    std::fs::write(
        &file,
        "--url=https://example.com/\n--url-list=urls.txt\n--ai-prompt-file=prompts/check.md\n--output-text-file=out.txt\n",
    )
    .expect("config file");
    let mut app = env.app_with(Some(env.fake_crawler()), vec![Some(file)]);
    app.perform(Action::ImportConfigFile, None);
    assert!(app.wait_for_background(Duration::from_secs(10)));
    let values = &app.session.config.values;
    assert_eq!(
        values.get("--url-list"),
        Some(&text(&folder.join("urls.txt").display().to_string()))
    );
    assert_eq!(
        values.get("--ai-prompt-file"),
        Some(&text(&folder.join("prompts/check.md").display().to_string()))
    );
    assert_eq!(values.get("--output-text-file"), Some(&text("out.txt")));
}

// ---------------------------------------------------------------------------------------------
// A CLI config file of the user (R8)
// ---------------------------------------------------------------------------------------------

/// A home folder with a CLI config file that would change a crawl.
fn home_with_cli_config() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("home");
    std::fs::write(
        home.path().join(".siteone-crawler.conf"),
        "--single-page=true\n--workers=7\n",
    )
    .expect("home config");
    home
}

#[test]
fn a_home_cli_config_file_does_not_change_a_run_of_the_real_crawler() {
    let tmp = tempfile::tempdir().expect("temp dir");
    write_site(&tmp.path().join("site"), 3);
    let server = serve(&tmp.path().join("site"), &tmp.path().join("server"));
    let home = home_with_cli_config();
    let mut config = CrawlConfig::new(server.url());
    set(&mut config, &OptionSchema::load(), "--max-reqs-per-sec", text("100"));

    let plan = plan_of(tmp.path(), &config);
    let state = run(&plan, &[("HOME", home.path())]);
    assert_eq!(state.outcome(), RunOutcome::Finished, "{}", state.console.plain_text());
    assert!(
        html_pages(&state) >= 4,
        "the home file's --single-page applied: {:?}",
        state.urls
    );

    // The same run without the app's empty config file: the crawler reads the home file.
    let mut plain = plan_of(tmp.path(), &config);
    plain.argv.retain(|arg| !arg.starts_with("--config-file="));
    let state = run(&plain, &[("HOME", home.path())]);
    assert_eq!(html_pages(&state), 1, "control: {:?}", state.urls);
}

/// Runs `test` in a child test process whose `HOME` holds a CLI config file (the environment of
/// this process is never changed). The child sees `SITEONE_GUI_TEST_CHILD`.
fn in_child_with_home_config(test: &str) {
    let home = home_with_cli_config();
    let output = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env("HOME", home.path())
        .env("SITEONE_GUI_TEST_CHILD", "1")
        .output()
        .expect("child test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn parse(argv: &[String]) -> core_options::CoreOptions {
    let mut full = vec![config::PROGRAM_NAME.to_string()];
    full.extend_from_slice(argv);
    core_options::parse_argv(&full).expect("parses")
}

#[test]
fn validation_runs_retries_and_tools_ignore_a_home_cli_config_file() {
    if std::env::var_os("SITEONE_GUI_TEST_CHILD").is_none() {
        in_child_with_home_config("validation_runs_retries_and_tools_ignore_a_home_cli_config_file");
        return;
    }
    let schema = OptionSchema::load();
    let secrets = SecretValues::default();
    let empty = shared_empty_config_file();
    let mut config = CrawlConfig::new("https://example.com/");
    config.ai_enabled = true;
    set(&mut config, &schema, "--ai-endpoint", text("http://127.0.0.1:9/v1"));
    set(&mut config, &schema, "--ai-model", text("m"));
    assert!(!config.values.contains_key("--single-page"));

    // Control: without the app's file, the CLI's parser takes the home file.
    let plain = parse(&config.to_argv(&schema, &secrets));
    assert!(plain.single_page && plain.workers == 7, "the home file was not read");

    config.validate(&schema, &secrets, empty).expect("valid");

    let tmp = tempfile::tempdir().expect("temp dir");
    let plan = plan_of(tmp.path(), &config);
    let run = parse(&plan.argv);
    assert!(!run.single_page, "Setup's default: off");
    assert_eq!(run.workers, 3, "Setup's default");

    let files = ChildFiles {
        events: tmp.path().join("events.ndjson"),
        result: tmp.path().join("result.json"),
        config: empty.to_path_buf(),
    };
    let options = request_options(&plan.argv, "https://example.com/", "https://example.com/a");
    let retry = parse(&crawl_argv(&options, "https://example.com/a", &files, true));
    assert!(!retry.single_page);
    assert_eq!(retry.workers, 3);

    let (args, _) = ai_tool_args(&config, &schema, &secrets, empty);
    for mode in ["--ai-list-models", "--ai-check"] {
        let mut argv = vec![mode.to_string()];
        argv.extend(args.iter().cloned());
        let tool = parse(&argv);
        assert!(!tool.single_page && tool.workers == 3, "{mode}");
    }
}
