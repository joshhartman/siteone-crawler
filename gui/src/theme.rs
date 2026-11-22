//! Visuals (GUI-SPEC §6.6): light and dark palettes with one accent colour, fonts (Segoe UI and
//! Cascadia Mono / Consolas from the system when present, egui's built-in fonts otherwise), the
//! icon font, an 8 px spacing grid, and the theme / UI-scale switch of the settings.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText, Shadow, Stroke,
    TextStyle, Visuals,
};

use crate::model::settings::Theme;

/// The spacing grid.
pub const GRID: f32 = 8.0;
/// Corner radius of controls.
pub const RADIUS: u8 = 6;
/// Corner radius of cards and dialogs.
pub const CARD_RADIUS: u8 = 8;

/// Font family of the semibold text face (headings, labels that carry weight).
pub const SEMIBOLD: &str = "semibold";

/// The semantic colours of one theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    /// Page background.
    pub bg: Color32,
    /// Cards, dialogs.
    pub surface: Color32,
    /// Subtle fills: the navigation rail, the command bar, hovered rows.
    pub surface_alt: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    /// Accent as text, icons and outlines.
    pub accent: Color32,
    /// Accent as a button fill (contrast with `on_accent`).
    pub accent_fill: Color32,
    pub accent_fill_hover: Color32,
    pub on_accent: Color32,
    /// Selected rows and cards.
    pub accent_soft: Color32,
    pub success: Color32,
    pub info: Color32,
    pub warning: Color32,
    pub danger: Color32,
    /// Background of a search match.
    pub highlight: Color32,
    /// Background of text fields.
    pub field: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const LIGHT: Palette = Palette {
    dark: false,
    bg: rgb(245, 246, 248),
    surface: rgb(255, 255, 255),
    surface_alt: rgb(237, 239, 242),
    border: rgb(220, 224, 229),
    border_strong: rgb(190, 197, 205),
    text: rgb(31, 35, 40),
    text_weak: rgb(93, 102, 112),
    accent: rgb(37, 96, 200),
    accent_fill: rgb(47, 111, 219),
    accent_fill_hover: rgb(37, 96, 200),
    on_accent: rgb(255, 255, 255),
    accent_soft: rgb(227, 236, 251),
    success: rgb(26, 127, 55),
    info: rgb(9, 105, 218),
    warning: rgb(154, 103, 0),
    danger: rgb(207, 34, 46),
    highlight: rgb(255, 229, 143),
    field: rgb(255, 255, 255),
};

pub const DARK: Palette = Palette {
    dark: true,
    bg: rgb(21, 23, 27),
    surface: rgb(29, 32, 37),
    surface_alt: rgb(35, 39, 45),
    border: rgb(52, 58, 66),
    border_strong: rgb(74, 82, 92),
    text: rgb(230, 232, 235),
    text_weak: rgb(154, 163, 173),
    accent: rgb(110, 165, 255),
    accent_fill: rgb(52, 112, 214),
    accent_fill_hover: rgb(66, 126, 228),
    on_accent: rgb(255, 255, 255),
    accent_soft: rgb(30, 45, 69),
    success: rgb(63, 185, 80),
    info: rgb(88, 166, 255),
    warning: rgb(210, 153, 34),
    danger: rgb(248, 81, 73),
    highlight: rgb(107, 83, 0),
    field: rgb(17, 19, 23),
};

/// The palette of the theme `ui` is drawn in.
pub fn palette(ui: &egui::Ui) -> &'static Palette {
    palette_for(ui.visuals().dark_mode)
}

pub fn palette_for(dark: bool) -> &'static Palette {
    if dark { &DARK } else { &LIGHT }
}

/// Text in the semibold face.
pub fn semibold(text: impl Into<String>) -> RichText {
    RichText::new(text).family(FontFamily::Name(SEMIBOLD.into()))
}

/// A page title.
pub fn page_title(text: impl Into<String>) -> RichText {
    semibold(text).size(22.0)
}

/// A card or section title.
pub fn section_title(text: impl Into<String>) -> RichText {
    semibold(text).size(15.0)
}

/// Monospace text (option names, commands, paths).
pub fn mono(text: impl Into<String>) -> RichText {
    RichText::new(text).monospace()
}

