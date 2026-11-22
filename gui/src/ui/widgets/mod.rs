//! Shared widgets of the pages (GUI-SPEC §6.6): cards, labelled field rows, chips, buttons, a
//! toggle switch, banners, empty states, search highlighting, toasts, dialogs and number formats;
//! for the Run and History pages also stat tiles, a phase stepper, a tab bar, score rings,
//! progress bars and data-table helpers.
//!
//! Every interactive widget here carries an accessible name (screen readers and the UI tests
//! find controls by it).

pub mod format;
pub mod modal;
pub mod progress;
pub mod score;
pub mod stat;
pub mod stepper;
pub mod table;
pub mod tabs;
pub mod toast;

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, Response, RichText, Role, Sense, Stroke,
    TextFormat, Ui, Vec2, WidgetInfo, WidgetText, text::LayoutJob,
};
use egui_material_icons::{MaterialIcon, icons};

use crate::theme::{self, Palette};

/// The meaning of a coloured element. Colour is never the only signal: chips carry text, banners
/// and toasts an icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Info,
    Warning,
    Danger,
}

impl Tone {
    pub fn color(self, p: &Palette) -> Color32 {
        match self {
            Tone::Neutral => p.text_weak,
            Tone::Accent => p.accent,
            Tone::Success => p.success,
            Tone::Info => p.info,
            Tone::Warning => p.warning,
            Tone::Danger => p.danger,
        }
    }

    pub fn icon(self) -> MaterialIcon {
        match self {
            Tone::Neutral | Tone::Info | Tone::Accent => icons::ICON_INFO,
            Tone::Success => icons::ICON_CHECK_CIRCLE,
            Tone::Warning => icons::ICON_WARNING,
            Tone::Danger => icons::ICON_ERROR,
        }
    }

    /// A faint background in the tone's colour.
    pub fn soft(self, p: &Palette) -> Color32 {
        let base = self.color(p);
        let alpha = if p.dark { 38 } else { 26 };
        Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha)
    }
}

/// A card: surface fill, subtle border, rounded corners, 16 px padding.
pub fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    let p = theme::palette(ui);
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(theme::CARD_RADIUS)
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
}

/// A title with an optional one-line explanation underneath.
pub fn section_heading(ui: &mut Ui, title: &str, subtitle: Option<&str>) {
    ui.label(theme::section_title(title));
    if let Some(subtitle) = subtitle {
        ui.label(RichText::new(subtitle).color(theme::palette(ui).text_weak));
    }
}

/// Weak (secondary) text.
pub fn weak(ui: &Ui, text: impl Into<String>) -> RichText {
    RichText::new(text).color(theme::palette(ui).text_weak)
}

/// Width of the label column of [`field_row`].
pub const LABEL_WIDTH: f32 = 220.0;

/// A labelled row: the label on the left (with `tooltip` on hover, e.g. the CLI option name), a
/// changed marker, and the control on the right. The control is named after the label.
pub fn field_row<R>(
    ui: &mut Ui,
    label: &str,
    tooltip: &str,
    changed: bool,
    add_control: impl FnOnce(&mut Ui) -> R,
) -> R {
    let p = theme::palette(ui);
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(LABEL_WIDTH, 28.0), Sense::hover());
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        // The marker's gutter is always reserved, so every label starts at the same x.
        if changed {
            changed_dot(&mut child, p);
        } else {
            child.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
        }
        child
            .add(egui::Label::new(RichText::new(label).color(p.text)).truncate())
            .on_hover_text(tooltip);
        ui.vertical(|ui| add_control(ui)).inner
    })
    .inner
}

/// The small accent dot that marks a changed option.
pub fn changed_dot(ui: &mut Ui, p: &Palette) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, p.accent);
    response.on_hover_text("Changed from the CLI default");
}

