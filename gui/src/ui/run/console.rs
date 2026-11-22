//! The Console tab: the crawler's output in a monospace view with its colours (mapped to palette
//! colours that stay readable in both themes), virtualized, with auto-scroll that lets go when the
//! user scrolls up, a text filter, Copy all, and a footer with skipped event lines.

use eframe::egui::{self, Align, Color32, FontId, Frame, Layout, Margin, Stroke, TextFormat, Ui, text::LayoutJob};
use egui_material_icons::icons;

use super::{RunAction, RunView, padded};
use crate::model::console::{AnsiColor, ConsoleBuffer, Style, StyledLine};
use crate::theme::{self, Palette};
use crate::ui::widgets::table::contains_ignore_case;
use crate::ui::widgets::{self, format};

const FONT_SIZE: f32 = 12.5;

/// UI state of the Console tab.
#[derive(Debug, Default)]
pub struct ConsoleUi {
    pub filter: String,
    /// Absolute numbers (dropped lines included) of the lines that match the filter.
    matches: Vec<u64>,
    /// Lines looked at so far (absolute), and the filter they were looked at for.
    scanned: u64,
    matched_for: String,
}

impl ConsoleUi {
    /// Brings the matches up to date: only new lines are looked at.
    fn sync(&mut self, console: &ConsoleBuffer) {
        let needle = self.filter.trim().to_lowercase();
        let dropped = console.dropped();
        let total = dropped + console.len() as u64;
        if needle != self.matched_for || total < self.scanned {
            self.matches.clear();
            self.scanned = dropped;
            self.matched_for = needle.clone();
        }
        if needle.is_empty() {
            self.scanned = total;
            return;
        }
        let start = self.scanned.max(dropped);
        let lines = console.lines();
        for absolute in start..total {
            if let Some(line) = usize::try_from(absolute - dropped).ok().and_then(|at| lines.get(at))
                && line_matches(line, &needle)
            {
                self.matches.push(absolute);
            }
        }
        self.scanned = total;
        let first_kept = self.matches.partition_point(|absolute| *absolute < dropped);
        self.matches.drain(..first_kept);
    }
}

fn line_matches(line: &StyledLine, needle: &str) -> bool {
    // A match inside one span needs no allocation; one across spans needs the whole text.
    line.spans.iter().any(|span| contains_ignore_case(&span.text, needle))
        || (line.spans.len() > 1 && contains_ignore_case(&line.text(), needle))
}

pub fn show(ui: &mut Ui, view: &RunView<'_>, console: &mut ConsoleUi, focus: bool, actions: &mut Vec<RunAction>) {
    let p = theme::palette(ui);
    let buffer = &view.state.console;
    console.sync(buffer);
    padded(ui, |ui| {
        let filtering = !console.filter.trim().is_empty();
        ui.horizontal(|ui| {
            ui.label(icons::ICON_SEARCH.rich_text().size(18.0).color(p.text_weak));
            let response = ui
                .add(
                    egui::TextEdit::singleline(&mut console.filter)
                        .hint_text("Filter lines   (Ctrl+F)")
                        .desired_width(280.0)
                        .margin(egui::vec2(8.0, 5.0)),
                )
                .accessible_name("Filter console");
            if focus {
                response.request_focus();
            }
            if filtering {
                ui.label(widgets::weak(
                    ui,
                    format!("{} matching", format::plural(console.matches.len() as u64, "line")),
                ));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let copy = ui
                    .add_enabled(
                        !buffer.is_empty(),
                        egui::Button::new(widgets::with_icon(icons::ICON_CONTENT_COPY, "Copy all")),
                    )
                    .accessible_name("Copy all")
                    .on_hover_text("Copy the whole output as plain text");
                if copy.clicked() {
                    actions.push(RunAction::Copy {
                        text: buffer.plain_text(),
                        what: "the console output".to_string(),
                    });
                }
            });
        });
        ui.add_space(theme::GRID);
        let footer_height = 22.0;
        let height = (ui.available_height() - footer_height - 4.0).max(80.0);
        let row_height = ui.fonts_mut(|fonts| fonts.row_height(&FontId::monospace(FONT_SIZE)));
        let count = if filtering { console.matches.len() } else { buffer.len() };
        Frame::new()
            .fill(if p.dark {
                Color32::from_rgb(16, 18, 21)
            } else {
                p.surface
            })
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(theme::CARD_RADIUS)
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(height - 16.0);
                if count == 0 {
                    ui.label(widgets::weak(
                        ui,
                        if filtering {
                            "No line matches the filter."
                        } else if view.live {
                            "Waiting for the crawler's output…"
                        } else {
                            "The crawler printed nothing."
                        },
                    ));
                    return;
                }
                let dropped = buffer.dropped();
                egui::ScrollArea::both()
                    .id_salt("run-console")
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show_rows(ui, row_height, count, |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for row in range {
                            let line = if filtering {
                                console
                                    .matches
                                    .get(row)
                                    .and_then(|absolute| usize::try_from(absolute.saturating_sub(dropped)).ok())
                                    .and_then(|at| buffer.lines().get(at))
                            } else {
                                buffer.lines().get(row)
                            };
                            let Some(line) = line else {
                                continue;
                            };
                            ui.add(egui::Label::new(layout(line, p)).extend());
                        }
                    });
            });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let mut parts = vec![format::plural(buffer.len() as u64, "line")];
            if buffer.dropped() > 0 {
                parts.push(format!(
                    "{} older lines are only in console.log",
                    format::count(buffer.dropped())
                ));
            }
            if view.state.malformed_events > 0 {
                parts.push(format!(
                    "{} skipped (not readable)",
                    format::plural(view.state.malformed_events, "event line")
                ));
            }
            ui.label(widgets::weak(ui, parts.join("   ·   ")).size(12.0));
        });
    });
}

