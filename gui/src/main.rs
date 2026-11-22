// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use eframe::egui;
use siteone_crawler_gui::app::{App, AppDeps};
use siteone_crawler_gui::model::paths::AppPaths;
use siteone_crawler_gui::model::schema::OptionSchema;
use siteone_crawler_gui::model::secrets::KeyringStore;

const TITLE: &str = "SiteOne Crawler";

fn main() -> ExitCode {
    let Some(paths) = AppPaths::discover() else {
        fatal("Windows reports no folder for application data. Set SITEONE_GUI_DATA_DIR to a writable folder.");
        return ExitCode::FAILURE;
    };
    if let Err(error) = paths.ensure() {
        fatal(&format!(
            "The data folder {} cannot be created: {error}",
            paths.data.display()
        ));
        return ExitCode::FAILURE;
    }
    install_panic_hook(paths.crash_log());
    // `get_options()` creates `./tmp`: it must land in the data folder (GUI-SPEC §2).
    if let Err(error) = std::env::set_current_dir(&paths.data) {
        fatal(&format!(
            "The data folder {} cannot be used: {error}",
            paths.data.display()
        ));
        return ExitCode::FAILURE;
    }

    let schema = OptionSchema::load();
    let deps = AppDeps::load(paths, schema, Box::new(KeyringStore));

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(TITLE)
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([1000.0, 640.0]);
    if let Some(window) = deps.settings.window {
        viewport = viewport
            .with_position([window.x, window.y])
            .with_inner_size([window.width, window.height]);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let result = eframe::run_native(
        TITLE,
        options,
        Box::new(move |cc| {
            siteone_crawler_gui::theme::install_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new(deps)))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            fatal(&format!("The window could not be opened: {error}"));
            ExitCode::FAILURE
        }
    }
}

/// Shows a native error message box.
fn fatal(message: &str) {
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(TITLE)
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// On a panic: append the message, location and backtrace to `crash.log`, then show a message box
/// (GUI-SPEC §7.1). Secrets never reach a panic message: the model never formats them. A panic in
/// a background task or a link-check worker is caught and the app goes on (it shows a toast), so it
/// is only logged: a box saying the app stopped would be wrong.
fn install_panic_hook(crash_log: PathBuf) {
    std::panic::set_hook(Box::new(move |info| {
        let caught = siteone_crawler_gui::backend::panic_is_caught();
        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = format!(
            "[{}] SiteOne Crawler GUI {} panicked{}: {info}\n{backtrace}\n",
            chrono::Local::now().to_rfc3339(),
            env!("CARGO_PKG_VERSION"),
            if caught { " (caught; the app went on)" } else { "" },
        );
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crash_log)
            .and_then(|mut file| file.write_all(report.as_bytes()))
            .is_ok();
        if caught {
            return;
        }
        let location = if written {
            format!("Details were written to {}.", crash_log.display())
        } else {
            "The details could not be written to the crash log.".to_string()
        };
        fatal(&format!(
            "SiteOne Crawler GUI stopped because of an internal error.\n\n{location}"
        ));
    }));
}