/// A chip: a short label in a rounded pill, tinted by its tone.
pub fn chip(ui: &mut Ui, text: impl Into<String>, tone: Tone) -> Response {
    let p = theme::palette(ui);
    let text = text.into();
    let color = tone.color(p);
    // Sized from its text and painted directly: a framed label would stretch to the height of the
    // row it sits in.
    let galley = ui
        .painter()
        .layout_no_wrap(text.clone(), FontId::proportional(12.0), color);
    let size = egui::vec2(galley.size().x + 16.0, 20.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &text));
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 10.0, tone.soft(p));
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    response
}

/// A chip with a leading icon (or a spinner when `spinning`), e.g. the outcome of a run. Exactly
/// sized like [`chip`]; its accessible name is the text.
pub fn icon_chip(ui: &mut Ui, icon: MaterialIcon, text: &str, tone: Tone, spinning: bool) -> Response {
    let p = theme::palette(ui);
    let color = tone.color(p);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), theme::semibold_font(12.5), color);
    let size = egui::vec2(galley.size().x + 36.0, 24.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, 12.0, tone.soft(p));
        let icon_center = egui::pos2(rect.left() + 15.0, rect.center().y);
        if spinning {
            let time = ui.input(|input| input.time);
            progress::paint_spinner(painter, icon_center, 5.5, color, time);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(40));
        } else {
            painter.text(
                icon_center,
                egui::Align2::CENTER_CENTER,
                icon.codepoint,
                FontId::new(15.0, icon.font_family()),
                color,
            );
        }
        painter.galley(
            egui::pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
    }
    response
}

/// A filter toggle: a pill with a coloured dot, a label and an optional count. Named by `label`
/// (the count is shown, not part of the name). Flips `selected` when clicked.
pub fn filter_chip(ui: &mut Ui, selected: &mut bool, label: &str, count: Option<u64>, dot: Color32) -> Response {
    let p = theme::palette(ui);
    let text_color = if *selected { p.text } else { p.text_weak };
    let label_galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), FontId::proportional(13.0), text_color);
    let count_galley = count.map(|count| {
        ui.painter()
            .layout_no_wrap(format::count(count), FontId::proportional(12.0), p.text_weak)
    });
    let count_width = count_galley.as_ref().map_or(0.0, |galley| galley.size().x + 6.0);
    let size = egui::vec2(label_galley.size().x + count_width + 34.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *selected = !*selected;
    }
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, ui.is_enabled(), *selected, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let (fill, stroke) = if *selected {
            (p.surface, Stroke::new(1.0, p.border_strong))
        } else if response.hovered() {
            (p.surface_alt, Stroke::new(1.0, p.border))
        } else {
            (egui::Color32::TRANSPARENT, Stroke::new(1.0, p.border))
        };
        painter.rect(rect, 13.0, fill, stroke, egui::StrokeKind::Inside);
        let dot_center = egui::pos2(rect.left() + 14.0, rect.center().y);
        if *selected {
            painter.circle_filled(dot_center, 4.5, dot);
        } else {
            painter.circle_stroke(dot_center, 4.0, Stroke::new(1.5, dot.gamma_multiply(0.7)));
        }
        let label_width = label_galley.size().x;
        painter.galley(
            egui::pos2(rect.left() + 24.0, rect.center().y - label_galley.size().y / 2.0),
            label_galley,
            text_color,
        );
        if let Some(galley) = count_galley {
            painter.galley(
                egui::pos2(
                    rect.left() + 24.0 + label_width + 6.0,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                p.text_weak,
            );
        }
        if response.has_focus() {
            painter.rect_stroke(
                rect.expand(2.0),
                15.0,
                Stroke::new(1.5, p.accent),
                egui::StrokeKind::Outside,
            );
        }
    }
    response
}

/// A button filled in the danger colour (an action that ends something at once).
pub fn danger_fill_button(ui: &mut Ui, icon: Option<MaterialIcon>, label: &str, height: f32) -> Response {
    let p = theme::palette(ui);
    let text = theme::semibold(label).color(Color32::WHITE);
    let button = match icon {
        Some(icon) => egui::Button::new((icon.rich_text().size(18.0).color(Color32::WHITE), text)),
        None => egui::Button::new(text),
    };
    ui.add(
        button
            .fill(p.danger)
            .stroke(Stroke::NONE)
            .min_size(egui::vec2(0.0, height)),
    )
    .accessible_name(label)
}

