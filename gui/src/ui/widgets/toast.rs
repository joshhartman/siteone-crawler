//! Toasts: short notices in the bottom-right corner that disappear by themselves (GUI-SPEC §6.1).

use eframe::egui::{self, Align2, Area, Frame, Id, Margin, Order, RichText, Stroke};
use egui_material_icons::icons;

use super::Tone;
use crate::theme;

/// At most this many toasts are kept; older ones make room.
const MAX_TOASTS: usize = 5;

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub tone: Tone,
    pub text: String,
    /// `ctx` time when it was shown; set on the first frame it is drawn.
    shown_at: Option<f64>,
    /// Seconds on screen.
    pub seconds: f64,
}

/// The queue of toasts.
#[derive(Debug, Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn info(&mut self, text: impl Into<String>) {
        self.push(Tone::Info, text, 5.0);
    }

    pub fn success(&mut self, text: impl Into<String>) {
        self.push(Tone::Success, text, 4.0);
    }

    pub fn warning(&mut self, text: impl Into<String>) {
        self.push(Tone::Warning, text, 9.0);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.push(Tone::Danger, text, 12.0);
    }

    pub fn push(&mut self, tone: Tone, text: impl Into<String>, seconds: f64) {
        let text = text.into();
        // The same notice twice in a row is shown once (e.g. a repeated save failure).
        if self.items.last().is_some_and(|last| last.text == text) {
            return;
        }
        if self.items.len() >= MAX_TOASTS {
            self.items.remove(0);
        }
        self.items.push(Toast {
            tone,
            text,
            shown_at: None,
            seconds,
        });
    }

    pub fn items(&self) -> &[Toast] {
        &self.items
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Draws the toasts and drops the expired ones.
    pub fn show(&mut self, ctx: &egui::Context) {
        if self.items.is_empty() {
            return;
        }
        let now = ctx.input(|input| input.time);
        for toast in &mut self.items {
            toast.shown_at.get_or_insert(now);
        }
        self.items
            .retain(|toast| now - toast.shown_at.unwrap_or(now) < toast.seconds);
        if let Some(next) = self
            .items
            .iter()
            .map(|toast| toast.shown_at.unwrap_or(now) + toast.seconds - now)
            .reduce(f64::min)
        {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(next.max(0.05)));
        }

        let mut close = None;
        Area::new(Id::unique("toasts"))
            .order(Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .interactable(true)
            .show(ctx, |ui| {
                let p = theme::palette(ui);
                ui.set_max_width(380.0);
                for (index, toast) in self.items.iter().enumerate() {
                    let color = toast.tone.color(p);
                    Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0, p.border))
                        .corner_radius(theme::CARD_RADIUS)
                        .shadow(ui.visuals().popup_shadow)
                        .inner_margin(Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_width(356.0);
                            ui.horizontal_top(|ui| {
                                ui.label(toast.tone.icon().rich_text().color(color).size(18.0));
                                let width = ui.available_width() - 28.0;
                                ui.allocate_ui(egui::vec2(width.max(80.0), 0.0), |ui| {
                                    ui.add(egui::Label::new(RichText::new(&toast.text).color(p.text)).wrap());
                                });
                                if super::icon_button(ui, icons::ICON_CLOSE, "Dismiss notice").clicked() {
                                    close = Some(index);
                                }
                            });
                        });
                    ui.add_space(theme::GRID);
                }
            });
        if let Some(index) = close
            && index < self.items.len()
        {
            self.items.remove(index);
        }
    }
}
