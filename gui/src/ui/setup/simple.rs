//! The Simple view: preset cards, then collapsible sections with the most used options under plain
//! labels (GUI-SPEC §6.2). Every control edits exactly one CLI option; its name is in the tooltip.

use eframe::egui::{
    self, Align, Frame, Layout, Margin, RichText, Role, ScrollArea, Sense, Stroke, Ui, WidgetInfo,
    collapsing_header::CollapsingState,
};
use egui_material_icons::{MaterialIcon, icons};
use siteone_crawler::options::option_type::OptionType;

use super::editor::{self, Editing};
use super::{PAGE_MARGIN, ai_panel, ops};
use crate::app::App;
use crate::model::config::{self, Value};
use crate::model::presets;
use crate::model::schema::{OptionSpec, PathRole};
use crate::theme;
use crate::ui::widgets;

const CARD_MIN_WIDTH: f32 = 210.0;
const CARD_HEIGHT: f32 = 76.0;
/// Width of text controls in the sections (less where the section is narrow).
const CONTROL_WIDTH: f32 = 380.0;
/// From this content width on, the sections are laid out in two columns.
const TWO_COLUMNS_FROM: f32 = 1040.0;
/// The preset that only means "the defaults": not offered as a card (Reset all does that).
const CUSTOM_PRESET: &str = "Custom";

/// The icon of a preset card.
fn preset_icon(name: &str) -> MaterialIcon {
    match name {
        "Quick Audit" => icons::ICON_BOLT,
        "SEO Analysis" => icons::ICON_QUERY_STATS,
        "Performance Test" => icons::ICON_SPEED,
        "Security Check" => icons::ICON_SHIELD,
        "Offline Clone" => icons::ICON_DOWNLOAD_FOR_OFFLINE,
        "Markdown Export" => icons::ICON_DESCRIPTION,
        "Stress Test" => icons::ICON_LOCAL_FIRE_DEPARTMENT,
        "Single Page" => icons::ICON_ARTICLE,
        "Large Site Crawl" => icons::ICON_HUB,
        "Sitemap Generator" => icons::ICON_ACCOUNT_TREE,
        _ => icons::ICON_TUNE,
    }
}

/// A field of a section: option, plain label, and a short note after the control (may be empty).
type Field = (&'static str, &'static str, &'static str);

const SCOPE: &[Field] = &[
    ("--single-page", "Crawl only the start page", ""),
    ("--max-depth", "Maximum link depth", "0 = no limit"),
    ("--max-visited-urls", "Maximum number of URLs", ""),
    (
        "--link-details",
        "Record every referring page",
        "link text and line · larger result on big sites",
    ),
    ("--allowed-domain-for-crawling", "Also crawl these domains", ""),
    ("--include-regex", "Only URLs matching", ""),
    ("--ignore-regex", "Skip URLs matching", ""),
    ("--ignore-robots-txt", "Ignore robots.txt", ""),
];

/// What the "Check external links" switch does (its tooltip). It is a setting of the app, not of
/// the crawler: the crawler never requests anything on other sites (GUI-SPEC §4.7).
pub const EXTERNAL_LINKS_TOOLTIP: &str = "After the crawl, the app requests each link to another website once (redirects followed) and records whether it answers without an HTTP error. Only the title of a linked HTML page is read; its files and links are never loaded. Images, scripts and styles from other websites are not checked.\n\nThe crawler has no equivalent option: this is not part of the command.";
/// How many leading [`SCOPE`] rows come before the "Check external links" switch.
const SCOPE_ROWS_BEFORE_EXTERNAL: usize = 3;

/// What the "Skip cart and action links" switch does (its tooltip; the patterns follow).
const ACTION_LINKS_TOOLTIP: &str = "The crawler does not request links that change something on the site when they are opened: add to cart, remove from the cart and undo (WooCommerce), log out (WordPress). Each such request can take seconds and adds nothing to a link check. They are not counted either.\n\nAdds these patterns to --ignore-regex, after your own:";

const SPEED: &[Field] = &[
    ("--workers", "Parallel requests", ""),
    ("--max-reqs-per-sec", "Requests per second", "at most"),
    ("--timeout", "Request timeout", "seconds"),
    ("--device", "Device to emulate", "sets the User-Agent"),
];

/// "Download" chips: each is the inverse of a `--disable-*` option.
const CONTENT: &[(&str, &str)] = &[
    ("--disable-javascript", "Scripts"),
    ("--disable-styles", "Styles"),
    ("--disable-fonts", "Fonts"),
    ("--disable-images", "Images"),
    ("--disable-files", "Documents and other files"),
];

const ACCESS: &[Field] = &[
    ("--http-auth", "HTTP authentication", ""),
    ("--header", "Extra HTTP headers", ""),
    ("--proxy", "Proxy", ""),
    ("--accept-invalid-certs", "Accept invalid TLS certificates", ""),
];

const BROWSER: &[Field] = &[
    ("--browser", "Render pages in Chromium", ""),
    ("--screenshots", "Screenshot of every page", "needs Chromium rendering"),
    ("--screenshot-viewport", "Screenshot sizes", ""),
    ("--screenshot-mode", "Screenshot area", ""),
];

/// Exports: (option, label, the value a switch puts in — relative to the run folder).
const EXPORTS: &[(&str, &str, &str)] = &[
    ("--offline-export-dir", "Offline copy of the website", "offline"),
    ("--markdown-export-dir", "Markdown export", "markdown"),
    ("--sitemap-xml-file", "XML sitemap", "sitemap.xml"),
    ("--sitemap-txt-file", "Text sitemap", "sitemap.txt"),
];
const HTML_REPORT: &str = "--output-html-report";

pub fn show(app: &mut App, ui: &mut Ui) {
    ScrollArea::vertical()
        .id_salt("setup-simple")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::new()
                .inner_margin(Margin {
                    left: PAGE_MARGIN,
                    right: PAGE_MARGIN,
                    top: 4,
                    bottom: 24,
                })
                .show(ui, |ui| {
                    presets_grid(app, ui);
                    ui.add_space(theme::GRID * 2.0);
                    sections(app, ui);
                });
        });
}