/// A selectable chip (a check option shown as a pill). Returns the response; flips `selected`.
pub fn check_chip(ui: &mut Ui, selected: &mut bool, label: &str) -> Response {
    let p = theme::palette(ui);
    let color = if *selected { p.accent } else { p.text };
    let mut button = if *selected {
        egui::Button::new((
            icons::ICON_CHECK.rich_text().size(14.0).color(color),
            RichText::new(label).color(color),
        ))
    } else {
        egui::Button::new(RichText::new(label).color(color))
    }
    .corner_radius(CornerRadius::same(14))
    .min_size(egui::vec2(0.0, 26.0));
    if *selected {
        button = button.fill(p.accent_soft).stroke(Stroke::new(1.0, p.accent));
    }
    let response = ui.add(button).accessible_name(label);
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, ui.is_enabled(), *selected, label));
    if response.clicked() {
        *selected = !*selected;
    }
    response
}

/// An icon followed by a label, as the content of a button. The icon is drawn in the icon font.
pub fn with_icon(icon: MaterialIcon, label: impl Into<RichText>) -> (RichText, RichText) {
    (icon.rich_text().size(16.0), label.into())
}

/// The one primary button of a view (accent fill).
pub fn primary_button(ui: &mut Ui, enabled: bool, icon: Option<MaterialIcon>, label: &str, height: f32) -> Response {
    primary_button_sized(ui, enabled, icon, label, egui::vec2(0.0, height))
}

/// [`primary_button`] at least `size` large.
pub fn primary_button_sized(
    ui: &mut Ui,
    enabled: bool,
    icon: Option<MaterialIcon>,
    label: &str,
    size: Vec2,
) -> Response {
    let p = theme::palette(ui);
    let color = if enabled { p.on_accent } else { p.text_weak };
    let text = theme::semibold(label).color(color);
    let mut button = match icon {
        Some(icon) => egui::Button::new((icon.rich_text().size(18.0).color(color), text)),
        None => egui::Button::new(text),
    }
    .min_size(size);
    if enabled {
        button = button.fill(p.accent_fill).stroke(Stroke::NONE);
    }
    ui.add_enabled(enabled, button).accessible_name(label)
}

/// An ordinary button with an optional leading icon.
pub fn button(ui: &mut Ui, icon: Option<MaterialIcon>, label: &str) -> Response {
    let button = match icon {
        Some(icon) => egui::Button::new(with_icon(icon, label)),
        None => egui::Button::new(label),
    };
    ui.add(button).accessible_name(label)
}

/// A button that destroys something (red text).
pub fn danger_button(ui: &mut Ui, label: &str) -> Response {
    let p = theme::palette(ui);
    ui.add(egui::Button::new(RichText::new(label).color(p.danger)))
        .accessible_name(label)
}

/// A frameless icon button; `label` is its accessible name and tooltip.
pub fn icon_button(ui: &mut Ui, icon: MaterialIcon, label: &str) -> Response {
    let p = theme::palette(ui);
    ui.add(
        egui::Button::new(icon.rich_text().size(18.0).color(p.text_weak))
            .frame(false)
            .min_size(egui::vec2(26.0, 26.0)),
    )
    .accessible_name(label)
    .on_hover_text(label)
}

/// A link-styled button (accent text, no frame).
pub fn link_button(ui: &mut Ui, label: &str) -> Response {
    let p = theme::palette(ui);
    ui.add(egui::Button::new(RichText::new(label).color(p.accent)).frame(false))
        .accessible_name(label)
}

