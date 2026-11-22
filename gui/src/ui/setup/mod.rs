//! The Setup page (GUI-SPEC §6.2): profile bar, URL bar with Start, the Simple view (presets,
//! sections, AI panel) and the All options view (every option, searchable), and the command
//! preview at the bottom.

mod ai_panel;
mod all_options;
pub mod editor;
pub mod ops;
mod preview;
mod profile_bar;
mod simple;
mod url_bar;

use eframe::egui::{self, CentralPanel, Frame, Margin, Panel, TextEdit, Ui};
use egui_material_icons::icons;

pub use ops::perform;

use crate::app::{App, CrawlerStatus};
use crate::backend::tools::{CheckResult, ModelList};
use crate::theme;
use crate::ui::widgets::{self, Tone};

/// Horizontal padding of the page.
pub const PAGE_MARGIN: i8 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SetupView {
    #[default]
    Simple,
    AllOptions,
}

/// The AI panel's tool calls.
#[derive(Debug, Default)]
pub struct AiTools {
    pub models: Option<Result<ModelList, String>>,
    pub loading_models: bool,
    pub check: Option<Result<(CheckResult, std::time::Duration), String>>,
    pub checking: bool,
}

/// UI state of the Setup page (not part of the configuration).
#[derive(Debug)]
pub struct SetupUi {
    pub view: SetupView,
    /// The cross-group option search.
    pub search: String,
    pub only_changed: bool,
    /// Group code selected in the All options view.
    pub group: Option<String>,
    /// Scroll the search results to this group's heading.
    pub scroll_to_group: Option<String>,
    pub preview_open: bool,
    /// The preview shows secrets in clear text.
    pub reveal_secrets: bool,
    /// The URL field was left once (its message shows from then on).
    pub url_touched: bool,
    /// The message of a failed Start (the CLI's validation message).
    pub start_error: Option<String>,
    pub focus_url: bool,
    pub editor: editor::EditorState,
    pub ai: AiTools,
}

impl Default for SetupUi {
    fn default() -> Self {
        Self {
            view: SetupView::Simple,
            search: String::new(),
            only_changed: false,
            group: None,
            scroll_to_group: None,
            preview_open: false,
            reveal_secrets: false,
            url_touched: false,
            start_error: None,
            focus_url: true,
            editor: editor::EditorState::default(),
            ai: AiTools::default(),
        }
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let p = theme::palette(ui);
    Panel::top("setup-header")
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new().fill(p.bg).inner_margin(Margin {
            left: PAGE_MARGIN,
            right: PAGE_MARGIN,
            top: 16,
            bottom: 10,
        }))
        .show(ui, |ui| {
            profile_bar::show(app, ui);
            ui.add_space(theme::GRID * 1.5);
            crawler_banner(app, ui);
            url_bar::show(app, ui);
            ui.add_space(theme::GRID * 1.5);
            view_row(app, ui);
        });
    Panel::bottom("setup-command")
        .resizable(false)
        .show_separator_line(false)
        .frame(
            Frame::new()
                .fill(p.surface)
                .stroke(egui::Stroke::new(1.0, p.border))
                .inner_margin(Margin::symmetric(PAGE_MARGIN, 10)),
        )
        .show(ui, |ui| preview::show(app, ui));
    CentralPanel::default().frame(Frame::new().fill(p.bg)).show(ui, |ui| {
        let searching = !app.setup.search.trim().is_empty();
        if app.setup.view == SetupView::AllOptions || searching {
            all_options::show(app, ui);
        } else {
            simple::show(app, ui);
        }
    });
}

/// The banner when the crawler binary is missing (GUI-SPEC §4.1).
fn crawler_banner(app: &mut App, ui: &mut Ui) {
    if app.crawler != CrawlerStatus::Missing {
        return;
    }
    let locate = widgets::banner(
        ui,
        Tone::Warning,
        "The crawler program (siteone-crawler.exe) was not found next to this app or on PATH. Choose it to start crawls.",
        |ui| {
            let locate = ui
                .add(egui::Button::new("Locate…"))
                .accessible_name("Locate the crawler")
                .clicked();
            let retry = widgets::link_button(ui, "Search again").clicked();
            (locate, retry)
        },
    );
    if locate.0 {
        app.locate_crawler();
    }
    if locate.1 {
        app.detect_crawler();
    }
    ui.add_space(theme::GRID * 1.5);
}

fn view_row(app: &mut App, ui: &mut Ui) {
    let p = theme::palette(ui);
    ui.horizontal(|ui| {
        let searching = !app.setup.search.trim().is_empty();
        let mut view = if searching {
            SetupView::AllOptions
        } else {
            app.setup.view
        };
        if widgets::segmented(
            ui,
            &mut view,
            &[(SetupView::Simple, "Simple"), (SetupView::AllOptions, "All options")],
        ) {
            app.setup.view = view;
            if view == SetupView::Simple {
                app.setup.search.clear();
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if searching && widgets::icon_button(ui, icons::ICON_CLOSE, "Clear search").clicked() {
                app.setup.search.clear();
            }
            let response = ui
                .add(
                    TextEdit::singleline(&mut app.setup.search)
                        .hint_text("Search all options   (Ctrl+F)")
                        .desired_width(300.0)
                        .margin(egui::vec2(8.0, 5.0)),
                )
                .accessible_name("Search options");
            ui.label(icons::ICON_SEARCH.rich_text().size(18.0).color(p.text_weak));
            if app.focus_search {
                app.focus_search = false;
                response.request_focus();
            }
            if response.changed() && !app.setup.search.trim().is_empty() {
                app.setup.view = SetupView::AllOptions;
            }
        });
    });
}
