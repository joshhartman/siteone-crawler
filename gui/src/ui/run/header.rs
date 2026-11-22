//! The header of the Run page: the URL, the outcome chip and the elapsed time with the primary
//! action on the right (Stop → Force quit while running, Stop check while the external links are
//! checked, Open HTML report when finished) and Open folder / Run again; a banner that explains a
//! failure and what to do next (GUI-SPEC §4.5, §7.6); the phase stepper with a slim progress bar.

use eframe::egui::{self, Align, FontId, Layout, Rect, Sense, Ui, UiBuilder};
use egui_material_icons::icons;

use super::{CheckStatus, RunAction, RunView, Tab, outcome_style};
use crate::model::run_state::{AiTaskState, PhaseStatus, RunOutcome};
use crate::theme;
use crate::ui::widgets::stepper::{self, Step, StepState};
use crate::ui::widgets::table::middle_truncate;
use crate::ui::widgets::{self, Tone, format, progress};

const TITLE_SIZE: f32 = 20.0;
const ROW_HEIGHT: f32 = 34.0;

pub fn show(ui: &mut Ui, view: &RunView<'_>, actions: &mut Vec<RunAction>) {
    title_row(ui, view, actions);
    meta_line(ui, view, actions);
    if let Some((tone, message)) = outcome_banner(view) {
        ui.add_space(theme::GRID * 1.5);
        let show_console = widgets::banner(ui, tone, &message, |ui| {
            widgets::link_button(ui, "Show console").clicked()
        });
        if show_console {
            actions.push(RunAction::ShowTab(Tab::Console));
        }
    }
    ui.add_space(theme::GRID * 1.5);
    progress_card(ui, view);
}

