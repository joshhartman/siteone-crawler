//! Quality scores: the crawler's bands as tones, a score ring (gauge) and a thin score bar.

use eframe::egui::{self, Align2, FontId, Pos2, Response, Role, Sense, Shape, Stroke, Ui, WidgetInfo};

use super::{Tone, progress};
use crate::theme;

/// The tone of a score, following the crawler's bands (`scoring::quality_score::score_label`):
/// Excellent ≥ 9 and Good ≥ 7 green, Fair ≥ 5 amber, Poor and Critical red.
pub fn band_tone(score: f64) -> Tone {
    if score >= 7.0 {
        Tone::Success
    } else if score >= 5.0 {
        Tone::Warning
    } else {
        Tone::Danger
    }
}

/// `8.3` — one decimal, as the crawler shows scores.
pub fn score_text(score: f64) -> String {
    format!("{:.1}", score.clamp(0.0, 10.0))
}

/// A ring `size` points across, filled clockwise from the top to `score / 10`, the score in the
/// middle. `name` is its accessible name.
pub fn score_ring(ui: &mut Ui, size: f32, score: f64, name: &str) -> Response {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, name));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let center = rect.center();
    let thickness = (size * 0.085).max(5.0);
    let radius = size / 2.0 - thickness / 2.0 - 1.0;
    // The track as a path like the arc, so both have the same width and centre line.
    let track: Vec<Pos2> = (0..96)
        .map(|i| {
            let angle = std::f32::consts::TAU * i as f32 / 96.0;
            center + radius * egui::vec2(angle.cos(), angle.sin())
        })
        .collect();
    painter.add(Shape::closed_line(track, Stroke::new(thickness, progress::track(p))));
    let fraction = (score / 10.0).clamp(0.0, 1.0) as f32;
    if fraction > 0.0 {
        let start = -std::f32::consts::FRAC_PI_2;
        let sweep = std::f32::consts::TAU * fraction;
        let steps = ((64.0 * fraction).ceil() as usize).max(2);
        let points: Vec<Pos2> = (0..=steps)
            .map(|i| {
                let angle = start + sweep * i as f32 / steps as f32;
                center + radius * egui::vec2(angle.cos(), angle.sin())
            })
            .collect();
        let color = band_tone(score).color(p);
        // Round caps: a dot at each end of the arc.
        if let (Some(first), Some(last)) = (points.first().copied(), points.last().copied()) {
            painter.circle_filled(first, thickness / 2.0, color);
            painter.circle_filled(last, thickness / 2.0, color);
        }
        painter.add(Shape::line(points, Stroke::new(thickness, color)));
    }
    painter.text(
        center - egui::vec2(0.0, size * 0.06),
        Align2::CENTER_CENTER,
        score_text(score),
        theme::semibold_font(size * 0.27),
        p.text,
    );
    painter.text(
        center + egui::vec2(0.0, size * 0.17),
        Align2::CENTER_CENTER,
        "of 10",
        FontId::proportional((size * 0.11).max(11.0)),
        p.text_weak,
    );
    response
}

/// A thin bar filled to `score / 10` in the band's colour.
pub fn score_bar(ui: &mut Ui, width: f32, score: f64) -> Response {
    let p = theme::palette(ui);
    progress::fraction_bar(ui, width, (score / 10.0) as f32, band_tone(score).color(p), p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_follow_the_crawler_thresholds() {
        assert_eq!(band_tone(9.5), Tone::Success);
        assert_eq!(band_tone(7.0), Tone::Success);
        assert_eq!(band_tone(6.9), Tone::Warning);
        assert_eq!(band_tone(5.0), Tone::Warning);
        assert_eq!(band_tone(4.9), Tone::Danger);
        assert_eq!(score_text(8.26), "8.3");
        assert_eq!(score_text(11.0), "10.0");
    }
}
