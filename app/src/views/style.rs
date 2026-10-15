//! UI3's color themes (ui3.css `:root` variables and `.theme-*` overrides) applied to egui, plus fonts.
//!
//! The active theme follows the `ui3_color_theme` setting; "Auto" picks like UI3's `SetColorTheme`
//! (Blue Iris 6 for BI 6.x servers, Legacy UI3 for 4.x, else Blue Iris 5).

use egui::{Color32, CornerRadius, Stroke, Visuals};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const REC_RED: Color32 = Color32::from_rgb(0xE5, 0x39, 0x35);
pub const MOTION_ORANGE: Color32 = Color32::from_rgb(0xFF, 0x98, 0x00);
/// `.notificationCounter`
pub const COUNTER_RED: Color32 = Color32::from_rgb(0xFF, 0x00, 0x00);
pub const VIDEO_BG: Color32 = Color32::BLACK;

/// The subset of UI3's CSS variables the native UI uses.
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: &'static str,
    pub highlight: Color32,
    pub panel: Color32,
    pub cliplist: Color32,
    pub button: Color32,
    pub ptzpad: Color32,
    #[allow(dead_code)]
    pub ptzpad_disabled: Color32,
    pub text: Color32,
    pub text_hover: Color32,
    pub text_active: Color32,
    pub pctext: Color32,
    pub pctext_hover: Color32,
    pub profile_active: Color32,
    pub profile_pause: Color32,
    pub profile_stop: Color32,
    pub seek: Color32,
    pub progress_inner: Color32,
    pub progress_outer: Color32,
    pub status_inner: Color32,
    pub tab_hover: Color32,
    pub sysname_bg: Color32,
    pub ui_bg: Color32,
    pub ui_hover: Color32,
    pub ui_active: Color32,
    pub dd_bg: Color32,
    pub dd_hover: Color32,
    pub dd_active: Color32,
    pub dd_text: Color32,
    pub dd_blue: Color32,
    pub dd_blue_hover: Color32,
    pub dd_golden: Color32,
    pub dd_golden_hover: Color32,
    pub dialog_bg: Color32,
    pub dialog_border: Color32,
    pub dialog_text: Color32,
    pub dialog_title_bg: Color32,
    pub link: Color32,
    pub collapsible_bg: Color32,
    pub collapsible_hover: Color32,
    pub datetile: Color32,
    pub badge: Color32,
    pub badge_hover: Color32,
    pub badge_selected: Color32,
    pub timeline_bg: Color32,
    pub timeline_legend: Color32,
    pub timeline_center: Color32,
    pub separator: Color32,
}

/// CSS `hsl()` -> Color32.
pub fn hsl(h: f32, s: f32, l: f32) -> Color32 {
    let (s, l) = (s / 100.0, l / 100.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h % 360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let f = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(r), f(g), f(b))
}

const fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn white_a(a: f32) -> Color32 {
    Color32::from_white_alpha((a * 255.0) as u8)
}

