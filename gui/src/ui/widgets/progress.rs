//! Progress indicators: a spinner, a slim progress bar (determinate or indeterminate) and the
//! small proportional bars of breakdown lists.
//!
//! Animations ask for the next frame with a short delay instead of at once, so an animating page
//! does not keep the CPU busy (and a headless test harness settles).

use std::time::Duration;

use eframe::egui::{self, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui};

use crate::theme::Palette;

/// The frame interval of the animations.
const ANIMATION_FRAME: Duration = Duration::from_millis(40);

/// A spinning arc `size` points large.
pub fn spinner(ui: &mut Ui, size: f32, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    if ui.is_rect_visible(rect) {
        let time = ui.input(|input| input.time);
        paint_spinner(ui.painter(), rect.center(), size / 2.0 - 1.5, color, time);
        ui.ctx().request_repaint_after(ANIMATION_FRAME);
    }
    response
}

/// Paints a spinner frame at `time` (seconds).
pub fn paint_spinner(painter: &egui::Painter, center: Pos2, radius: f32, color: Color32, time: f64) {
    let start = (time * std::f64::consts::TAU * 0.9) as f32;
    let sweep = std::f32::consts::TAU * 0.7;
    let points: Vec<Pos2> = (0..=24)
        .map(|i| {
            let angle = start + sweep * i as f32 / 24.0;
            center + radius * egui::vec2(angle.cos(), angle.sin())
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(2.0, color)));
}

/// A slim progress bar across the available width. `fraction = None` is indeterminate (a moving
/// segment).
pub fn slim_bar(ui: &mut Ui, fraction: Option<f32>, color: Color32, p: &Palette) -> Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 4.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, track(p));
    match fraction {
        Some(fraction) => {
            let fraction = fraction.clamp(0.0, 1.0);
            if fraction > 0.0 {
                let filled = Rect::from_min_size(rect.min, egui::vec2(rect.width() * fraction, rect.height()));
                painter.rect_filled(filled, 2.0, color);
            }
        }
        None => {
            let time = ui.input(|input| input.time) as f32;
            let segment = rect.width() * 0.25;
            let travel = rect.width() + segment;
            let x = rect.left() - segment + (time * 0.6).fract() * travel;
            let left = x.max(rect.left());
            let right = (x + segment).min(rect.right());
            if right > left {
                let moving = Rect::from_x_y_ranges(left..=right, rect.y_range());
                painter.rect_filled(moving, 2.0, color);
            }
            ui.ctx().request_repaint_after(ANIMATION_FRAME);
        }
    }
    response
}

/// A small horizontal bar `width` wide, filled to `fraction` (breakdown lists).
pub fn fraction_bar(ui: &mut Ui, width: f32, fraction: f32, color: Color32, p: &Palette) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 6.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 3.0, track(p));
        let fraction = fraction.clamp(0.0, 1.0);
        if fraction > 0.0 {
            let filled = Rect::from_min_size(rect.min, egui::vec2((rect.width() * fraction).max(3.0), rect.height()));
            ui.painter().rect_filled(filled, 3.0, color);
        }
    }
    response
}

/// The unfilled part of a bar.
pub fn track(p: &Palette) -> Color32 {
    if p.dark {
        Color32::from_rgb(48, 54, 62)
    } else {
        Color32::from_rgb(226, 230, 235)
    }
}
