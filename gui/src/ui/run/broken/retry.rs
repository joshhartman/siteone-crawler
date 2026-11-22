//! The retry parts of the Link checker tab (GUI-SPEC §4.9, §6.3): the queue bar above the views,
//! the quiet marks in the Status cell (queued, being retried, could not be retried), the State cell
//! of the *Retry queue* view, the actions on the selection and the first entries of the row menu.
//! The URLs tab uses the marks and the menu entries too.

use eframe::egui::{self, RichText, Ui};
use egui_material_icons::icons;

use super::{BrokenUi, Context, Scope};
use crate::model::retry::RetryEntry;
use crate::theme;
use crate::ui::run::{RetryView, RunAction, RunView};
use crate::ui::widgets::table::middle_truncate;
use crate::ui::widgets::{self, Tone, format, progress};

/// The mark of a row after its status chip, with a tooltip; nothing for a row that is neither
/// queued nor being retried.
pub fn mark(ui: &mut Ui, retry: &RetryView<'_>, url: &str) {
    let p = theme::palette(ui);
    let icon = |ui: &mut Ui, icon: egui_material_icons::MaterialIcon, color: egui::Color32, tip: &str| {
        ui.label(icon.rich_text().size(15.0).color(color)).on_hover_text(tip);
    };
    if retry.current == Some(url) {
        progress::spinner(ui, 13.0, p.accent).on_hover_text("Being retried");
    } else if retry.state.is_queued(url) {
        match retry.state.entry(url).and_then(|entry| entry.error.as_deref()) {
            Some(error) => icon(
                ui,
                icons::ICON_WARNING,
                p.warning,
                &format!("Could not be retried: {error}\nIt stays in the retry queue."),
            ),
            None => icon(ui, icons::ICON_SCHEDULE, p.text_weak, "In the retry queue"),
        }
    }
}

/// The State cell of the *Retry queue* view.
pub(super) fn state_cell(ui: &mut Ui, retry: &RetryView<'_>, url: &str) {
    let p = theme::palette(ui);
    if retry.current == Some(url) {
        ui.label(RichText::new("Retrying…").color(p.accent));
        return;
    }
    match retry.state.entry(url).and_then(|entry| entry.error.as_deref()) {
        Some(error) => {
            ui.label(RichText::new("Not retried").color(p.warning))
                .on_hover_text(format!("Could not be retried: {error}"));
        }
        None => {
            ui.label(RichText::new("Queued").color(p.text));
        }
    }
}

/// Why crawled URLs cannot be retried (a run from History whose configuration is incomplete).
fn missing_reason(missing: &str) -> String {
    format!("This run's configuration lacks {missing}; only external links can be retried")
}