fn title_row(ui: &mut Ui, view: &RunView<'_>, actions: &mut Vec<RunAction>) {
    let p = theme::palette(ui);
    let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), Sense::hover());

    // The actions first, from the right: the title takes the room they leave.
    let mut right = ui.new_child(
        UiBuilder::new()
            .max_rect(row)
            .layout(Layout::right_to_left(Align::Center)),
    );
    right.spacing_mut().item_spacing.x = theme::GRID;
    if view.live {
        if view.state.stop_requested {
            if widgets::danger_fill_button(&mut right, Some(icons::ICON_CANCEL), "Force quit", ROW_HEIGHT)
                .on_hover_text("End the crawler at once (asks first)")
                .clicked()
            {
                actions.push(RunAction::ForceQuit);
            }
        } else if widgets::primary_button(&mut right, true, Some(icons::ICON_STOP), "Stop", ROW_HEIGHT)
            .on_hover_text("Stop crawling; what was crawled is still analysed and reported")
            .clicked()
        {
            actions.push(RunAction::Stop);
        }
    } else {
        let checking = matches!(view.links.status, CheckStatus::Running { .. });
        if checking
            && widgets::primary_button(&mut right, true, Some(icons::ICON_STOP), "Stop check", ROW_HEIGHT)
                .on_hover_text("Stop checking the external links; what was checked is kept")
                .clicked()
        {
            actions.push(RunAction::StopCheck);
        }
        let report = view.html_report().map(std::path::Path::to_path_buf);
        let response = if checking {
            // The check is the primary action while it runs; the report stays at hand.
            right
                .add_enabled(
                    report.is_some(),
                    egui::Button::new(widgets::with_icon(icons::ICON_OPEN_IN_NEW, "Open HTML report"))
                        .min_size(egui::vec2(0.0, ROW_HEIGHT)),
                )
                .accessible_name("Open HTML report")
        } else {
            widgets::primary_button(
                &mut right,
                report.is_some(),
                Some(icons::ICON_OPEN_IN_NEW),
                "Open HTML report",
                ROW_HEIGHT,
            )
        }
        .on_disabled_hover_text("This run wrote no HTML report");
        if response.clicked()
            && let Some(report) = report
        {
            actions.push(RunAction::OpenPath(report));
        }
    }
    let secondary = |ui: &mut Ui, icon, label: &str, tip: &str| {
        ui.add(egui::Button::new(widgets::with_icon(icon, label)).min_size(egui::vec2(0.0, ROW_HEIGHT)))
            .accessible_name(label)
            .on_hover_text(tip)
            .clicked()
    };
    // Not offered while the crawl runs: nothing can be started then.
    if !view.live
        && secondary(
            &mut right,
            icons::ICON_REPLAY,
            "Run again",
            "Load this run's configuration into Setup",
        )
    {
        actions.push(RunAction::RunAgain);
    }
    if secondary(
        &mut right,
        icons::ICON_FOLDER_OPEN,
        "Open folder",
        "Open the run folder with every file of this run",
    ) {
        actions.push(RunAction::OpenPath(view.run_dir.to_path_buf()));
    }
    let actions_width = right.min_rect().width();

    let title_rect = Rect::from_min_max(row.min, egui::pos2(row.right() - actions_width - 24.0, row.bottom()));
    let mut left = ui.new_child(
        UiBuilder::new()
            .max_rect(title_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    left.spacing_mut().item_spacing.x = 10.0;
    let (tone, icon) = outcome_style(&view.outcome);
    let label = view.outcome.label();
    // A running clock ticks in whole seconds.
    let elapsed = if view.live {
        match view.elapsed.as_secs() {
            seconds @ 0..60 => format!("{seconds} s"),
            seconds => format::millis(seconds * 1000),
        }
    } else {
        format::duration(view.elapsed)
    };
    // Room for the chip and the elapsed time after the URL.
    let chip_width = left
        .painter()
        .layout_no_wrap(label.to_string(), theme::semibold_font(12.5), p.text)
        .size()
        .x
        + 36.0;
    let elapsed_width = left
        .painter()
        .layout_no_wrap(elapsed.clone(), FontId::proportional(13.5), p.text)
        .size()
        .x
        + 22.0;
    let url_width = (title_rect.width() - chip_width - elapsed_width - 30.0).max(80.0);
    let font = theme::semibold_font(TITLE_SIZE);
    let url = if view.url.is_empty() { "(no URL)" } else { view.url };
    let shown = middle_truncate(&left, url, &font, url_width);
    let response = left.add(egui::Label::new(theme::semibold(&shown).size(TITLE_SIZE).color(p.text)));
    if shown != url {
        response.on_hover_text(url);
    }
    widgets::icon_chip(&mut left, icon, label, tone, view.live);
    left.spacing_mut().item_spacing.x = 4.0;
    left.label(icons::ICON_SCHEDULE.rich_text().size(16.0).color(p.text_weak));
    left.label(egui::RichText::new(elapsed).size(13.5).color(p.text_weak))
        .on_hover_text(if view.live { "Elapsed time" } else { "Duration" });
}

fn meta_line(ui: &mut Ui, view: &RunView<'_>, actions: &mut Vec<RunAction>) {
    let mut parts = Vec::new();
    if let Some(started) = &view.started_at {
        parts.push(format!("Started {started}"));
    }
    if let Some(profile) = view.profile {
        parts.push(format!("Profile “{profile}”"));
    }
    if let Some(version) = view
        .state
        .started
        .as_ref()
        .map(|started| started.version.as_str())
        .filter(|version| !version.is_empty())
    {
        parts.push(format!("Crawler {version}"));
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        if !parts.is_empty() {
            ui.label(widgets::weak(ui, parts.join("   ·   ")).size(12.5));
        }
        if view.current_elsewhere {
            ui.label(widgets::weak(ui, "·"));
            if widgets::link_button(ui, "Back to the current run")
                .on_hover_text("This is a run from History; show the run started in this window")
                .clicked()
            {
                actions.push(RunAction::ShowCurrent);
            }
        }
    });
}