fn blue_iris_6() -> Theme {
    let highlight = hsl(245.0, 38.0, 39.0);
    Theme {
        name: "Blue Iris 6",
        highlight,
        panel: rgb(0x0C0C0C),
        cliplist: rgb(0x101010),
        button: rgb(0xBBB1B0),
        ptzpad: rgb(0x6D6868),
        ptzpad_disabled: rgb(0x2D2C2C),
        text: rgb(0xAEAEAE),
        text_hover: rgb(0xD9DBDF),
        text_active: rgb(0xFFFFFF),
        pctext: rgb(0xE8E8E8),
        pctext_hover: rgb(0xFFFFFF),
        profile_active: rgb(0x63C35B),
        profile_pause: rgb(0xE3D246),
        profile_stop: rgb(0xDC164F),
        seek: hsl(245.0, 50.0, 70.0),
        progress_inner: hsl(245.0, 50.0, 60.0),
        progress_outer: rgb(0x292E33),
        status_inner: hsl(245.0, 40.0, 50.0),
        tab_hover: white_a(0.15),
        sysname_bg: rgb(0x393378),
        ui_bg: Color32::TRANSPARENT,
        ui_hover: white_a(0.125),
        ui_active: white_a(0.2),
        dd_bg: rgb(0x000000),
        dd_hover: rgb(0x303030),
        dd_active: rgb(0x505050),
        dd_text: rgb(0xAEAEAE),
        dd_blue: hsl(245.0, 71.0, 71.0),
        dd_blue_hover: hsl(245.0, 81.0, 81.0),
        dd_golden: rgb(0xC8B11C),
        dd_golden_hover: rgb(0xE6CF35),
        dialog_bg: rgb(0x0C0C0C),
        dialog_border: rgb(0x393378),
        dialog_text: rgb(0xBCBCBC),
        dialog_title_bg: hsl(245.0, 45.0, 34.0),
        link: rgb(0x889EFF),
        collapsible_bg: rgb(0x0C0C0C),
        collapsible_hover: rgb(0x333333),
        datetile: hsl(245.0, 22.0, 29.0),
        badge: highlight,
        badge_hover: hsl(245.0, 71.0, 71.0),
        badge_selected: rgb(0xBAD8FF),
        timeline_bg: rgb(0x000000),
        timeline_legend: hsl(245.0, 29.0, 22.0),
        timeline_center: Color32::from_rgba_unmultiplied(0x00, 0xAA, 0xFF, 191),
        separator: rgb(0x383E47),
    }
}

fn blue_iris_5() -> Theme {
    let highlight = rgb(0x3E5F8A);
    Theme {
        name: "Blue Iris 5",
        highlight,
        panel: rgb(0x181818),
        text: rgb(0xBFBFBF),
        dd_text: rgb(0xBFBFBF),
        seek: rgb(0x2C82C9),
        progress_inner: highlight,
        progress_outer: rgb(0x383E47),
        status_inner: highlight,
        tab_hover: white_a(0.2),
        sysname_bg: highlight,
        ui_bg: rgb(0x101010),
        ui_hover: rgb(0x303030),
        ui_active: rgb(0x505050),
        dd_blue: rgb(0x618DC7),
        dd_blue_hover: rgb(0x7DB0F3),
        dialog_bg: rgb(0x181818),
        dialog_border: rgb(0x404040),
        dialog_text: rgb(0xCDCDCD),
        dialog_title_bg: highlight,
        link: rgb(0x88DDFF),
        collapsible_bg: rgb(0x181818),
        datetile: rgb(0x383E47),
        badge: highlight,
        badge_hover: rgb(0x618DC7),
        timeline_legend: rgb(0x383E47),
        timeline_center: Color32::from_rgba_unmultiplied(0, 213, 255, 191),
        ..blue_iris_6()
    }
}

fn legacy_ui3() -> Theme {
    let highlight = rgb(0x0097F0);
    Theme {
        name: "Legacy UI3",
        highlight,
        panel: rgb(0x15171B),
        cliplist: rgb(0x15171B),
        button: rgb(0x969BA7),
        ptzpad: rgb(0x363B46),
        ptzpad_disabled: rgb(0x20242B),
        text: rgb(0x969BA7),
        seek: highlight,
        progress_inner: highlight,
        progress_outer: rgb(0x383E47),
        status_inner: highlight,
        tab_hover: white_a(0.2),
        sysname_bg: highlight,
        ui_bg: Color32::TRANSPARENT,
        ui_hover: white_a(0.1),
        ui_active: white_a(0.3),
        dd_bg: rgb(0x2C2E31),
        dd_hover: rgb(0x444444),
        dd_active: rgb(0x666666),
        dd_text: rgb(0xDDDDDD),
        dd_blue: highlight,
        dd_blue_hover: rgb(0x4DBEFF),
        dd_golden: rgb(0xBBBB00),
        dd_golden_hover: rgb(0xDDDD00),
        dialog_bg: rgb(0x393939),
        dialog_border: rgb(0x1870B5),
        dialog_text: rgb(0xCDCDCD),
        dialog_title_bg: rgb(0x272727),
        link: rgb(0x88DDFF),
        collapsible_bg: rgb(0x424242),
        collapsible_hover: rgb(0x4A4A4A),
        datetile: rgb(0x383E47),
        badge: rgb(0x2B4BC6),
        badge_hover: rgb(0x0035FF),
        badge_selected: rgb(0x95C7FC),
        timeline_legend: rgb(0x383E47),
        timeline_center: Color32::from_rgba_unmultiplied(0, 213, 255, 191),
        ..blue_iris_6()
    }
}