/// The preset cards under a header that folds them away (small windows need the room).
fn presets_grid(app: &mut App, ui: &mut Ui) {
    let p = theme::palette(ui);
    let active = presets::matching(&app.session.config, &app.presets, &app.schema)
        .map(|preset| preset.name)
        .filter(|name| *name != CUSTOM_PRESET);
    let id = ui.make_persistent_id("setup-presets");
    // Folded by default in a short window, where the cards would fill the view.
    let roomy = ui.ctx().content_rect().height() >= 760.0;
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, roomy);
    let open = state.is_open();
    ui.horizontal(|ui| {
        let chevron = if open {
            icons::ICON_EXPAND_MORE
        } else {
            icons::ICON_CHEVRON_RIGHT
        };
        let header = ui
            .add(
                egui::Button::new((
                    chevron.rich_text().size(20.0).color(p.text_weak),
                    theme::section_title("Start from a preset").color(p.text),
                ))
                .frame(false),
            )
            .accessible_name("Presets");
        header.widget_info(|| WidgetInfo::selected(Role::DisclosureTriangle, true, open, "Presets"));
        if header.clicked() {
            state.toggle(ui);
        }
        if !open && let Some(name) = active {
            widgets::chip(ui, name, widgets::Tone::Accent);
        }
    });
    state.show_body_unindented(ui, |ui| {
        ui.add_space(2.0);
        preset_cards(app, ui, active);
    });
}