/// What went wrong and what to do next, for the outcomes that need it.
fn outcome_banner(view: &RunView<'_>) -> Option<(Tone, String)> {
    let message = match &view.outcome {
        RunOutcome::Failed { error } => (
            Tone::Danger,
            format!(
                "The crawl failed: {}. The console shows what happened; change the settings in Setup and use Run again.",
                error
                    .as_deref()
                    .map(|error| error.trim_end_matches('.'))
                    .unwrap_or("the crawler gave no reason")
            ),
        ),
        // -4: the request was sent and the server closed the connection without an answer. Hosts
        // with bot protection do that for the crawler's default User-Agent, so say what helps.
        RunOutcome::NoPagesCrawled if view.state.urls.first().is_some_and(|row| row.status == -4) => (
            Tone::Danger,
            "No page could be crawled: the server closed the connection without answering. Some hosts do this for the crawler's default User-Agent — set a current browser User-Agent in Setup (All options → --user-agent) and use Run again. If that does not help, check the address and that this computer can reach it.".to_string(),
        ),
        RunOutcome::NoPagesCrawled => (
            Tone::Danger,
            "No page could be crawled. Check that the address is right and that this computer can reach it (DNS, firewall, VPN, proxy), or raise the request timeout — then use Run again.".to_string(),
        ),
        RunOutcome::Crashed { exit_code } => (
            Tone::Danger,
            format!(
                "The crawler ended unexpectedly{} before it finished. Its last output is in the Console tab. Use Run again; if it happens again, keep console.log from the run folder for a bug report.",
                exit_code.map(exit_code_text).unwrap_or_default()
            ),
        ),
        RunOutcome::Aborted if view.unfinished => (
            Tone::Warning,
            "This run did not finish: the app was closed while it ran. What it crawled until then is shown; nothing was analysed.".to_string(),
        ),
        RunOutcome::Aborted => (
            Tone::Warning,
            "The crawl was force-quit. What it crawled until then is shown; nothing was analysed.".to_string(),
        ),
        RunOutcome::Stopped => (
            Tone::Info,
            "Stopped on request. The results cover the pages crawled until then.".to_string(),
        ),
        RunOutcome::QualityGateFailed => (
            Tone::Warning,
            "The quality gate failed: at least one --ci threshold was not met. The findings and the console say which.".to_string(),
        ),
        _ => {
            return view.state.newer_protocol.map(|protocol| {
                (
                    Tone::Info,
                    format!(
                        "This crawler speaks a newer event protocol ({protocol}) than this app knows; some live details may be missing."
                    ),
                )
            });
        }
    };
    Some(message)
}

/// " (exit code N)", with the Windows status code in hex for large negative codes.
fn exit_code_text(code: i32) -> String {
    if code < -0xFFFF {
        format!(" (exit code {code}, 0x{:08X})", code as u32)
    } else {
        format!(" (exit code {code})")
    }
}

fn progress_card(ui: &mut Ui, view: &RunView<'_>) {
    let p = theme::palette(ui);
    let steps = steps(view);
    let bar = match view.links.status {
        CheckStatus::Running { done, total } if !view.live => Some((total > 0).then(|| done as f32 / total as f32)),
        _ => view.live.then(|| bar_fraction(view)),
    };
    egui::Frame::new()
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(theme::CARD_RADIUS)
        .inner_margin(egui::Margin {
            left: 16,
            right: 16,
            top: 12,
            bottom: if bar.is_some() { 12 } else { 8 },
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            stepper::stepper(ui, &steps);
            if let Some(fraction) = bar {
                ui.add_space(6.0);
                progress::slim_bar(ui, fraction, p.accent, p);
            }
        });
}

/// The progress bar's fill: the crawl's `done / total`, the AI tasks' progress, or indeterminate.
fn bar_fraction(view: &RunView<'_>) -> Option<f32> {
    let state = view.state;
    let phases = state.phases;
    if phases.analysis == PhaseStatus::Running {
        return None;
    }
    if phases.ai == PhaseStatus::Running {
        let (done, total) = state
            .ai_tasks
            .iter()
            .filter(|task| task.state != AiTaskState::Finished)
            .fold((0u64, 0u64), |(done, total), task| {
                (done + task.done, total + task.total)
            });
        return (total > 0).then(|| done as f32 / total as f32);
    }
    if matches!(phases.crawl, PhaseStatus::Running | PhaseStatus::NotStarted) && state.total > 0 {
        return Some(state.done as f32 / state.total as f32);
    }
    None
}

/// The External links step: the app's check after the crawl (GUI-SPEC §4.7).
fn links_step(view: &RunView<'_>) -> Step {
    let links = &view.links;
    let (state, detail) = match &links.status {
        CheckStatus::Running { done, total } => (
            StepState::Active,
            format!(
                "{} / {} links",
                format::count(*done as u64),
                format::count(*total as u64)
            ),
        ),
        CheckStatus::Waiting { another: true } => (StepState::Pending, "After the other check".to_string()),
        CheckStatus::Waiting { .. } if view.live => (StepState::Pending, "After the crawl".to_string()),
        CheckStatus::Waiting { .. } => (StepState::Pending, "Starting…".to_string()),
        CheckStatus::Done => {
            let counts = links.report.map(|report| report.counts()).unwrap_or_default();
            let flagged = counts.refused + counts.broken;
            let detail = match (counts.total(), flagged) {
                (0, _) => "No external links".to_string(),
                (total, 0) => format!("{} · all OK", format::plural(total as u64, "link")),
                (total, flagged) => format!(
                    "{} · {} flagged",
                    format::plural(total as u64, "link"),
                    format::count(flagged as u64)
                ),
            };
            (StepState::Done, detail)
        }
        CheckStatus::Stopped => {
            let total = links.report.map_or(0, |report| report.total);
            (
                StepState::Warning,
                format!(
                    "Stopped at {} / {}",
                    format::count(links.results.len() as u64),
                    format::count(total as u64)
                ),
            )
        }
        CheckStatus::Failed(_) => (StepState::Failed, "Not checked".to_string()),
        CheckStatus::NotRequested => (StepState::Pending, "Not checked".to_string()),
    };
    Step::new("External links", state, detail)
}

