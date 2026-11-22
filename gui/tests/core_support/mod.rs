//! Helpers of the backend integration tests (`core_*.rs`): the real crawler binary, a generated
//! site served by the crawler's own `--serve-offline`, run plans in temp dirs, and driving a run
//! to its end. No network: everything listens on 127.0.0.1.

#![allow(dead_code)]

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use siteone_crawler::options::option_type::OptionType;
use siteone_crawler_gui::backend::runner::{self, RunHandle, RunMsg, RunRequest};
use siteone_crawler_gui::model::config::{CrawlConfig, Value};
use siteone_crawler_gui::model::paths::{create_run_dir, run_id_base};
use siteone_crawler_gui::model::run_plan::{ChildEnv, RunPaths, RunPlan, build_plan};
use siteone_crawler_gui::model::run_state::RunState;
use siteone_crawler_gui::model::schema::OptionSchema;
use siteone_crawler_gui::model::secrets::SecretValues;

/// The crawler binary of this build profile, built once per test binary with
/// `$CARGO build -p siteone-crawler` (a no-op when it is up to date).
pub fn crawler_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        // `<target>/<profile>/deps/<test>.exe` → `<target>/<profile>`.
        let exe = std::env::current_exe().expect("test executable");
        let profile_dir = exe
            .parent()
            .and_then(Path::parent)
            .expect("profile directory")
            .to_path_buf();
        let profile = profile_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("debug")
            .to_string();
        let cargo = std::env::var_os("CARGO")
            .or_else(|| option_env!("CARGO").map(Into::into))
            .unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command
            .args(["build", "--quiet", "-p", "siteone-crawler", "--bin", "siteone-crawler"])
            .current_dir(env!("CARGO_MANIFEST_DIR"));
        match profile.as_str() {
            "debug" => {}
            "release" => {
                command.arg("--release");
            }
            other => {
                command.args(["--profile", other]);
            }
        }
        let status = command.status().expect("cargo build of the crawler");
        assert!(status.success(), "building the crawler failed: {status}");
        let binary = profile_dir.join(format!("siteone-crawler{}", std::env::consts::EXE_SUFFIX));
        assert!(binary.is_file(), "no crawler binary at {}", binary.display());
        binary
    })
}

/// An empty config file, so that a `~/.siteone-crawler.conf` of the machine is not merged.
pub fn empty_config_file(dir: &Path) -> PathBuf {
    let file = dir.join("empty.conf");
    std::fs::write(&file, "").expect("empty config file");
    file
}

/// An empty config file that lasts as long as the test process.
pub fn shared_empty_config_file() -> &'static Path {
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

/// Writes a site: `index.html` linking to `page-1.html` … `page-<pages>.html` (each linking to
/// the next one) and a shared `style.css`.
pub fn write_site(dir: &Path, pages: usize) {
    std::fs::create_dir_all(dir).expect("site dir");
    let page = |title: &str, body: &str| {
        format!(
            "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title}</title>\
             <meta name=\"description\" content=\"{title} of the test site\">\
             <link rel=\"stylesheet\" href=\"/style.css\"></head>\
             <body><header><nav><a href=\"/\">Home</a></nav></header><main><h1>{title}</h1>{body}</main></body></html>\n"
        )
    };
    let links: String = (1..=pages)
        .map(|n| format!("<li><a href=\"/page-{n}.html\">Page {n}</a></li>"))
        .collect();
    std::fs::write(dir.join("index.html"), page("Home", &format!("<ul>{links}</ul>"))).expect("index");
    for n in 1..=pages {
        let next = if n < pages {
            format!("<p><a href=\"/page-{}.html\">Next</a></p>", n + 1)
        } else {
            String::new()
        };
        std::fs::write(
            dir.join(format!("page-{n}.html")),
            page(&format!("Page {n}"), &format!("<p>Text of page {n}.</p>{next}")),
        )
        .expect("page");
    }
    std::fs::write(dir.join("style.css"), "body { font-family: sans-serif; }\n").expect("css");
}

/// The crawler's `--serve-offline` on a free local port; killed when dropped (also on a panic).
pub struct Server {
    handle: RunHandle,
    pub port: u16,
}