/// A toggle switch named `label`. Returns the response (`changed()` when flipped).
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let p = theme::palette(ui);
    let size = egui::vec2(36.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, ui.is_enabled(), *on, label));
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let enabled = ui.is_enabled();
        let off_fill = if p.dark {
            Color32::from_rgb(70, 77, 87)
        } else {
            Color32::from_rgb(196, 202, 210)
        };
        let fill = if enabled {
            lerp_color(off_fill, p.accent_fill, t)
        } else {
            p.surface_alt
        };
        let radius = rect.height() / 2.0;
        ui.painter().rect_filled(rect, radius, fill);
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(2.0),
                radius + 2.0,
                Stroke::new(1.5, p.accent),
                egui::StrokeKind::Outside,
            );
        }
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        let knob = if enabled { Color32::WHITE } else { p.border };
        ui.painter()
            .circle_filled(egui::pos2(x, rect.center().y), radius - 3.0, knob);
    }
    response
}

/// A toggle with its label to the right; clicking the label flips it too.
pub fn toggle_labeled(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    ui.horizontal(|ui| {
        let mut response = toggle(ui, on, label);
        let text = ui.add(egui::Label::new(label).sense(Sense::click()));
        if text.clicked() {
            *on = !*on;
            response.mark_changed();
        }
        response
    })
    .inner
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// A banner: an icon, a message, and actions on the right.
pub fn banner<R>(ui: &mut Ui, tone: Tone, message: &str, add_actions: impl FnOnce(&mut Ui) -> R) -> R {
    let p = theme::palette(ui);
    Frame::new()
        .fill(tone.soft(p))
        .stroke(Stroke::new(1.0, tone.color(p).gamma_multiply(0.5)))
        .corner_radius(theme::CARD_RADIUS)
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // The message wraps in the room the actions leave; their width is remembered from the
            // previous frame.
            let memory = ui.auto_id_with("banner-actions-width");
            let reserved = ui.data(|data| data.get_temp::<f32>(memory)).unwrap_or(0.0);
            ui.horizontal_top(|ui| {
                ui.label(tone.icon().rich_text().size(18.0).color(tone.color(p)));
                let spacing = ui.spacing().item_spacing.x;
                let text_width = (ui.available_width() - reserved - spacing).max(120.0);
                let text =
                    ui.allocate_ui_with_layout(egui::vec2(text_width, 0.0), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(text_width);
                        ui.add_space(1.0);
                        ui.add(egui::Label::new(RichText::new(message).color(p.text)).wrap());
                    });
                let row = egui::vec2(ui.available_width(), text.response.rect.height().max(22.0));
                let actions = ui.allocate_ui_with_layout(row, Layout::right_to_left(Align::Center), add_actions);
                let used = actions.response.rect.width();
                if (used - reserved).abs() > 0.5 {
                    ui.data_mut(|data| data.insert_temp(memory, used));
                    ui.ctx().request_repaint();
                }
                actions.inner
            })
            .inner
        })
        .inner
}

/// An inline message under a field (an error or a hint).
pub fn inline_message(ui: &mut Ui, tone: Tone, message: &str) {
    let p = theme::palette(ui);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(tone.icon().rich_text().size(15.0).color(tone.color(p)));
        ui.add(egui::Label::new(RichText::new(message).color(tone.color(p))).wrap());
    });
}

/// An empty state: an icon, a title, an explanation and an optional action. Returns whether the
/// action was clicked.
pub fn empty_state(ui: &mut Ui, icon: MaterialIcon, title: &str, text: &str, action: Option<&str>) -> bool {
    let p = theme::palette(ui);
    let mut clicked = false;
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.25).clamp(24.0, 160.0));
        ui.label(icon.rich_text().size(44.0).color(p.text_weak));
        ui.add_space(theme::GRID);
        ui.label(theme::semibold(title).size(18.0).color(p.text));
        ui.add_space(4.0);
        ui.set_max_width(460.0);
        ui.label(RichText::new(text).color(p.text_weak));
        if let Some(action) = action {
            ui.add_space(theme::GRID * 2.0);
            clicked = primary_button(ui, true, None, action, 32.0).clicked();
        }
    });
    clicked
}