/// How a pass ended, in words: "3 now OK, 2 still failing". A count of zero is left out; `None`
/// when no URL has an answer.
fn outcome_words(ok: u64, failing: u64) -> Option<String> {
    let mut parts = Vec::new();
    if ok > 0 {
        parts.push(format!("{} now OK", format::count(ok)));
    }
    if failing > 0 {
        parts.push(format!("{} still failing", format::count(failing)));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The queue bar: what is queued, a pass in progress (with Stop), or how the last pass ended.
pub(super) fn queue_bar(ui: &mut Ui, view: &RunView<'_>, actions: &mut Vec<RunAction>) {
    let retry = &view.retry;
    let state = retry.state;
    let p = theme::palette(ui);
    if let Some((done, total)) = retry.running {
        let font = egui::TextStyle::Body.resolve(ui.style());
        let url = middle_truncate(
            ui,
            retry.current.unwrap_or(""),
            &font,
            (ui.available_width() * 0.45).max(160.0),
        );
        let mut message = format!(
            "Retrying {} of {}",
            format::count((done + 1).min(total) as u64),
            format::count(total as u64)
        );
        // Between two requests to one host no URL is in flight.
        if !url.is_empty() {
            message.push_str(" · ");
            message.push_str(&url);
        }
        let stop = widgets::banner(ui, Tone::Info, &message, |ui| {
            let stop = ui
                .add(egui::Button::new("Stop"))
                .accessible_name("Stop retrying")
                .on_hover_text("End the request in flight and the pass; that URL and the rest stay queued")
                .clicked();
            progress::spinner(ui, 16.0, p.accent);
            stop
        });
        ui.add_space(4.0);
        progress::slim_bar(ui, (total > 0).then(|| done as f32 / total as f32), p.accent, p);
        if stop {
            actions.push(RunAction::StopRetry);
        }
        ui.add_space(theme::GRID);
        return;
    }
    let queue = state.queue.len();
    let pass = state.last_pass.as_ref();
    if queue == 0 && pass.is_none() {
        return;
    }
    let mut parts = Vec::new();
    let mut tone = Tone::Info;
    if let Some(pass) = pass {
        let answered = pass.ok + pass.failing;
        let head = if pass.stopped {
            format!(
                "Stopped after {} of {}",
                format::count((answered + pass.not_retried) as u64),
                format::plural(pass.total as u64, "URL")
            )
        } else {
            format!("Retried {}", format::plural(pass.total as u64, "URL"))
        };
        let mut text = match outcome_words(pass.ok as u64, pass.failing as u64) {
            Some(words) => format!("{head}: {words}."),
            None => format!("{head}."),
        };
        if pass.not_retried > 0 {
            text.push_str(&format!(
                " {} could not be retried.",
                format::count(pass.not_retried as u64)
            ));
        }
        if pass.failing > 0 || pass.not_retried > 0 || pass.stopped {
            tone = Tone::Warning;
        } else {
            tone = Tone::Success;
        }
        parts.push(text);
    }
    if queue > 0 {
        parts.push(format!(
            "Retry queue: {} · one request each.",
            format::plural(queue as u64, "URL")
        ));
    }
    let message = parts.join(" ");
    // The queue can only be retried in part when the run's configuration is incomplete.
    let blocker: Option<String> = retry.blocker.map(str::to_string).or_else(|| {
        let missing = retry.missing.as_deref()?;
        let any_external = state
            .queue
            .iter()
            .any(|url| state.entry(url).is_some_and(|entry| !entry.is_crawled()));
        (!any_external).then(|| missing_reason(missing))
    });
    let reasons = pass.map(|pass| pass.reasons.join("\n")).unwrap_or_default();
    let (retry_queue, clear) = widgets::banner(ui, tone, &message, |ui| {
        let mut clear = false;
        let mut retry_queue = false;
        if queue > 0 {
            clear = ui
                .add_enabled(retry.running.is_none(), egui::Button::new("Clear queue"))
                .accessible_name("Clear queue")
                .on_hover_text("Remove every URL from the retry queue (their results stay)")
                .clicked();
            retry_queue = widgets::primary_button(ui, blocker.is_none(), Some(icons::ICON_REPLAY), "Retry queue", 0.0)
                .on_hover_text("Request each queued URL once more, one after another")
                .on_disabled_hover_text(blocker.as_deref().unwrap_or(""))
                .clicked();
        }
        if !reasons.is_empty() {
            ui.label(icons::ICON_INFO.rich_text().size(16.0).color(p.text_weak))
                .on_hover_text(format!("Why URLs could not be retried:\n{reasons}"));
        }
        (retry_queue, clear)
    });
    if retry_queue {
        actions.push(RunAction::RetryQueue);
    }
    if clear {
        actions.push(RunAction::ClearQueue);
    }
    ui.add_space(theme::GRID);
}

/// What can be done with the selected rows, right to left after Link report: the queue action,
/// *Retry now* (primary), clear the selection and the count.
pub(super) fn selection_actions(ui: &mut Ui, view: &RunView<'_>, broken: &mut BrokenUi, actions: &mut Vec<RunAction>) {
    let count = broken.selection.len();
    let retry = &view.retry;
    let queue_view = broken.filter.scope == Scope::RetryQueue;
    let selected = |broken: &BrokenUi| {
        let context: Context<'_> = broken.context(view);
        broken.selection.in_view_order(&broken.view, &context)
    };
    if queue_view {
        if widgets::button(ui, Some(icons::ICON_PLAYLIST_REMOVE), "Remove from queue")
            .on_hover_text("Take the selected URLs out of the retry queue")
            .clicked()
        {
            actions.push(RunAction::Dequeue(selected(broken)));
        }
    } else if widgets::button(ui, Some(icons::ICON_PLAYLIST_ADD), "Add to queue")
        .on_hover_text("Put the selected URLs in the retry queue, to be retried together later")
        .clicked()
    {
        actions.push(RunAction::Enqueue(selected(broken)));
    }
    let blocker: Option<String> = retry.blocker.map(str::to_string).or_else(|| {
        let missing = retry.missing.as_deref()?;
        let any_external = view
            .links
            .results
            .iter()
            .any(|link| broken.selection.contains(&link.url));
        (!any_external).then(|| missing_reason(missing))
    });
    if widgets::primary_button(ui, blocker.is_none(), Some(icons::ICON_REPLAY), "Retry now", 0.0)
        .on_hover_text("Request each selected URL once more now, one after another")
        .on_disabled_hover_text(blocker.as_deref().unwrap_or(""))
        .clicked()
    {
        actions.push(RunAction::RetryNow(selected(broken)));
    }
    if widgets::icon_button(ui, icons::ICON_DESELECT, "Clear selection").clicked() {
        broken.selection.clear();
    }
    ui.label(RichText::new(format!("{} selected", format::count(count as u64))).color(theme::palette(ui).text));
}

/// The first entries of a row menu: retry the row (or the selection it is part of) and queue it or
/// take it out of the queue. `selected` gives the selection in view order (empty on the URLs tab).
pub fn menu_entries(
    ui: &mut Ui,
    retry: &RetryView<'_>,
    url: &str,
    selected: &dyn Fn() -> Vec<String>,
    in_selection: usize,
    actions: &mut Vec<RunAction>,
) {
    let many = in_selection > 1;
    let urls = || if many { selected() } else { vec![url.to_string()] };
    let blocker: Option<String> = retry.blocker.map(str::to_string).or_else(|| {
        let missing = retry.missing.as_deref()?;
        let crawled = retry.state.entry(url).is_none_or(RetryEntry::is_crawled);
        (crawled && !many).then(|| missing_reason(missing))
    });
    let label = if many {
        format!("Retry {} selected now", format::count(in_selection as u64))
    } else {
        "Retry now".to_string()
    };
    let response = ui
        .add_enabled(
            blocker.is_none(),
            egui::Button::new(widgets::with_icon(icons::ICON_REPLAY, &label)),
        )
        .accessible_name(&label)
        .on_disabled_hover_text(blocker.as_deref().unwrap_or(""));
    if response.clicked() {
        actions.push(RunAction::RetryNow(urls()));
        ui.close();
    }
    let queued = retry.state.is_queued(url);
    let (label, icon) = match (queued, many) {
        (true, false) => ("Remove from retry queue".to_string(), icons::ICON_PLAYLIST_REMOVE),
        (true, true) => (
            format!(
                "Remove {} selected from retry queue",
                format::count(in_selection as u64)
            ),
            icons::ICON_PLAYLIST_REMOVE,
        ),
        (false, false) => ("Add to retry queue".to_string(), icons::ICON_PLAYLIST_ADD),
        (false, true) => (
            format!("Add {} selected to retry queue", format::count(in_selection as u64)),
            icons::ICON_PLAYLIST_ADD,
        ),
    };
    if widgets::button(ui, Some(icon), &label).clicked() {
        actions.push(if queued {
            RunAction::Dequeue(urls())
        } else {
            RunAction::Enqueue(urls())
        });
        ui.close();
    }
    ui.separator();
}