fn preset_cards(app: &mut App, ui: &mut Ui, active: Option<&'static str>) {
    let p = theme::palette(ui);
    let gap = theme::GRID;
    let width = ui.available_width();
    let columns = (((width + gap) / (CARD_MIN_WIDTH + gap)).floor() as usize).clamp(1, 6);
    let card_width = (width - gap * (columns as f32 - 1.0)) / columns as f32;
    let mut clicked = None;
    // (index in `app.presets`, preset) of the cards.
    let cards: Vec<_> = app
        .presets
        .iter()
        .enumerate()
        .filter(|(_, preset)| preset.name != CUSTOM_PRESET)
        .collect();
    for row in cards.chunks(columns) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (index, preset) in row.iter().copied() {
                let selected = active == Some(preset.name);
                let (rect, response) = ui.allocate_exact_size(egui::vec2(card_width, CARD_HEIGHT), Sense::click());
                response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, preset.name));
                let response = response.on_hover_text(format!("{}\n\n{}", preset.name, preset.description));
                if ui.is_rect_visible(rect) {
                    let (fill, stroke) = if selected {
                        (p.accent_soft, Stroke::new(1.5, p.accent))
                    } else if response.hovered() {
                        (p.surface, Stroke::new(1.0, p.border_strong))
                    } else {
                        (p.surface, Stroke::new(1.0, p.border))
                    };
                    ui.painter()
                        .rect(rect, theme::CARD_RADIUS, fill, stroke, egui::StrokeKind::Inside);
                    if response.has_focus() {
                        ui.painter().rect_stroke(
                            rect.expand(2.0),
                            theme::CARD_RADIUS + 2,
                            Stroke::new(1.5, p.accent),
                            egui::StrokeKind::Outside,
                        );
                    }
                    let inner = rect.shrink2(egui::vec2(12.0, 10.0));
                    let name_color = if selected { p.accent } else { p.text };
                    let check = if selected { 22.0 } else { 0.0 };
                    let icon = preset_icon(preset.name);
                    let icon_color = if selected || response.hovered() {
                        p.accent
                    } else {
                        p.text_weak
                    };
                    ui.painter().text(
                        inner.left_top() + egui::vec2(0.0, 1.0),
                        egui::Align2::LEFT_TOP,
                        icon.codepoint,
                        egui::FontId::new(17.0, icon.font_family()),
                        icon_color,
                    );
                    let name_left = 25.0;
                    let name = widgets::one_line(
                        ui,
                        preset.name,
                        theme::semibold_font(14.0),
                        name_color,
                        inner.width() - check - name_left,
                    );
                    ui.painter()
                        .galley(inner.left_top() + egui::vec2(name_left, 0.0), name, name_color);
                    if selected {
                        ui.painter().text(
                            inner.right_top() + egui::vec2(0.0, 1.0),
                            egui::Align2::RIGHT_TOP,
                            icons::ICON_CHECK_CIRCLE.codepoint,
                            egui::FontId::new(16.0, icons::ICON_CHECK_CIRCLE.font_family()),
                            p.accent,
                        );
                    }
                    let description = widgets::clipped(
                        ui,
                        preset.description,
                        egui::FontId::proportional(12.5),
                        p.text_weak,
                        inner.width(),
                        2,
                    );
                    ui.painter()
                        .galley(inner.left_top() + egui::vec2(0.0, 24.0), description, p.text_weak);
                }
                if response.clicked() {
                    clicked = Some(index);
                }
            }
        });
        ui.add_space(gap - ui.spacing().item_spacing.y);
    }
    if let Some(index) = clicked {
        ops::choose_preset(app, index);
    }
}

/// The header of a collapsible section.
struct Section<'a> {
    id: &'a str,
    icon: MaterialIcon,
    title: &'a str,
    /// Shown at the right of the header (e.g. "2 changed").
    summary: Option<String>,
    default_open: bool,
}

impl<'a> Section<'a> {
    fn new(id: &'a str, icon: MaterialIcon, title: &'a str, summary: Option<String>, default_open: bool) -> Self {
        Self {
            id,
            icon,
            title,
            summary,
            default_open,
        }
    }
}

/// A collapsible card; `header_extra` adds controls at the right of the header.
fn section(ui: &mut Ui, header: Section<'_>, header_extra: impl FnOnce(&mut Ui), body: impl FnOnce(&mut Ui)) {
    let Section {
        id,
        icon,
        title,
        summary,
        default_open,
    } = header;
    let p = theme::palette(ui);
    let id = ui.make_persistent_id(("setup-section", id));
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(theme::CARD_RADIUS)
        .inner_margin(Margin::symmetric(16, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let open = state.is_open();
            ui.horizontal(|ui| {
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2((ui.available_width() - 220.0).max(160.0), 40.0),
                    Sense::click(),
                );
                response.widget_info(|| WidgetInfo::selected(Role::DisclosureTriangle, true, open, title));
                if response.clicked() {
                    state.toggle(ui);
                }
                let chevron = if open {
                    icons::ICON_EXPAND_MORE
                } else {
                    icons::ICON_CHEVRON_RIGHT
                };
                let painter = ui.painter();
                let y = rect.center().y;
                painter.text(
                    egui::pos2(rect.left() + 8.0, y),
                    egui::Align2::CENTER_CENTER,
                    chevron.codepoint,
                    egui::FontId::new(20.0, chevron.font_family()),
                    p.text_weak,
                );
                painter.text(
                    egui::pos2(rect.left() + 32.0, y),
                    egui::Align2::CENTER_CENTER,
                    icon.codepoint,
                    egui::FontId::new(18.0, icon.font_family()),
                    p.accent,
                );
                painter.text(
                    egui::pos2(rect.left() + 48.0, y),
                    egui::Align2::LEFT_CENTER,
                    title,
                    theme::semibold_font(15.0),
                    p.text,
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    header_extra(ui);
                    if let Some(summary) = summary {
                        widgets::chip(ui, summary, widgets::Tone::Accent);
                    }
                });
            });
            state.show_body_unindented(ui, |ui| {
                widgets::hairline(ui);
                ui.add_space(theme::GRID * 1.5);
                body(ui);
                ui.add_space(theme::GRID * 1.5);
            });
        });
    ui.add_space(theme::GRID * 1.5);
}