/// A segmented control: one button per option, the current one highlighted.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, options: &[(T, &str)]) -> bool {
    let p = theme::palette(ui);
    let mut changed = false;
    Frame::new()
        .fill(p.surface_alt)
        .corner_radius(theme::RADIUS + 2)
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (value, label) in options {
                    let selected = *current == *value;
                    let text = if selected {
                        theme::semibold(*label).color(p.text)
                    } else {
                        RichText::new(*label).color(p.text_weak)
                    };
                    let mut button = egui::Button::new(text).min_size(egui::vec2(0.0, 24.0));
                    button = if selected {
                        button.fill(p.surface).stroke(Stroke::new(1.0, p.border))
                    } else {
                        button.frame_when_inactive(false)
                    };
                    let response = ui.add(button).accessible_name(*label);
                    response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, selected, *label));
                    if response.clicked() && !selected {
                        *current = *value;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// [`segmented`] with a count after each label. Each option is named by its label alone.
pub fn segmented_counts<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, options: &[(T, &str, u64)]) -> bool {
    let p = theme::palette(ui);
    let mut changed = false;
    Frame::new()
        .fill(p.surface_alt)
        .corner_radius(theme::RADIUS + 2)
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (value, label, count) in options {
                    let selected = *current == *value;
                    let text = if selected {
                        theme::semibold(*label).color(p.text)
                    } else {
                        RichText::new(*label).color(p.text_weak)
                    };
                    let count = RichText::new(format::count(*count)).size(12.0).color(p.text_weak);
                    let mut button = egui::Button::new((text, count)).min_size(egui::vec2(0.0, 24.0));
                    button = if selected {
                        button.fill(p.surface).stroke(Stroke::new(1.0, p.border))
                    } else {
                        button.frame_when_inactive(false)
                    };
                    let response = ui.add(button).accessible_name(*label);
                    response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, selected, *label));
                    if response.clicked() && !selected {
                        *current = *value;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// `text` with every case-insensitive occurrence of `query` highlighted.
pub fn highlighted(text: &str, query: &str, font: FontId, color: Color32, highlight: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    append_marked(&mut job, text, query, &TextFormat::simple(font, color), highlight);
    job
}

/// Appends `text` in `format`, the occurrences of `query` on the `highlight` background.
fn append_marked(job: &mut LayoutJob, text: &str, query: &str, format: &TextFormat, highlight: Color32) {
    let mut at = 0;
    for (start, end) in match_ranges(text, query) {
        if start > at {
            job.append(&text[at..start], 0.0, format.clone());
        }
        let marked = TextFormat {
            background: highlight,
            ..format.clone()
        };
        job.append(&text[start..end], 0.0, marked);
        at = end;
    }
    if at < text.len() {
        job.append(&text[at..], 0.0, format.clone());
    }
}

/// A CLI help text for display: `` `code` `` spans are set in the monospace font on a faint
/// background (the backticks themselves dropped), and occurrences of `query` are highlighted.
pub fn described(text: &str, query: &str, size: f32, p: &Palette) -> LayoutJob {
    let prose = TextFormat::simple(FontId::proportional(size), p.text_weak);
    let code = TextFormat {
        background: p.surface_alt,
        ..TextFormat::simple(FontId::monospace(size - 0.5), p.text)
    };
    let mut job = LayoutJob::default();
    let parts: Vec<&str> = text.split('`').collect();
    // An odd number of backticks leaves the last one unpaired: it stays ordinary text.
    let paired = parts.len() % 2 == 1;
    for (index, part) in parts.iter().enumerate() {
        if index % 2 == 0 {
            append_marked(&mut job, part, query, &prose, p.highlight);
        } else if paired || index + 1 < parts.len() {
            append_marked(&mut job, part, query, &code, p.highlight);
        } else {
            job.append("`", 0.0, prose.clone());
            append_marked(&mut job, part, query, &prose, p.highlight);
        }
    }
    job
}

/// A CLI help text as plain words (for tooltips): the code backticks removed.
pub fn without_backticks(text: &str) -> String {
    text.replace('`', "")
}

/// Byte ranges of the case-insensitive occurrences of `query` in `text` (on char boundaries).
pub fn match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let lower_query: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let mut matched = 0;
        let mut j = i;
        let mut q = 0;
        while j < chars.len() && q < lower_query.len() {
            let lowered: Vec<char> = chars[j].1.to_lowercase().collect();
            if lower_query[q..].starts_with(&lowered) {
                q += lowered.len();
                j += 1;
                matched += 1;
            } else {
                break;
            }
        }
        if q == lower_query.len() && matched > 0 {
            let start = chars[i].0;
            let end = chars.get(j).map_or(text.len(), |(at, _)| *at);
            ranges.push((start, end));
            i = j;
        } else {
            i += 1;
        }
    }
    ranges
}