/// The font of [`semibold`] at `size`.
pub fn semibold_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Loads the system fonts (when present) and registers the icon font. Idempotent per context.
pub fn install_fonts(ctx: &egui::Context) {
    let marker = egui::Id::unique("siteone-fonts-installed");
    if ctx.data(|data| data.get_temp::<bool>(marker)).unwrap_or(false) {
        return;
    }
    ctx.data_mut(|data| data.insert_temp(marker, true));

    let mut fonts = FontDefinitions::default();
    let dir = fonts_dir();
    let add = |fonts: &mut FontDefinitions, key: &str, files: &[&str]| -> bool {
        let Some(bytes) = files
            .iter()
            .find_map(|file| dir.as_ref().and_then(|dir| std::fs::read(dir.join(file)).ok()))
        else {
            return false;
        };
        fonts
            .font_data
            .insert(key.to_string(), Arc::new(FontData::from_owned(bytes)));
        true
    };

    // The icon font: a family of its own (icons are always drawn in it, never inside a text run),
    // and a fallback of the text families ahead of Segoe UI Symbol, whose private-use glyphs must
    // never stand in for an icon.
    let icons = egui_material_icons::font_insert();
    let icon_font = icons.name.clone();
    fonts.font_data.insert(icon_font.clone(), Arc::new(icons.data));
    fonts.families.insert(
        FontFamily::Name(egui_material_icons::FONT_FAMILY.into()),
        vec![icon_font.clone()],
    );

    let base = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut proportional = base.clone();
    let mut semibold = base;
    if add(&mut fonts, "segoe-ui", &["segoeui.ttf"]) {
        proportional.insert(0, "segoe-ui".to_string());
        // Semibold falls back to the regular face for glyphs it lacks.
        semibold.insert(0, "segoe-ui".to_string());
    }
    if add(&mut fonts, "segoe-ui-semibold", &["seguisb.ttf"]) {
        semibold.insert(0, "segoe-ui-semibold".to_string());
    }
    let mut monospace = fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    if add(&mut fonts, "code", &["CascadiaMono.ttf", "consola.ttf"]) {
        monospace.insert(0, "code".to_string());
    }
    let has_symbols = add(&mut fonts, "segoe-ui-symbol", &["seguisym.ttf"]);
    for family in [&mut proportional, &mut semibold, &mut monospace] {
        family.push(icon_font.clone());
        // Segoe UI Symbol covers arrows and symbols the text faces lack.
        if has_symbols {
            family.push("segoe-ui-symbol".to_string());
        }
    }
    fonts.families.insert(FontFamily::Proportional, proportional);
    fonts.families.insert(FontFamily::Monospace, monospace);
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), semibold);
    ctx.set_fonts(fonts);
}

fn fonts_dir() -> Option<PathBuf> {
    let windows = std::env::var_os("WINDIR").or_else(|| std::env::var_os("SystemRoot"))?;
    Some(PathBuf::from(windows).join("Fonts"))
}

/// Applies the theme and UI scale of the settings, and the visuals of both themes.
pub fn apply(ctx: &egui::Context, theme: Theme, ui_scale: f32) {
    ctx.set_visuals_of(egui::Theme::Light, visuals(&LIGHT));
    ctx.set_visuals_of(egui::Theme::Dark, visuals(&DARK));
    ctx.all_styles_mut(style);
    ctx.set_theme(match theme {
        Theme::System => egui::ThemePreference::System,
        Theme::Light => egui::ThemePreference::Light,
        Theme::Dark => egui::ThemePreference::Dark,
    });
    if (ctx.zoom_factor() - ui_scale).abs() > f32::EPSILON {
        ctx.set_zoom_factor(ui_scale);
    }
}

fn style(style: &mut egui::Style) {
    use TextStyle::*;
    style.text_styles = [
        (Small, FontId::proportional(12.0)),
        (Body, FontId::proportional(14.0)),
        (Button, FontId::proportional(14.0)),
        (Heading, semibold_font(20.0)),
        (Monospace, FontId::monospace(13.0)),
    ]
    .into();
    let spacing = &mut style.spacing;
    spacing.item_spacing = egui::vec2(GRID, 6.0);
    spacing.button_padding = egui::vec2(12.0, 5.0);
    spacing.interact_size = egui::vec2(40.0, 28.0);
    spacing.menu_margin = Margin::same(6);
    spacing.window_margin = Margin::same(20);
    spacing.combo_width = 180.0;
    spacing.text_edit_width = 280.0;
    spacing.icon_width = 16.0;
    spacing.icon_width_inner = 10.0;
    spacing.tooltip_width = 420.0;
    spacing.scroll = egui::style::ScrollStyle::thin();
    style.interaction.tooltip_delay = 0.35;
    style.url_in_tooltip = false;
}

fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    let radius = CornerRadius::same(RADIUS);
    v.override_text_color = None;
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(CARD_RADIUS);
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(if p.dark { 110 } else { 40 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 3],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(if p.dark { 90 } else { 30 }),
    };
    v.extreme_bg_color = p.field;
    v.text_edit_bg_color = Some(p.field);
    v.faint_bg_color = p.surface_alt;
    v.code_bg_color = p.surface_alt;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = if p.dark { rgb(36, 64, 107) } else { rgb(207, 224, 251) };
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.striped = false;
    v.slider_trailing_fill = true;
    v.indent_has_left_vline = false;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;

    let (button, button_hover, button_active) = if p.dark {
        (rgb(42, 47, 54), rgb(50, 56, 65), rgb(58, 65, 75))
    } else {
        (rgb(240, 242, 245), rgb(231, 234, 238), rgb(221, 225, 230))
    };
    w.inactive.bg_fill = button;
    w.inactive.weak_bg_fill = button;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = radius;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = button_hover;
    w.hovered.weak_bg_fill = button_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = button_active;
    w.active.weak_bg_fill = button_active;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open.bg_fill = button_hover;
    w.open.weak_bg_fill = button_hover;
    w.open.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = radius;
    v
}