fn black(mut t: Theme, name: &'static str) -> Theme {
    t.name = name;
    t.panel = Color32::BLACK;
    t.dialog_bg = Color32::BLACK;
    t.collapsible_bg = Color32::BLACK;
    t
}

static THEMES: LazyLock<[Theme; 5]> = LazyLock::new(|| [blue_iris_6(), black(blue_iris_6(), "Blue Iris 6 Black"), blue_iris_5(), black(blue_iris_5(), "Blue Iris 5 Black"), legacy_ui3()]);
static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// The active theme.
pub fn t() -> &'static Theme {
    &THEMES[CURRENT.load(Ordering::Relaxed)]
}

/// Resolves a `ui3_color_theme` value ("Auto" uses the server version) to a theme index.
fn resolve(setting: &str, bi_version: &str) -> usize {
    let name = if setting.is_empty() || setting.eq_ignore_ascii_case("Auto") {
        if bi_version.starts_with("6.") {
            "Blue Iris 6"
        } else if bi_version.starts_with("4.") {
            "Legacy UI3"
        } else if bi_version.is_empty() {
            "Blue Iris 6"
        } else {
            "Blue Iris 5"
        }
    } else {
        setting
    };
    THEMES.iter().position(|t| t.name.eq_ignore_ascii_case(name)).unwrap_or(0)
}

/// Applies the theme for this setting/server if it changed. Cheap to call every frame.
pub fn sync_theme(ctx: &egui::Context, setting: &str, bi_version: &str) {
    let i = resolve(setting, bi_version);
    if i != CURRENT.load(Ordering::Relaxed) {
        CURRENT.store(i, Ordering::Relaxed);
        apply_visuals(ctx);
    }
}

/// Arial (UI3's font) first, then egui's defaults, then Segoe UI Symbol / Emoji for any remaining glyphs.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let dir = std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
    let mut load = |name: &str, file: &str| -> bool {
        match std::fs::read(dir.join(file)) {
            Ok(bytes) => {
                fonts.font_data.insert(name.to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
                true
            }
            Err(_) => false,
        }
    };
    if load("arial", "arial.ttf") {
        fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "arial".into());
    }
    if load("arial_bold", "arialbd.ttf") {
        fonts.families.insert(egui::FontFamily::Name("bold".into()), vec!["arial_bold".into(), "arial".into()]);
    }
    if load("consolas", "consola.ttf") {
        fonts.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "consolas".into());
    }
    for (name, file) in [("segoe_symbol", "seguisym.ttf"), ("segoe_emoji", "seguiemj.ttf")] {
        if load(name, file) {
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push(name.to_owned());
            }
        }
    }
    if !fonts.families.contains_key(&egui::FontFamily::Name("bold".into())) {
        let p = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts.families.insert(egui::FontFamily::Name("bold".into()), p);
    }
    ctx.set_fonts(fonts);
}

/// Bold Arial at `size` (UI3 uses bold for dialog titles and a few labels).
pub fn bold(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name("bold".into()))
}

pub fn apply(ctx: &egui::Context) {
    egui_extras::install_image_loaders(ctx);
    install_fonts(ctx);
    ctx.set_theme(egui::ThemePreference::Dark);
    apply_visuals(ctx);
}

