//! A horizontal stepper: connected steps (done = check, active = accent with a spinner, pending =
//! hollow), each with a label and a one-line detail underneath.

use eframe::egui::{self, Align2, Color32, FontId, Role, Sense, Stroke, Ui, WidgetInfo};
use egui_material_icons::icons;

use super::{one_line, progress};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Pending,
    Active,
    Done,
    /// Ended, but not as planned (e.g. stopped).
    Warning,
    Failed,
}

impl StepState {
    fn word(self) -> &'static str {
        match self {
            StepState::Pending => "pending",
            StepState::Active => "in progress",
            StepState::Done => "done",
            StepState::Warning => "ended early",
            StepState::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub label: String,
    pub state: StepState,
    pub detail: String,
}

impl Step {
    pub fn new(label: impl Into<String>, state: StepState, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            state,
            detail: detail.into(),
        }
    }
}

/// The height of the stepper.
pub const STEPPER_HEIGHT: f32 = 40.0;

const DOT: f32 = 22.0;

/// Draws the steps across the available width.
pub fn stepper(ui: &mut Ui, steps: &[Step]) {
    if steps.is_empty() {
        return;
    }
    let p = theme::palette(ui);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, STEPPER_HEIGHT), Sense::hover());
    let segment = rect.width() / steps.len() as f32;
    let time = ui.input(|input| input.time);
    let mut animating = false;
    for (index, step) in steps.iter().enumerate() {
        let left = rect.left() + segment * index as f32;
        let center = egui::pos2(left + DOT / 2.0, rect.top() + DOT / 2.0);
        let text_left = left + DOT + 10.0;
        // Each step is a labelled node for screen readers and tests.
        let step_rect = egui::Rect::from_min_size(egui::pos2(left, rect.top()), egui::vec2(segment, rect.height()));
        let response = ui.interact(step_rect, ui.scope_id().with(("step", index)), Sense::hover());
        let name = if step.detail.is_empty() {
            format!("{}: {}", step.label, step.state.word())
        } else {
            format!("{}: {}, {}", step.label, step.state.word(), step.detail)
        };
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));

        let painter = ui.painter();
        let fill = match step.state {
            StepState::Pending => p.surface,
            StepState::Active => p.accent_soft,
            StepState::Done => p.success,
            StepState::Warning => p.warning,
            StepState::Failed => p.danger,
        };
        match step.state {
            StepState::Pending => {
                painter.circle(center, DOT / 2.0 - 1.0, fill, Stroke::new(1.5, p.border_strong));
                painter.text(
                    center,
                    Align2::CENTER_CENTER,
                    (index + 1).to_string(),
                    FontId::proportional(11.5),
                    p.text_weak,
                );
            }
            StepState::Active => {
                painter.circle(center, DOT / 2.0, fill, Stroke::NONE);
                progress::paint_spinner(painter, center, DOT / 2.0 - 4.0, p.accent, time);
                animating = true;
            }
            StepState::Done | StepState::Warning | StepState::Failed => {
                painter.circle_filled(center, DOT / 2.0, fill);
                let icon = match step.state {
                    StepState::Done => icons::ICON_CHECK,
                    StepState::Warning => icons::ICON_PRIORITY_HIGH,
                    _ => icons::ICON_CLOSE,
                };
                painter.text(
                    center,
                    Align2::CENTER_CENTER,
                    icon.codepoint,
                    FontId::new(15.0, icon.font_family()),
                    Color32::WHITE,
                );
            }
        }
        let next_left = left + segment;
        let is_last = index + 1 == steps.len();
        let text_width = if is_last {
            rect.right() - text_left
        } else {
            (next_left - text_left - 18.0).max(40.0)
        };
        let label_color = if step.state == StepState::Pending {
            p.text_weak
        } else {
            p.text
        };
        let label = one_line(ui, &step.label, theme::semibold_font(14.0), label_color, text_width);
        let label_width = label.size().x;
        painter.galley(
            egui::pos2(text_left, center.y - label.size().y / 2.0),
            label,
            label_color,
        );
        if !step.detail.is_empty() {
            let detail_color = match step.state {
                StepState::Active => p.accent,
                StepState::Failed => p.danger,
                StepState::Warning => p.warning,
                _ => p.text_weak,
            };
            let detail = one_line(ui, &step.detail, FontId::proportional(12.0), detail_color, text_width);
            painter.galley(egui::pos2(text_left, rect.top() + DOT + 2.0), detail, detail_color);
        }
        if !is_last {
            let from = text_left + label_width + 10.0;
            let to = next_left - 8.0;
            if to > from {
                let line_color = if step.state == StepState::Done {
                    p.success.gamma_multiply(0.6)
                } else {
                    p.border_strong
                };
                painter.hline(from..=to, center.y, Stroke::new(1.5, line_color));
            }
        }
    }
    if animating {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(40));
    }
}
