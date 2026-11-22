//! The selection of the Link checker tab: a checkbox per row, a header checkbox for every shown row
//! (three states) and Shift+click to extend a range from the last checkbox clicked.
//!
//! The selection is a set of URLs, so a frame only looks rows up (nothing is allocated per row), and
//! a URL listed under several pages of the *by page* layout is selected under each. It never holds
//! a row that is not shown: a change of the view or the filter clears it, and a rebuild of the rows
//! (a retry changed them) drops what left the view. Switching between *by link* and *by page* keeps
//! it: both show the same URLs.

use std::collections::HashSet;

use eframe::egui::{self, Ui};

use super::{BrokenFilter, BrokenView, Context};

#[derive(Debug, Default)]
pub struct Selection {
    urls: HashSet<String>,
    /// The position in the view of the checkbox clicked last (the start of a Shift range).
    anchor: Option<usize>,
    /// The filter and the rebuild of the view the selection belongs to.
    filter: Option<BrokenFilter>,
    rebuilds: u64,
}

/// A click on a checkbox, applied after the table is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Click {
    /// The header checkbox: select every shown row (`true`) or none.
    All(bool),
    /// The checkbox of the row at this position in the view; with Shift.
    Row(usize, bool),
}

impl Selection {
    pub fn len(&self) -> usize {
        self.urls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.urls.is_empty()
    }

    pub fn contains(&self, url: &str) -> bool {
        self.urls.contains(url)
    }

    pub fn clear(&mut self) {
        self.urls.clear();
        self.anchor = None;
    }

    /// The selected URLs in the order of the view.
    pub(super) fn in_view_order(&self, view: &BrokenView, context: &Context<'_>) -> Vec<String> {
        if self.urls.is_empty() {
            return Vec::new();
        }
        view.rows()
            .iter()
            .filter_map(|row| context.item(*row))
            .map(|item| item.url().to_string())
            .filter(|url| self.urls.contains(url))
            .collect()
    }

    /// Keeps the selection to the shown rows: cleared when the filter changed, pruned when the view
    /// was rebuilt.
    pub(super) fn follow(&mut self, filter: &BrokenFilter, view: &BrokenView, context: &Context<'_>) {
        if self.filter.as_ref() != Some(filter) {
            self.filter = Some(filter.clone());
            self.rebuilds = view.rebuilds;
            self.clear();
            return;
        }
        if self.rebuilds != view.rebuilds {
            self.rebuilds = view.rebuilds;
            self.anchor = None;
            if !self.urls.is_empty() {
                let shown: HashSet<&str> = view
                    .rows()
                    .iter()
                    .filter_map(|row| context.item(*row))
                    .map(|item| item.url())
                    .collect();
                self.urls.retain(|url| shown.contains(url.as_str()));
            }
        }
    }

    /// Applies a click on a checkbox. `shown` is the URL of each row as drawn (`None` for a page
    /// heading of the *by page* layout, where a URL can be shown under several pages).
    pub(super) fn apply(&mut self, click: Click, shown: &[Option<String>]) {
        let url_at = |position: usize| shown.get(position).cloned().flatten();
        match click {
            Click::All(true) => {
                self.urls = shown.iter().flatten().cloned().collect();
                self.anchor = None;
            }
            Click::All(false) => self.clear(),
            Click::Row(position, shift) => {
                let Some(url) = url_at(position) else {
                    return;
                };
                let select = !self.urls.contains(&url);
                let range = match (shift, self.anchor) {
                    (true, Some(anchor)) => anchor.min(position)..=anchor.max(position),
                    _ => position..=position,
                };
                for at in range {
                    if let Some(url) = url_at(at) {
                        if select {
                            self.urls.insert(url);
                        } else {
                            self.urls.remove(&url);
                        }
                    }
                }
                self.anchor = Some(position);
            }
        }
    }
}

/// Whether the click of this frame was made with Shift (the modifiers of the pointer event, which
/// a test's scripted click carries too).
fn shift_click(ui: &Ui) -> bool {
    ui.input(|input| {
        input.modifiers.shift
            || input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::PointerButton {
                        pressed: false,
                        modifiers,
                        ..
                    } if modifiers.shift
                )
            })
    })
}

/// The header checkbox: checked when every shown row is selected, mixed when some are.
pub(super) fn header_checkbox(ui: &mut Ui, selection: &Selection, shown: usize) -> Option<Click> {
    let all = shown > 0 && selection.len() >= shown;
    let mut checked = all;
    let response = ui
        .add(egui::Checkbox::without_text(&mut checked).indeterminate(!selection.is_empty() && !all))
        .accessible_name("Select all shown rows")
        .on_hover_text(if all {
            "Clear the selection"
        } else {
            "Select every row shown"
        });
    response.clicked().then_some(Click::All(!all))
}

/// The checkbox of the row at `position` (Shift+click extends the range from the last one).
pub(super) fn row_checkbox(ui: &mut Ui, selection: &Selection, position: usize, url: &str) -> Option<Click> {
    let mut checked = selection.contains(url);
    let response = ui
        .add(egui::Checkbox::without_text(&mut checked))
        .accessible_name(format!("Select {url}"));
    response.clicked().then(|| Click::Row(position, shift_click(ui)))
}
