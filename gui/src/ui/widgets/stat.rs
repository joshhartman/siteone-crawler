//! Stat tiles: a big number with a small label and an optional sub-line, laid out in a row that
//! wraps on narrow windows.

use eframe::egui::{self, Align2, FontId, Response, Role, Sense, Stroke, Ui, WidgetInfo};

use super::{Tone, one_line};
use crate::theme;

/// The height of a tile.
pub const TILE_HEIGHT: f32 = 80.0;
/// Tiles are not narrower than this; a row wraps instead.
pub const TILE_MIN_WIDTH: f32 = 150.0;

/// One tile's content.
#[derive(Debug, Clone, PartialEq)]
pub struct Stat {
    pub label: String,
    pub value: String,
    pub sub: Option<String>,
    /// Colours the value (e.g. errors in red). The label says what it means.
    pub tone: Option<Tone>,
    /// The tile is a button (with this tooltip) that opens the details.
    pub link: Option<String>,
}

impl Stat {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            sub: None,
            tone: None,
            link: None,
        }
    }

    pub fn link(mut self, tooltip: impl Into<String>) -> Self {
        self.link = Some(tooltip.into());
        self
    }

    pub fn detail(mut self, sub: impl Into<String>) -> Self {
        self.sub = Some(sub.into());
        self
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }
}

/// A tile `width` wide. Its accessible name is its label (a button when it links somewhere).
pub fn stat_tile(ui: &mut Ui, width: f32, stat: &Stat) -> Response {
    let p = theme::palette(ui);
    let sense = if stat.link.is_some() {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(width, TILE_HEIGHT), sense);
    let name = format!("{}: {}", stat.label, stat.value);
    if stat.link.is_some() {
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &stat.label));
    } else {
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
    }
    if let Some(tip) = &stat.link {
        response = response.on_hover_text(tip);
    }
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let border = if stat.link.is_some() && response.hovered() {
        p.border_strong
    } else {
        p.border
    };
    painter.rect(
        rect,
        theme::CARD_RADIUS,
        p.surface,
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    if stat.link.is_some() {
        painter.text(
            rect.right_top() + egui::vec2(-10.0, 10.0),
            Align2::RIGHT_TOP,
            egui_material_icons::icons::ICON_ARROW_FORWARD.codepoint,
            FontId::new(15.0, egui_material_icons::icons::ICON_ARROW_FORWARD.font_family()),
            p.text_weak,
        );
    }
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            theme::CARD_RADIUS + 2,
            Stroke::new(1.5, p.accent),
            egui::StrokeKind::Outside,
        );
    }
    let inner = rect.shrink2(egui::vec2(14.0, 11.0));
    let label = one_line(ui, &stat.label, FontId::proportional(12.5), p.text_weak, inner.width());
    painter.galley(inner.left_top(), label, p.text_weak);
    let color = stat.tone.map_or(p.text, |tone| tone.color(p));
    let value = one_line(ui, &stat.value, theme::semibold_font(22.0), color, inner.width());
    painter.galley(inner.left_top() + egui::vec2(0.0, 17.0), value, color);
    if let Some(sub) = &stat.sub {
        let sub = one_line(ui, sub, FontId::proportional(12.0), p.text_weak, inner.width());
        painter.galley(inner.left_bottom() - egui::vec2(0.0, sub.size().y), sub, p.text_weak);
    }
    response
}

/// Tiles in rows of equal width that fill the available width, wrapping when a tile would get
/// narrower than [`TILE_MIN_WIDTH`]. Returns the position of a linked tile that was clicked.
pub fn tile_row(ui: &mut Ui, stats: &[Stat]) -> Option<usize> {
    if stats.is_empty() {
        return None;
    }
    let mut clicked = None;
    let gap = theme::GRID * 1.5;
    let width = ui.available_width();
    let fit = (((width + gap) / (TILE_MIN_WIDTH + gap)).floor() as usize).max(1);
    let per_row = if fit >= stats.len() {
        stats.len()
    } else {
        // Balanced rows (5 tiles in a window for 4: 3 + 2, not 4 + 1).
        let rows = stats.len().div_ceil(fit);
        stats.len().div_ceil(rows)
    };
    let tile_width = (width - gap * (per_row as f32 - 1.0)) / per_row as f32;
    for (index, row) in stats.chunks(per_row).enumerate() {
        if index > 0 {
            ui.add_space(gap - ui.spacing().item_spacing.y);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (offset, stat) in row.iter().enumerate() {
                if stat_tile(ui, tile_width, stat).clicked() {
                    clicked = Some(index * per_row + offset);
                }
            }
        });
    }
    clicked
}
