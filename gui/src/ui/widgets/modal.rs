//! Modal dialogs: centred, over a dimmed background, closed with `Esc` (GUI-SPEC §6.1).
//!
//! A [`Dialog`] is plain data — title, message, an optional list, an optional text input and
//! check box, and buttons that each carry an action of the caller's type `A`. The caller keeps the
//! dialog, calls [`Dialog::show`] every frame and performs the chosen action.

use eframe::egui::{self, Frame, Id, Key, Margin, RichText, ScrollArea, Stroke};

use super::Tone;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Danger,
    Secondary,
}

#[derive(Debug, Clone)]
pub struct DialogButton<A> {
    pub label: String,
    pub kind: ButtonKind,
    /// `None` closes the dialog without an action (Cancel).
    pub action: Option<A>,
}

#[derive(Debug, Clone, Default)]
pub struct DialogInput {
    pub label: String,
    pub value: String,
    pub hint: String,
    pub multiline: bool,
}

#[derive(Debug, Clone)]
pub struct Dialog<A> {
    pub title: String,
    pub message: String,
    pub tone: Option<Tone>,
    /// A list shown under the message in a scrollable box (e.g. unknown options).
    pub details: Vec<String>,
    /// Monospace text shown in a scrollable box (e.g. the CLI's error output).
    pub code: Option<String>,
    pub input: Option<DialogInput>,
    /// A check box: label and state.
    pub check: Option<(String, bool)>,
    /// An error shown above the buttons (e.g. "a profile with this name exists").
    pub error: Option<String>,
    pub buttons: Vec<DialogButton<A>>,
    focused: bool,
}

/// What a dialog's frame produced.
#[derive(Debug, Clone)]
pub enum DialogOutcome<A> {
    /// Still open.
    Open,
    /// Closed without an action (Cancel, `Esc`).
    Dismissed,
    /// A button with an action was chosen; the dialog closes unless the caller shows it again.
    Chosen(A),
}

