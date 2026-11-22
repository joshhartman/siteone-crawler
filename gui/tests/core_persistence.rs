//! The autosave of the working configuration and its passwords (GUI-SPEC §5.6): writes run in
//! order, so an older value never replaces a newer one — also not after the exit flush — and a
//! value counts as saved only when it was written; a failed write is written again.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use siteone_crawler_gui::app::{AUTOSAVE_DELAY, App, AppDeps, CrawlerProbe, NoDialogs};
use siteone_crawler_gui::model::paths::AppPaths;
use siteone_crawler_gui::model::profiles::Session;
use siteone_crawler_gui::model::schema::OptionSchema;
use siteone_crawler_gui::model::secrets::{MemoryStore, SecretError, SecretStore};
use siteone_crawler_gui::model::settings::Settings;
use siteone_crawler_gui::ui::widgets::Tone;

const ACCOUNT: &str = "session/--http-auth";

/// A credential store whose `set` can fail a number of times, or block on one value until
/// released.
#[derive(Default)]
struct TestStore {
    memory: MemoryStore,
    failures: AtomicUsize,
    block_value: Option<&'static str>,
    gate: Arc<Gate>,
}

/// `entered`: a write of the blocked value is waiting; `open`: it may go on.
#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl Gate {
    fn wait_entered(&self) -> bool {
        let state = self.state.lock().expect("gate");
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |(entered, _)| !*entered)
            .expect("gate");
        state.0
    }

    fn open(&self) {
        self.state.lock().expect("gate").1 = true;
        self.changed.notify_all();
    }
}

impl SecretStore for TestStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        self.memory.get(account)
    }

    fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
        if self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
            .is_ok()
        {
            return Err(SecretError("the store is locked (test)".to_string()));
        }
        if self.block_value == Some(value) {
            let mut state = self.gate.state.lock().expect("gate");
            state.0 = true;
            self.gate.changed.notify_all();
            while !state.1 {
                state = self.gate.changed.wait(state).expect("gate");
            }
        }
        self.memory.set(account, value)
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.memory.delete(account)
    }
}

fn app_with(dir: &std::path::Path, store: TestStore) -> App {
    let paths = AppPaths::at(dir);
    paths.ensure().expect("data folder");
    let mut app = App::new(AppDeps {
        paths,
        schema: OptionSchema::load(),
        settings: Settings::default(),
        notices: Vec::new(),
        secret_store: Box::new(store),
        dialogs: Box::new(NoDialogs),
        crawler_probe: CrawlerProbe::Fixed {
            path: None,
            version: None,
        },
    });
    app.fetch_link_texts = false;
    app
}

fn frame(app: &mut App, ctx: &egui::Context) {
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.show(ui));
    output.textures_delta.clear();
}

/// Draws frames until the autosave has started the write of the current values.
fn autosave(app: &mut App, ctx: &egui::Context) {
    frame(app, ctx);
    std::thread::sleep(AUTOSAVE_DELAY + Duration::from_millis(50));
    frame(app, ctx);
}

fn stored(app: &App) -> Option<String> {
    app.secret_store.get(ACCOUNT).expect("get")
}

fn session_url(app: &App) -> String {
    let schema = OptionSchema::load();
    let (loaded, _) = Session::load(&app.paths, &schema);
    loaded.value.config.url
}

fn has_toast(app: &App, tone: Tone, needle: &str) -> bool {
    app.toasts
        .items()
        .iter()
        .any(|toast| toast.tone == tone && toast.text.contains(needle))
}

#[test]
fn a_failed_autosave_stays_unsaved_shows_and_is_written_by_the_exit_flush() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = TestStore {
        failures: AtomicUsize::new(1),
        ..TestStore::default()
    };
    let mut app = app_with(dir.path(), store);
    let ctx = egui::Context::default();
    app.session.config.url = "https://new.example/".to_string();
    app.secrets.set("--http-auth", "new-password");
    autosave(&mut app, &ctx);
    assert!(app.wait_for_background(Duration::from_secs(5)));

    // The file part was written, the password not: the failure is shown, nothing counts as saved.
    assert_eq!(stored(&app), None);
    assert_eq!(session_url(&app), "https://new.example/");
    assert!(
        has_toast(&app, Tone::Danger, "Passwords could not be saved"),
        "{:?}",
        app.toasts.items()
    );
    // No retry storm: further frames before the back-off write nothing.
    for _ in 0..5 {
        frame(&mut app, &ctx);
    }
    assert_eq!(app.pending_tasks(), 0);

    // On exit the unchanged value is written again, both parts.
    app.flush();
    assert_eq!(stored(&app).as_deref(), Some("new-password"));
    assert!(
        has_toast(&app, Tone::Success, "saved again"),
        "{:?}",
        app.toasts.items()
    );
}