fn apply_visuals(ctx: &egui::Context) {
    let th = t();
    let mut v = Visuals::dark();
    v.panel_fill = th.panel;
    v.window_fill = th.dialog_bg;
    v.window_stroke = Stroke::new(1.0, th.dialog_border);
    v.window_corner_radius = CornerRadius::ZERO;
    v.menu_corner_radius = CornerRadius::ZERO;
    v.window_shadow = egui::Shadow { offset: [0, 5], blur: 15, spread: 5, color: Color32::from_black_alpha(200) };
    v.popup_shadow = v.window_shadow;
    v.extreme_bg_color = Color32::from_rgb(0x08, 0x08, 0x08);
    v.faint_bg_color = Color32::from_white_alpha(14);
    v.code_bg_color = Color32::from_rgb(0x1A, 0x1A, 0x1A);
    v.selection.bg_fill = th.highlight;
    v.selection.stroke = Stroke::new(1.0, th.text_active);
    v.hyperlink_color = th.link;
    v.override_text_color = None;
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = th.panel;
    w.noninteractive.weak_bg_fill = th.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, th.separator);
    w.noninteractive.fg_stroke = Stroke::new(1.0, th.text);
    w.inactive.bg_fill = Color32::from_rgb(0x1C, 0x1C, 0x1C);
    w.inactive.weak_bg_fill = th.ui_bg;
    w.inactive.bg_stroke = Stroke::new(1.0, th.button.gamma_multiply(0.55));
    w.inactive.fg_stroke = Stroke::new(1.0, th.text);
    w.hovered.bg_fill = Color32::from_rgb(0x30, 0x30, 0x30);
    w.hovered.weak_bg_fill = th.ui_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, th.button);
    w.hovered.fg_stroke = Stroke::new(1.5, th.text_hover);
    w.hovered.expansion = 0.0;
    w.active.bg_fill = Color32::from_rgb(0x50, 0x50, 0x50);
    w.active.weak_bg_fill = th.ui_active;
    w.active.bg_stroke = Stroke::new(1.0, th.button);
    w.active.fg_stroke = Stroke::new(1.5, th.text_active);
    w.active.expansion = 0.0;
    w.open.bg_fill = th.dd_hover;
    w.open.weak_bg_fill = th.dd_hover;
    w.open.bg_stroke = Stroke::new(1.0, th.button);
    w.open.fg_stroke = Stroke::new(1.0, th.text_active);
    for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = CornerRadius::ZERO;
    }
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.all_styles_mut(|s| {
        use egui::{FontFamily, FontId, TextStyle};
        s.text_styles = [
            (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(6.0, 5.0);
        s.spacing.button_padding = egui::vec2(9.0, 4.0);
        s.spacing.interact_size.y = 22.0;
        s.spacing.menu_margin = egui::Margin::same(2);
        s.spacing.window_margin = egui::Margin::same(12);
        s.spacing.scroll = egui::style::ScrollStyle::thin();
        s.interaction.selectable_labels = false;
    });
}

pub fn rgb3(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// "#RRGGBB" -> Color32.
/// Like [`hex`], but invalid input yields `fallback` (UI3 `ValidateHexColor`, accepts `#RGB` and `#RRGGBB`).
pub fn hex_or(s: &str, fallback: &str) -> Color32 {
    let t = s.trim();
    let ok = t.starts_with('#') && matches!(t.len(), 4 | 7) && t[1..].chars().all(|c| c.is_ascii_hexdigit());
    hex(if ok { t } else { fallback })
}

pub fn hex(s: &str) -> Color32 {
    let s = s.trim_start_matches('#');
    if s.len() == 3 {
        let p = |i: usize| u8::from_str_radix(&s[i..i + 1], 16).map(|v| v * 17).unwrap_or(0);
        return Color32::from_rgb(p(0), p(1), p(2));
    }
    if s.len() < 6 {
        return Color32::GRAY;
    }
    let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0);
    Color32::from_rgb(p(0), p(2), p(4))
}