impl<A: Clone> Dialog<A> {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: String::new(),
            tone: None,
            details: Vec::new(),
            code: None,
            input: None,
            check: None,
            error: None,
            buttons: Vec::new(),
            focused: false,
        }
    }

    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    pub fn details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }

    pub fn code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn input(mut self, label: impl Into<String>, value: impl Into<String>, hint: impl Into<String>) -> Self {
        self.input = Some(DialogInput {
            label: label.into(),
            value: value.into(),
            hint: hint.into(),
            multiline: false,
        });
        self
    }

    pub fn multiline_input(mut self, label: impl Into<String>, hint: impl Into<String>) -> Self {
        self.input = Some(DialogInput {
            label: label.into(),
            value: String::new(),
            hint: hint.into(),
            multiline: true,
        });
        self
    }

    pub fn check(mut self, label: impl Into<String>, checked: bool) -> Self {
        self.check = Some((label.into(), checked));
        self
    }

    pub fn button(mut self, label: impl Into<String>, kind: ButtonKind, action: A) -> Self {
        self.buttons.push(DialogButton {
            label: label.into(),
            kind,
            action: Some(action),
        });
        self
    }

    pub fn cancel(mut self, label: impl Into<String>) -> Self {
        self.buttons.push(DialogButton {
            label: label.into(),
            kind: ButtonKind::Secondary,
            action: None,
        });
        self
    }

    /// The text of the input, trimmed.
    pub fn input_text(&self) -> String {
        self.input
            .as_ref()
            .map(|input| input.value.trim().to_string())
            .unwrap_or_default()
    }

    pub fn checked(&self) -> bool {
        self.check.as_ref().is_some_and(|(_, checked)| *checked)
    }

    /// Draws the dialog. `Enter` in a single-line input chooses the first primary/danger button.
    pub fn show(&mut self, ctx: &egui::Context) -> DialogOutcome<A> {
        let mut outcome = DialogOutcome::Open;
        let escape = ctx.input(|input| input.key_pressed(Key::Escape));
        let id = Id::unique("dialog").with(&self.title);
        let palette = theme::palette_for(ctx.theme() == egui::Theme::Dark);
        let backdrop = egui::Color32::from_black_alpha(if palette.dark { 150 } else { 90 });
        let frame = Frame::new()
            .fill(palette.surface)
            .stroke(Stroke::new(1.0, palette.border))
            .corner_radius(10)
            .shadow(egui::Shadow {
                offset: [0, 8],
                blur: 28,
                spread: 0,
                color: egui::Color32::from_black_alpha(if palette.dark { 120 } else { 50 }),
            })
            .inner_margin(Margin::same(20));
        egui::Modal::new(id)
            .frame(frame)
            .backdrop_color(backdrop)
            .show(ctx, |ui| {
                let p = theme::palette(ui);
                let width = (ctx.content_rect().width() - 64.0).clamp(280.0, 520.0);
                ui.set_width(width);
                ui.horizontal(|ui| {
                    if let Some(tone) = self.tone {
                        ui.label(tone.icon().rich_text().size(22.0).color(tone.color(p)));
                    }
                    ui.label(theme::semibold(&self.title).size(17.0).color(p.text));
                });
                if !self.message.is_empty() {
                    ui.add_space(theme::GRID);
                    ui.add(egui::Label::new(RichText::new(&self.message).color(p.text)).wrap());
                }
                if !self.details.is_empty() {
                    ui.add_space(theme::GRID);
                    Frame::new()
                        .fill(p.surface_alt)
                        .corner_radius(theme::RADIUS)
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                                for item in &self.details {
                                    ui.label(theme::mono(item));
                                }
                            });
                        });
                }
                if let Some(code) = &self.code {
                    ui.add_space(theme::GRID);
                    Frame::new()
                        .fill(p.surface_alt)
                        .corner_radius(theme::RADIUS)
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                                ui.add(egui::Label::new(theme::mono(code)).selectable(true).wrap());
                            });
                        });
                }
                let mut submit = false;
                if let Some(input) = &mut self.input {
                    ui.add_space(theme::GRID * 1.5);
                    let label = ui.label(&input.label);
                    let response = if input.multiline {
                        ui.add(
                            egui::TextEdit::multiline(&mut input.value)
                                .hint_text(&input.hint)
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(6)
                                .desired_width(f32::INFINITY),
                        )
                    } else {
                        ui.add(
                            egui::TextEdit::singleline(&mut input.value)
                                .hint_text(&input.hint)
                                .desired_width(f32::INFINITY)
                                .margin(egui::vec2(8.0, 6.0)),
                        )
                    }
                    .labelled_by(label.id);
                    if !self.focused {
                        response.request_focus();
                        self.focused = true;
                    }
                    if response.changed() {
                        self.error = None;
                    }
                    submit = !input.multiline && response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                }
                if let Some((label, checked)) = &mut self.check {
                    ui.add_space(theme::GRID);
                    ui.checkbox(checked, label.as_str());
                }
                if let Some(error) = &self.error {
                    ui.add_space(theme::GRID);
                    super::inline_message(ui, Tone::Danger, error);
                }
                ui.add_space(theme::GRID * 2.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Right to left: the last button is the leftmost; the primary one goes right.
                    for button in self.buttons.iter().rev() {
                        let clicked = match button.kind {
                            ButtonKind::Primary => super::primary_button(ui, true, None, &button.label, 30.0).clicked(),
                            ButtonKind::Danger => {
                                let response = ui
                                    .add(
                                        egui::Button::new(theme::semibold(&button.label).color(egui::Color32::WHITE))
                                            .fill(p.danger)
                                            .min_size(egui::vec2(0.0, 30.0)),
                                    )
                                    .accessible_name(&button.label);
                                response.clicked()
                            }
                            ButtonKind::Secondary => ui
                                .add(egui::Button::new(&button.label).min_size(egui::vec2(0.0, 30.0)))
                                .accessible_name(&button.label)
                                .clicked(),
                        };
                        if clicked {
                            outcome = match &button.action {
                                Some(action) => DialogOutcome::Chosen(action.clone()),
                                None => DialogOutcome::Dismissed,
                            };
                        }
                    }
                });
                if submit
                    && let Some(action) = self
                        .buttons
                        .iter()
                        .find(|button| button.kind != ButtonKind::Secondary)
                        .and_then(|button| button.action.clone())
                {
                    outcome = DialogOutcome::Chosen(action);
                }
            });
        if escape && matches!(outcome, DialogOutcome::Open) {
            outcome = DialogOutcome::Dismissed;
        }
        outcome
    }
}

impl<A: PartialEq> PartialEq for DialogButton<A> {
    fn eq(&self, other: &Self) -> bool {
        self.label == other.label && self.kind == other.kind && self.action == other.action
    }
}