/// "n changed" of a section: its changed options plus `extra` changed app settings.
fn changed_summary(app: &App, names: &[&str], extra: usize) -> Option<String> {
    let count = extra
        + names
            .iter()
            .filter(|name| {
                app.schema.get(name).is_some_and(|spec| {
                    if spec.secret {
                        app.secrets.get(&spec.name).is_some()
                    } else {
                        app.session.config.is_changed(&app.schema, name)
                    }
                })
            })
            .count();
    (count > 0).then(|| format!("{count} changed"))
}

/// The field rows of a section; the CLI option name and help text are in the tooltip.
pub(super) fn rows(ui: &mut Ui, edit: &mut Editing<'_>, fields: &[Field]) {
    for (name, label, note) in fields {
        let Some(spec) = edit.schema.get(name).cloned() else {
            continue;
        };
        let changed = edit.is_changed(&spec);
        let tooltip = option_tooltip(&spec);
        widgets::field_row(ui, label, &tooltip, changed, |ui| {
            let width = control_width(ui);
            // A switch or a number is narrow: its note follows on the same line. Under a wider
            // control (dropdown, text) the note gets a line of its own instead of breaking mid-phrase.
            let narrow =
                spec.choices.is_none() && matches!(spec.kind, OptionType::Bool | OptionType::Int | OptionType::Float);
            if note.is_empty() || spec.is_list {
                editor::control(ui, edit, &spec, label, width).on_hover_text(&tooltip);
            } else if narrow {
                ui.horizontal(|ui| {
                    editor::control(ui, edit, &spec, label, width).on_hover_text(&tooltip);
                    ui.label(widgets::weak(ui, *note));
                });
            } else {
                editor::control(ui, edit, &spec, label, width).on_hover_text(&tooltip);
                ui.label(widgets::weak(ui, *note));
            }
        });
        ui.add_space(4.0);
    }
}

/// The tooltip of a control: the CLI option and its help text.
pub(super) fn option_tooltip(spec: &OptionSpec) -> String {
    format!("{}\n\n{}", spec.name, widgets::without_backticks(&spec.description))
}

/// The width for a text control in the room a field row leaves.
pub(super) fn control_width(ui: &Ui) -> f32 {
    CONTROL_WIDTH.min(ui.available_width()).max(140.0)
}

fn names(fields: &[Field]) -> Vec<&'static str> {
    fields.iter().map(|(name, _, _)| *name).collect()
}

/// The sections of plain crawl settings (everything but AI), in single-column order.
#[derive(Clone, Copy)]
enum Basic {
    Scope,
    Speed,
    Content,
    Access,
    Exports,
    Browser,
}

impl Basic {
    const ALL: [Basic; 6] = [
        Basic::Scope,
        Basic::Speed,
        Basic::Content,
        Basic::Access,
        Basic::Exports,
        Basic::Browser,
    ];

    /// The options the section edits (for its "n changed" summary).
    fn options(self) -> Vec<&'static str> {
        match self {
            Basic::Scope => names(SCOPE),
            Basic::Speed => names(SPEED),
            Basic::Content => CONTENT.iter().map(|(name, _)| *name).collect(),
            Basic::Access => names(ACCESS),
            Basic::Exports => EXPORTS
                .iter()
                .map(|(name, _, _)| *name)
                .chain(std::iter::once(HTML_REPORT))
                .collect(),
            Basic::Browser => names(BROWSER),
        }
    }
}

