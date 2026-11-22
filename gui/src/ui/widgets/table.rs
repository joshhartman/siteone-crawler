//! Helpers of the data tables: sortable header cells, text that is cut in the middle, and
//! case-insensitive matching without allocating per row.

use eframe::egui::{self, Align, FontId, Layout, Response, RichText, Role, Sense, Ui, WidgetInfo};
use egui_material_icons::icons;

use crate::theme;

/// The height of a table row.
pub const ROW_HEIGHT: f32 = 26.0;
/// The height of a header row.
pub const HEADER_HEIGHT: f32 = 30.0;

/// A header cell that sorts its column: the label, an arrow when it is the sort column. Returns
/// the response (`clicked()` toggles the sort). Named "Sort by <label>".
pub fn sort_header(ui: &mut Ui, label: &str, sorted: Option<bool>, right_aligned: bool) -> Response {
    let p = theme::palette(ui);
    let layout = if right_aligned {
        Layout::right_to_left(Align::Center)
    } else {
        Layout::left_to_right(Align::Center)
    };
    let inner = ui.with_layout(layout, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let color = if sorted.is_some() { p.text } else { p.text_weak };
        let text = theme::semibold(label).size(12.5).color(color);
        let arrow = match sorted {
            Some(true) => Some(icons::ICON_ARROW_UPWARD),
            Some(false) => Some(icons::ICON_ARROW_DOWNWARD),
            None => None,
        };
        let button = match arrow {
            Some(arrow) if right_aligned => egui::Button::new((arrow.rich_text().size(14.0).color(p.accent), text)),
            Some(arrow) => egui::Button::new((text, arrow.rich_text().size(14.0).color(p.accent))),
            None => egui::Button::new(text),
        };
        ui.add(button.frame(false).min_size(egui::vec2(0.0, 22.0)))
    });
    let response = inner.inner;
    let name = format!("Sort by {label}");
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
    response.on_hover_text(match sorted {
        Some(true) => "Sorted ascending — click to sort descending",
        Some(false) => "Sorted descending — click to sort ascending",
        None => "Click to sort by this column",
    })
}

/// A plain (not sortable) header cell.
pub fn header_label(ui: &mut Ui, label: &str) {
    let p = theme::palette(ui);
    ui.label(theme::semibold(label).size(12.5).color(p.text_weak));
}

/// `text` cut in the middle with an ellipsis so that it fits `width` in `font`
/// (`https://example.com/…/page.html`). Returns the text unchanged when it fits.
pub fn middle_truncate(ui: &Ui, text: &str, font: &FontId, width: f32) -> String {
    let measure = |candidate: &str| {
        ui.painter()
            .layout_no_wrap(candidate.to_string(), font.clone(), egui::Color32::WHITE)
            .size()
            .x
    };
    if measure(text) <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    // The longest cut that fits: keep `keep` characters, 40 % of them from the start.
    let (mut low, mut high) = (0usize, chars.len());
    let cut = |keep: usize| {
        let head = (keep * 2).div_ceil(5);
        let tail = keep - head;
        let mut out: String = chars.iter().take(head).collect();
        out.push('…');
        out.extend(chars.iter().skip(chars.len() - tail));
        out
    };
    while low < high {
        let mid = (low + high).div_ceil(2);
        if measure(&cut(mid)) <= width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    cut(low)
}

/// Whether `haystack` contains `needle_lower` (already lower-case), ignoring case. ASCII needles
/// are matched without allocating.
pub fn contains_ignore_case(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if needle_lower.is_ascii() {
        let needle = needle_lower.as_bytes();
        haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
    } else {
        haystack.to_lowercase().contains(needle_lower)
    }
}

/// A one-line label that is cut at the cell's width; the full text shows on hover when cut.
pub fn cell_text(ui: &mut Ui, text: RichText) -> Response {
    ui.add(egui::Label::new(text).truncate())
}

/// A clickable variant of [`cell_text`] (e.g. click to copy).
pub fn cell_text_clickable(ui: &mut Ui, text: RichText) -> Response {
    ui.add(egui::Label::new(text).truncate().sense(Sense::click()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_ignores_case() {
        assert!(contains_ignore_case("https://Example.com/About", "about"));
        assert!(contains_ignore_case("anything", ""));
        assert!(!contains_ignore_case("short", "longer needle"));
        assert!(contains_ignore_case("Žluťoučký kůň", "kůň"));
        assert!(contains_ignore_case("ŽLUŤOUČKÝ", "žluť"));
    }
}