/// `text` on one line, cut with an ellipsis at `width`, for painting.
pub fn one_line(ui: &Ui, text: &str, font: FontId, color: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    clipped(ui, text, font, color, width, 1)
}

/// `text` wrapped at `width` into at most `rows` lines, the last cut with an ellipsis.
pub fn clipped(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
    rows: usize,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::simple(text.to_string(), font, color, width.max(10.0));
    job.wrap.max_rows = rows;
    job.wrap.break_anywhere = rows == 1;
    job.wrap.overflow_character = Some('…');
    ui.painter().layout_job(job)
}

/// A label in `text` colour that is selectable (paths, commands).
pub fn selectable_mono(ui: &mut Ui, text: &str) -> Response {
    ui.add(egui::Label::new(theme::mono(text)).selectable(true).wrap())
}

/// Text for widgets that need an accessible name as plain words.
pub fn plain(text: impl Into<WidgetText>) -> WidgetText {
    text.into()
}

/// A one-line label in a box `width` wide, aligned to its start (`Align::Min`) or end
/// (`Align::Max`), cut with an ellipsis when it does not fit. For columns of values in a row.
pub fn fixed_label(ui: &mut Ui, width: f32, text: RichText, align: Align) -> Response {
    let layout = match align {
        Align::Max => Layout::right_to_left(Align::Center),
        _ => Layout::left_to_right(Align::Center),
    };
    ui.allocate_ui_with_layout(egui::vec2(width, 20.0), layout, |ui| {
        ui.set_width(width);
        ui.add(egui::Label::new(text).truncate())
    })
    .inner
}

/// A horizontal hairline in the border colour.
pub fn hairline(ui: &mut Ui) {
    let p = theme::palette(ui);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, p.border));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_are_found_case_insensitively_on_char_boundaries() {
        assert_eq!(match_ranges("Max Workers and workers", "WORK"), vec![(4, 8), (16, 20)]);
        assert_eq!(match_ranges("Žluťoučký kůň", "ŤOU"), vec![(4, 8)]);
        assert!(match_ranges("abc", "  ").is_empty());
        assert!(match_ranges("abc", "abcd").is_empty());
        let job = highlighted(
            "a--workers b",
            "--work",
            FontId::default(),
            Color32::WHITE,
            Color32::RED,
        );
        assert_eq!(job.text, "a--workers b");
        assert_eq!(job.sections.len(), 3);
    }

    #[test]
    fn help_texts_show_code_spans_without_their_backticks() {
        let p = &theme::LIGHT;
        let job = described("Default is `0` (no limit).", "", 13.0, p);
        assert_eq!(job.text, "Default is 0 (no limit).");
        assert_eq!(job.sections.len(), 3);
        assert_eq!(job.sections[1].format.font_id, FontId::monospace(12.5));
        // A match inside a code span is highlighted there.
        let job = described("Values `desktop`, `mobile`.", "desk", 13.0, p);
        assert_eq!(job.text, "Values desktop, mobile.");
        assert!(job.sections.iter().any(|s| s.format.background == p.highlight));
        // An unpaired backtick stays in the text.
        assert_eq!(described("a `b` and ` c", "", 13.0, p).text, "a b and ` c");
        assert_eq!(without_backticks("use `--url`"), "use --url");
    }
}