/// One of the plain sections. `roomy` is the two-column layout, where the short Content section
/// starts open.
fn basic_section(ui: &mut Ui, edit: &mut Editing<'_>, which: Basic, summary: Option<String>, roomy: bool) {
    let (id, icon, title, open) = match which {
        Basic::Scope => ("scope", icons::ICON_TRAVEL_EXPLORE, "Scope", true),
        Basic::Speed => ("speed", icons::ICON_SPEED, "Speed", true),
        Basic::Content => ("content", icons::ICON_DOWNLOAD, "Content", roomy),
        Basic::Access => ("access", icons::ICON_LOCK, "Access", false),
        Basic::Exports => ("exports", icons::ICON_INVENTORY_2, "Exports", true),
        Basic::Browser => ("browser", icons::ICON_WEB, "Browser rendering", false),
    };
    section(
        ui,
        Section::new(id, icon, title, summary, open),
        |_| {},
        |ui| match which {
            Basic::Scope => {
                let (before, after) = SCOPE.split_at(SCOPE_ROWS_BEFORE_EXTERNAL.min(SCOPE.len()));
                rows(ui, edit, before);
                external_links(ui, edit);
                action_links(ui, edit);
                rows(ui, edit, after);
            }
            Basic::Speed => rows(ui, edit, SPEED),
            Basic::Content => content(ui, edit),
            Basic::Access => rows(ui, edit, ACCESS),
            Basic::Exports => exports(ui, edit),
            Basic::Browser => rows(ui, edit, BROWSER),
        },
    );
}

fn sections(app: &mut App, ui: &mut Ui) {
    // The summaries read the configuration the editor borrows below.
    let summaries: Vec<Option<String>> = Basic::ALL
        .iter()
        .map(|which| {
            // The external link and action link switches are settings of the app in Scope.
            let config = &app.session.config;
            let extra = match which {
                Basic::Scope => usize::from(config.check_external_links) + usize::from(!config.skip_action_links),
                _ => 0,
            };
            changed_summary(app, &which.options(), extra)
        })
        .collect();
    let summary = |which: Basic| summaries.get(which as usize).cloned().flatten();
    // A wide window shows the sections side by side instead of one long, half-empty column.
    let two_columns = ui.available_width() >= TWO_COLUMNS_FROM;

    {
        let App {
            schema,
            session,
            secrets,
            setup,
            dialogs,
            ..
        } = app;
        let mut edit = Editing {
            schema,
            config: &mut session.config,
            secrets,
            state: &mut setup.editor,
            dialogs: &**dialogs,
        };
        if two_columns {
            ui.scope(|ui| {
                // The gutter between the columns only: the columns keep the normal spacing.
                let spacing = ui.spacing().item_spacing.x;
                ui.spacing_mut().item_spacing.x = theme::GRID * 1.5;
                ui.columns(2, |columns| {
                    if let [left, right] = columns {
                        left.spacing_mut().item_spacing.x = spacing;
                        right.spacing_mut().item_spacing.x = spacing;
                        for which in [Basic::Scope, Basic::Content, Basic::Access] {
                            basic_section(left, &mut edit, which, summary(which), true);
                        }
                        for which in [Basic::Speed, Basic::Exports, Basic::Browser] {
                            basic_section(right, &mut edit, which, summary(which), true);
                        }
                    }
                });
            });
        } else {
            for which in Basic::ALL {
                basic_section(ui, &mut edit, which, summary(which), false);
            }
        }
    }

    let mut ai_on = app.session.config.ai_enabled;
    let mut flipped = false;
    section(
        ui,
        Section::new("ai", icons::ICON_AUTO_AWESOME, "AI analysis", None, true),
        |ui| {
            flipped = widgets::toggle(ui, &mut ai_on, "Use AI analysis").changed();
            ui.label(widgets::weak(ui, if ai_on { "On" } else { "Off" }));
        },
        |ui| ai_panel::show(app, ui),
    );
    if flipped {
        app.session.config.ai_enabled = ai_on;
    }
}

/// "Check external links": a setting of the app (`check_external_links`), not a CLI option. After
/// the crawl the app requests each external link once and shows the answers in Link checker.
fn external_links(ui: &mut Ui, edit: &mut Editing<'_>) {
    let mut on = edit.config.check_external_links;
    widgets::field_row(ui, "Check external links", EXTERNAL_LINKS_TOOLTIP, on, |ui| {
        ui.horizontal(|ui| {
            if widgets::toggle(ui, &mut on, "Check external links")
                .on_hover_text(EXTERNAL_LINKS_TOOLTIP)
                .changed()
            {
                edit.config.check_external_links = on;
            }
            ui.label(widgets::weak(ui, "one request per link, after the crawl"));
        });
    });
    ui.add_space(4.0);
}