#[test]
fn a_failed_settings_write_is_retried_by_the_exit_flush() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut app = app_with(dir.path(), TestStore::default());
    let ctx = egui::Context::default();
    // A folder where the file goes: every write fails until it is gone.
    let file = app.paths.settings_file();
    std::fs::create_dir_all(file.join("blocker")).expect("blocking folder");
    app.settings.ui_scale = 1.25;
    autosave(&mut app, &ctx);
    assert!(app.wait_for_background(Duration::from_secs(5)));
    assert!(
        has_toast(&app, Tone::Danger, "The settings could not be saved"),
        "{:?}",
        app.toasts.items()
    );

    std::fs::remove_dir_all(&file).expect("unblocked");
    app.flush();
    let saved = Settings::load(&app.paths).value;
    assert_eq!(saved.ui_scale, 1.25);
}

#[test]
fn a_newer_autosave_waits_for_an_older_one_and_ends_up_stored() {
    let dir = tempfile::tempdir().expect("temp dir");
    let gate = Arc::new(Gate::default());
    let store = TestStore {
        block_value: Some("old-password"),
        gate: Arc::clone(&gate),
        ..TestStore::default()
    };
    let mut app = app_with(dir.path(), store);
    let ctx = egui::Context::default();
    app.session.config.url = "https://old.example/".to_string();
    app.secrets.set("--http-auth", "old-password");
    autosave(&mut app, &ctx);
    assert!(gate.wait_entered(), "the first write did not start");

    app.session.config.url = "https://new.example/".to_string();
    app.secrets.set("--http-auth", "new-password");
    autosave(&mut app, &ctx);
    // The newer write cannot run ahead of the older one that holds the order.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(stored(&app), None, "the newer write ran ahead");

    gate.open();
    assert!(app.wait_for_background(Duration::from_secs(5)));
    // Without any flush: the newest value is stored, file and password alike.
    assert_eq!(stored(&app).as_deref(), Some("new-password"));
    assert_eq!(session_url(&app), "https://new.example/");
    app.flush();
    assert_eq!(stored(&app).as_deref(), Some("new-password"));
}

#[test]
fn the_exit_flush_waits_for_a_running_write_and_nothing_older_runs_after_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let gate = Arc::new(Gate::default());
    let store = TestStore {
        block_value: Some("old-password"),
        gate: Arc::clone(&gate),
        ..TestStore::default()
    };
    let mut app = app_with(dir.path(), store);
    let ctx = egui::Context::default();
    app.secrets.set("--http-auth", "old-password");
    autosave(&mut app, &ctx);
    assert!(gate.wait_entered(), "the first write did not start");
    // A second write is queued behind it …
    app.secrets.set("--http-auth", "middle-password");
    autosave(&mut app, &ctx);
    // … and the newest value is not autosaved yet when the window closes.
    app.secrets.set("--http-auth", "final-password");
    frame(&mut app, &ctx);

    let opened = Arc::new(AtomicBool::new(false));
    let opener = {
        let (gate, opened) = (Arc::clone(&gate), Arc::clone(&opened));
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            opened.store(true, Ordering::SeqCst);
            gate.open();
        })
    };
    let started = Instant::now();
    app.flush();
    assert!(
        opened.load(Ordering::SeqCst),
        "flush returned after {:?} while a write was running",
        started.elapsed()
    );
    opener.join().expect("opener");
    assert_eq!(stored(&app).as_deref(), Some("final-password"));
    // The queued write and the ends of the older ones change nothing.
    assert!(app.wait_for_background(Duration::from_secs(5)));
    for _ in 0..3 {
        frame(&mut app, &ctx);
    }
    assert_eq!(stored(&app).as_deref(), Some("final-password"));
}