fn duration_detail(status: PhaseStatus) -> String {
    match status {
        PhaseStatus::Finished { ms: Some(ms) } => format::millis(ms),
        _ => String::new(),
    }
}

/// The steps Crawl → AI (only with AI) → Analysis → Done.
fn steps(view: &RunView<'_>) -> Vec<Step> {
    let state = view.state;
    let phases = state.phases;
    let ended = !view.live;
    // A phase still "running" when the run has ended was cut short.
    let step_state = |status: PhaseStatus| match status {
        PhaseStatus::NotStarted => StepState::Pending,
        PhaseStatus::Running if ended => StepState::Warning,
        PhaseStatus::Running => StepState::Active,
        PhaseStatus::Finished { .. } => StepState::Done,
        PhaseStatus::Failed => StepState::Failed,
    };
    let urls = format!("{} / {} URLs", format::count(state.done), format::count(state.total));
    let mut steps = Vec::new();

    let starting = view.live
        && phases.crawl == PhaseStatus::NotStarted
        && phases.ai == PhaseStatus::NotStarted
        && phases.analysis == PhaseStatus::NotStarted;
    let crawl = if starting {
        Step::new(
            "Crawl",
            StepState::Active,
            if state.done > 0 {
                urls
            } else {
                "Starting…".to_string()
            },
        )
    } else {
        let detail = match phases.crawl {
            PhaseStatus::Running if !ended => urls,
            PhaseStatus::Running => format!("Interrupted at {urls}"),
            PhaseStatus::Finished { ms } => {
                let count = format::plural(state.urls.len() as u64, "URL");
                match ms {
                    Some(ms) => format!("{count} · {}", format::millis(ms)),
                    None => count,
                }
            }
            PhaseStatus::Failed => "Failed".to_string(),
            PhaseStatus::NotStarted if ended && !state.urls.is_empty() => {
                format::plural(state.urls.len() as u64, "URL")
            }
            PhaseStatus::NotStarted => String::new(),
        };
        let mut crawl_state = step_state(phases.crawl);
        if phases.crawl == PhaseStatus::NotStarted && ended && !state.urls.is_empty() {
            crawl_state = StepState::Warning;
        }
        Step::new("Crawl", crawl_state, detail)
    };
    steps.push(crawl);

    if state.uses_ai() {
        let detail = match phases.ai {
            PhaseStatus::Running if !ended => state
                .ai_tasks
                .iter()
                .find(|task| task.state != AiTaskState::Finished)
                .map(|task| {
                    let label = if task.label.is_empty() { &task.key } else { &task.label };
                    format!("{label} {}/{}", format::count(task.done), format::count(task.total))
                })
                .unwrap_or_else(|| "Working…".to_string()),
            PhaseStatus::Running => "Interrupted".to_string(),
            PhaseStatus::Failed => "Failed".to_string(),
            status => duration_detail(status),
        };
        steps.push(Step::new("AI", step_state(phases.ai), detail));
    }

    let detail = match phases.analysis {
        PhaseStatus::Running if !ended => "Analysing the pages…".to_string(),
        PhaseStatus::Running => "Interrupted".to_string(),
        PhaseStatus::Failed => "Failed".to_string(),
        status => duration_detail(status),
    };
    steps.push(Step::new("Analysis", step_state(phases.analysis), detail));
    if view.links.shows_step() {
        steps.push(links_step(view));
    }

    let done = if view.live {
        Step::new("Done", StepState::Pending, "")
    } else {
        let total = format::duration(view.elapsed);
        let (state, detail) = match &view.outcome {
            RunOutcome::Finished => (StepState::Done, format!("Finished in {total}")),
            RunOutcome::Stopped => (StepState::Warning, format!("Stopped after {total}")),
            RunOutcome::QualityGateFailed => (StepState::Warning, "Quality gate failed".to_string()),
            RunOutcome::Aborted => (StepState::Warning, format!("Aborted after {total}")),
            other => (StepState::Failed, other.label().to_string()),
        };
        Step::new("Done", state, detail)
    };
    steps.push(done);
    steps
}