/// "Skip cart and action links" (`skip_action_links`): on, the run adds
/// [`config::ACTION_LINK_PATTERNS`] to `--ignore-regex`. On by default, so it is marked changed
/// when off.
fn action_links(ui: &mut Ui, edit: &mut Editing<'_>) {
    let label = "Skip cart and action links";
    let tooltip = format!("{ACTION_LINKS_TOOLTIP}\n{}", config::ACTION_LINK_PATTERNS.join("\n"));
    let mut on = edit.config.skip_action_links;
    widgets::field_row(ui, label, &tooltip, !on, |ui| {
        ui.horizontal(|ui| {
            if widgets::toggle(ui, &mut on, label).on_hover_text(&tooltip).changed() {
                edit.config.skip_action_links = on;
            }
            ui.label(widgets::weak(ui, "add to cart, log out, …"));
        });
    });
    ui.add_space(4.0);
}

fn content(ui: &mut Ui, edit: &mut Editing<'_>) {
    let tooltip = CONTENT
        .iter()
        .map(|(name, label)| format!("{label}: off = {name}"))
        .collect::<Vec<_>>()
        .join("\n");
    widgets::field_row(ui, "Download", &tooltip, false, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            for (name, label) in CONTENT {
                let mut download = !edit.flag(name);
                let description = edit
                    .schema
                    .get(name)
                    .map(|spec| spec.description.clone())
                    .unwrap_or_default();
                if widgets::check_chip(ui, &mut download, label)
                    .on_hover_text(format!("Off = {name}\n\n{description}"))
                    .clicked()
                {
                    edit.set(name, Value::Bool(!download));
                }
            }
        });
        ui.label(widgets::weak(
            ui,
            "Turned-off types are neither downloaded nor checked; HTML pages are always crawled.",
        ));
    });
}

fn exports(ui: &mut Ui, edit: &mut Editing<'_>) {
    let p = theme::palette(ui);
    // The HTML report is on by default; the GUI writes it into the run folder.
    if let Some(spec) = edit.schema.get(HTML_REPORT).cloned() {
        let mut on = !edit.text(HTML_REPORT).is_empty();
        let tooltip = option_tooltip(&spec);
        widgets::field_row(ui, "HTML report", &tooltip, edit.is_changed(&spec), |ui| {
            ui.horizontal(|ui| {
                if widgets::toggle(ui, &mut on, "HTML report").changed() {
                    if on {
                        edit.reset(&spec);
                    } else {
                        edit.set(HTML_REPORT, Value::Text(String::new()));
                    }
                }
                let note = if !on {
                    "Not written".to_string()
                } else if edit.is_changed(&spec) {
                    edit.text(HTML_REPORT)
                } else {
                    "report.html in the run folder".to_string()
                };
                ui.label(RichText::new(note).color(p.text_weak));
            });
        });
        ui.add_space(4.0);
    }
    for (name, label, fill) in EXPORTS {
        let Some(spec) = edit.schema.get(name).cloned() else {
            continue;
        };
        let mut on = !edit.text(name).is_empty();
        let tooltip = option_tooltip(&spec);
        widgets::field_row(ui, label, &tooltip, edit.is_changed(&spec), |ui| {
            ui.horizontal(|ui| {
                if widgets::toggle(ui, &mut on, label).changed() {
                    if on {
                        edit.set(name, Value::Text((*fill).to_string()));
                    } else {
                        edit.reset(&spec);
                    }
                }
                if on {
                    let mut text = edit.text(name);
                    // Room for the Browse… button after the field.
                    let field_width = (ui.available_width() - 96.0).clamp(100.0, CONTROL_WIDTH - 140.0);
                    let field = ui
                        .add(
                            egui::TextEdit::singleline(&mut text)
                                .desired_width(field_width)
                                .hint_text("relative paths are inside the run folder")
                                .margin(egui::vec2(8.0, 5.0)),
                        )
                        .accessible_name(format!("{label} location"))
                        .on_hover_text("A relative path is inside the run folder.");
                    if field.changed() {
                        edit.set(name, Value::Text(text.clone()));
                    }
                    let role = spec.path_role.unwrap_or(PathRole::Dir);
                    if ui
                        .add(egui::Button::new("Browse…"))
                        .accessible_name(format!("Browse for {label}"))
                        .clicked()
                        && let Some(path) = editor::browse_path(edit.dialogs, role, label, &text)
                    {
                        edit.set(name, Value::Text(path.display().to_string()));
                    }
                } else {
                    ui.label(RichText::new("Off").color(p.text_weak));
                }
            });
        });
        ui.add_space(4.0);
    }
}