impl Server {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("address").port()
}

/// Serves `site` (the server's working directory is `work`, where it may create `tmp/`).
pub fn serve(site: &Path, work: &Path) -> Server {
    std::fs::create_dir_all(work).expect("server dir");
    let config = empty_config_file(work);
    for _ in 0..5 {
        let port = free_port();
        let request = RunRequest {
            binary: crawler_binary().to_path_buf(),
            argv: vec![
                format!("--serve-offline={}", site.display()),
                format!("--serve-port={port}"),
                "--serve-bind-address=127.0.0.1".to_string(),
                format!("--config-file={}", config.display()),
            ],
            env: ChildEnv::default(),
            run_dir: work.to_path_buf(),
            events_file: work.join("no-events.ndjson"),
            console_log: work.join("server.log"),
        };
        let server = Server {
            handle: runner::start(request, || {}).expect("server starts"),
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while server.handle.is_running() && Instant::now() < deadline {
            if TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200)).is_ok() {
                return server;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // The port was taken in between, or the server did not come up: try another one.
    }
    panic!("the --serve-offline server did not start");
}

/// The plan of a run of `url` with `overrides`, everything (run dir, caches) inside `root`.
pub fn plan(root: &Path, url: &str, overrides: &[(&str, &str)]) -> RunPlan {
    let schema = OptionSchema::load();
    let mut config = CrawlConfig::new(url);
    for (name, value) in overrides {
        let is_bool = schema.get(name).is_some_and(|spec| spec.kind == OptionType::Bool);
        let value = match is_bool {
            true => Value::Bool(*value == "true"),
            false => Value::Text((*value).to_string()),
        };
        config.set(&schema, name, value).expect("override");
    }
    plan_of(root, &config)
}

/// The plan of a run of `config`, everything (run dir, caches) inside `root`.
pub fn plan_of(root: &Path, config: &CrawlConfig) -> RunPlan {
    let schema = OptionSchema::load();
    let url = config.url.as_str();
    let (run_id, run_dir) =
        create_run_dir(&root.join("runs"), &run_id_base(&chrono::Local::now(), url)).expect("run dir");
    let paths = RunPaths {
        run_id,
        run_dir,
        http_cache_dir: root.join("cache").join("http"),
        ai_cache_dir: root.join("cache").join("ai"),
        config_file: empty_config_file(root),
    };
    build_plan(config, &schema, &SecretValues::default(), &paths)
}

/// A waker that counts its calls.
pub fn counting_waker() -> (Arc<AtomicUsize>, impl Fn() + Send + Sync + 'static) {
    let count = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&count);
    (count, move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })
}

/// Receives and applies messages until [`RunMsg::Exited`]; `react` sees each message first (and
/// may stop or kill the run). Returns the messages. Panics after `timeout`.
pub fn drive(
    handle: &mut RunHandle,
    state: &mut RunState,
    timeout: Duration,
    mut react: impl FnMut(&RunMsg, &mut RunHandle, &mut RunState),
) -> Vec<RunMsg> {
    let deadline = Instant::now() + timeout;
    let mut messages = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let msg = handle
            .receiver()
            .recv_timeout(remaining)
            .unwrap_or_else(|error| panic!("no Exited within {timeout:?} ({error}); state: {state:?}"));
        react(&msg, handle, state);
        let exited = matches!(msg, RunMsg::Exited { .. });
        messages.push(msg.clone());
        msg.apply_to(state, Instant::now());
        if exited {
            return messages;
        }
    }
}

/// The events among `messages`, in order.
pub fn events(messages: &[RunMsg]) -> Vec<&siteone_crawler_gui::model::events::Event> {
    messages
        .iter()
        .filter_map(|msg| match msg {
            RunMsg::Event(event) => Some(event),
            _ => None,
        })
        .collect()
}

/// Whether a process with this id exists (Windows: `tasklist`).
pub fn process_exists(pid: u32) -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .expect("tasklist");
    String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
}

/// Waits until the process with this id is gone; false after `timeout`.
pub fn wait_until_gone(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}