/// A console line as a layout job in palette colours.
fn layout(line: &StyledLine, p: &Palette) -> LayoutJob {
    let mut job = LayoutJob::default();
    if line.spans.is_empty() {
        job.append(" ", 0.0, TextFormat::simple(FontId::monospace(FONT_SIZE), p.text));
    }
    for span in &line.spans {
        job.append(&span.text, 0.0, format_for(&span.style, p));
    }
    job
}

fn format_for(style: &Style, p: &Palette) -> TextFormat {
    let mut color = style.fg.map_or(p.text, |fg| ansi_color(fg, p));
    let mut background = Color32::TRANSPARENT;
    if style.inverse {
        background = color;
        color = style.bg.map_or(p.surface, |bg| ansi_color(bg, p));
    }
    if style.dim {
        color = color.gamma_multiply(0.7);
    }
    TextFormat {
        font_id: FontId::monospace(FONT_SIZE),
        color,
        background,
        italics: style.italic,
        underline: if style.underline {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        },
        ..TextFormat::default()
    }
}

/// A terminal colour as a palette colour readable on the console's background.
pub fn ansi_color(color: AnsiColor, p: &Palette) -> Color32 {
    match color {
        AnsiColor::Standard(index) => standard(index % 16, p),
        AnsiColor::Indexed(index) if index < 16 => standard(index, p),
        AnsiColor::Indexed(index) => readable(xterm(index), p),
        AnsiColor::Rgb(r, g, b) => readable(Color32::from_rgb(r, g, b), p),
    }
}

fn standard(index: u8, p: &Palette) -> Color32 {
    let (magenta, cyan) = if p.dark {
        (Color32::from_rgb(210, 140, 240), Color32::from_rgb(90, 200, 210))
    } else {
        (Color32::from_rgb(140, 50, 170), Color32::from_rgb(0, 120, 135))
    };
    match index % 8 {
        0 => p.text_weak,
        1 => p.danger,
        2 => p.success,
        3 => p.warning,
        4 => p.info,
        5 => magenta,
        6 => cyan,
        // "White" would vanish on a light background: the normal text colour.
        _ => p.text,
    }
}

/// The xterm 256-colour palette above the 16 standard colours.
fn xterm(index: u8) -> Color32 {
    if index >= 232 {
        let level = 8 + (index - 232) * 10;
        return Color32::from_rgb(level, level, level);
    }
    let cube = index - 16;
    let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
    Color32::from_rgb(level(cube / 36), level((cube / 6) % 6), level(cube % 6))
}

/// Darkens light colours on a light background and lightens dark ones on a dark background.
fn readable(color: Color32, p: &Palette) -> Color32 {
    let luminance = 0.299 * f32::from(color.r()) + 0.587 * f32::from(color.g()) + 0.114 * f32::from(color.b());
    let scale = |factor: f32, toward: f32| {
        let mix = |channel: u8| (f32::from(channel) + (toward - f32::from(channel)) * factor).round() as u8;
        Color32::from_rgb(mix(color.r()), mix(color.g()), mix(color.b()))
    };
    if p.dark && luminance < 110.0 {
        scale(0.45, 255.0)
    } else if !p.dark && luminance > 150.0 {
        scale(0.45, 0.0)
    } else {
        color
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::console::Stream;
    use crate::theme::{DARK, LIGHT};

    #[test]
    fn colours_stay_readable_in_both_themes() {
        // White text in a light theme becomes the normal text colour; black in a dark theme too.
        assert_eq!(ansi_color(AnsiColor::Standard(7), &LIGHT), LIGHT.text);
        assert_eq!(ansi_color(AnsiColor::Standard(0), &DARK), DARK.text_weak);
        assert_eq!(ansi_color(AnsiColor::Standard(1), &DARK), DARK.danger);
        let yellow = ansi_color(AnsiColor::Rgb(255, 255, 0), &LIGHT);
        assert!(yellow.r() < 200, "{yellow:?}");
        let navy = ansi_color(AnsiColor::Indexed(17), &DARK);
        assert!(navy.b() > 150, "{navy:?}");
    }

    #[test]
    fn the_filter_only_scans_new_lines_and_follows_dropped_ones() {
        let mut buffer = ConsoleBuffer::with_capacity(3);
        let mut ui = ConsoleUi {
            filter: "err".to_string(),
            ..ConsoleUi::default()
        };
        buffer.push(StyledLine::plain(Stream::Stdout, "ok"));
        buffer.push(StyledLine::plain(Stream::Stdout, "ERROR one"));
        ui.sync(&buffer);
        assert_eq!(ui.matches, vec![1]);
        buffer.push(StyledLine::plain(Stream::Stdout, "error two"));
        buffer.push(StyledLine::plain(Stream::Stdout, "fine"));
        buffer.push(StyledLine::plain(Stream::Stdout, "fine"));
        ui.sync(&buffer);
        assert_eq!(ui.matches, vec![2], "line 1 was dropped");
    }
}
