//! UI3-styled widgets: SVG icons, icon buttons, top-bar tabs, `.dropdownBox`, `.leftBarHeading`, `.statusBar`,
//! `.squareButton`, `.notificationCounter` and `.dropdown_list` menu items. Metrics follow ui3.css (large size).

use crate::icons::Icon;
use crate::views::style::{self, t};
use egui::{Align2, Color32, FontId, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

/// An icon as an egui image, tinted (the SVGs are white) and sized to `h` points tall.
pub fn icon_image(icon: Icon, h: f32, color: Color32) -> egui::Image<'static> {
    egui::Image::from_bytes(format!("bytes://ui3icon/{}.svg", icon.name), icon.svg).tint(color).fit_to_exact_size(vec2(h * icon.aspect, h))
}

/// Paints `icon` centered in `rect`, fitted by height (or width for wide icons).
pub fn paint_icon(ui: &Ui, icon: Icon, rect: Rect, color: Color32) {
    let (w, h) = if icon.aspect * rect.height() <= rect.width() { (rect.height() * icon.aspect, rect.height()) } else { (rect.width(), rect.width() / icon.aspect) };
    let r = Rect::from_center_size(rect.center(), vec2(w, h));
    icon_image(icon, h, color).paint_at(ui, r);
}

/// Text / icon color for a button-like response, using UI3's button → hover → active progression.
pub fn state_color(r: &Response, base: Color32) -> Color32 {
    if r.is_pointer_button_down_on() {
        t().text_active
    } else if r.hovered() {
        t().text_hover
    } else {
        base
    }
}

/// A frameless icon button (`div.icon`): `icon_size` is the icon box, `pad` the padding around it.
pub fn icon_button(ui: &mut Ui, icon: Icon, icon_size: Vec2, pad: Vec2, tip: &str) -> Response {
    icon_button_colored(ui, icon, icon_size, pad, tip, t().button, false)
}

pub fn icon_button_colored(ui: &mut Ui, icon: Icon, icon_size: Vec2, pad: Vec2, tip: &str, base: Color32, hover_bg: bool) -> Response {
    let (rect, r) = ui.allocate_exact_size(icon_size + pad * 2.0, Sense::click());
    if hover_bg && (r.hovered() || r.is_pointer_button_down_on()) {
        ui.painter().rect_filled(rect, 0.0, if r.is_pointer_button_down_on() { t().ui_active } else { t().ui_hover });
    }
    paint_icon(ui, icon, Rect::from_center_size(rect.center(), icon_size), state_color(&r, base));
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}

/// Playback-control button (`.pcButton`): 26px icon with 8px padding, pctext colors.
pub fn pc_button(ui: &mut Ui, icon: Icon, tip: &str) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(42.0, 42.0), Sense::click());
    let c = if r.hovered() { t().pctext_hover } else { t().pctext };
    paint_icon(ui, icon, Rect::from_center_size(rect.center(), vec2(26.0, 26.0)), c);
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}

/// Like [`pc_button`] with a small caption centered over the icon (UI3's skip buttons show "1" / seconds).
pub fn pc_button_caption(ui: &mut Ui, icon: Icon, caption: &str, tip: &str) -> Response {
    let r = pc_button(ui, icon, tip);
    ui.painter().text(r.rect.center() + vec2(0.0, 1.5), Align2::CENTER_CENTER, caption, FontId::proportional(9.0), t().pctext);
    r
}

/// `.pcText`: 16px text in the playback bar; clickable when `sense` is click.
pub fn pc_text(ui: &mut Ui, text: &str, clickable: bool, tip: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(16.0), t().pctext);
    let (rect, r) = ui.allocate_exact_size(vec2(galley.size().x + 16.0, 42.0), if clickable { Sense::click() } else { Sense::hover() });
    let c = if clickable && r.hovered() { t().pctext_hover } else { t().pctext };
    ui.painter().galley_with_override_text_color(pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0), galley, c);
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}

/// A top-bar tab (`.topbar_tab`): 20px label, 8px highlight underline when selected.
pub fn topbar_tab(ui: &mut Ui, label: &str, selected: bool, size: Vec2) -> Response {
    let (rect, r) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if !selected && r.hovered() {
        p.rect_filled(rect, 0.0, t().tab_hover);
    }
    let underline = (size.y / 8.0).round().max(4.0);
    if selected {
        p.rect_filled(Rect::from_min_max(pos2(rect.left(), rect.bottom() - underline), rect.max), 0.0, t().highlight);
    }
    let color = if selected { t().text_active } else if r.hovered() { t().text_hover } else { t().text };
    let text_rect = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - underline));
    let font = if size.y >= 56.0 { 20.0 } else { 18.0 };
    p.text(text_rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(font), color);
    r
}

/// `.notificationCounter`: red pill with white 12px text, anchored at its top-right corner.
pub fn counter(p: &egui::Painter, top_right: egui::Pos2, text: &str) {
    let g = p.layout_no_wrap(text.to_string(), FontId::proportional(12.0), Color32::WHITE);
    let r = Rect::from_min_max(pos2(top_right.x - g.size().x - 6.0, top_right.y), pos2(top_right.x, top_right.y + g.size().y + 2.0));
    p.rect_filled(r, 2.0, style::COUNTER_RED);
    p.galley(pos2(r.left() + 3.0, r.top() + 1.0), g, Color32::WHITE);
}

/// `.dropdownBox`: a bordered box with a 16px label that opens a `.dropdown_list` popup.
/// Returns the popup contents' result while it is open.
pub fn dropdown_box<R>(ui: &mut Ui, id_salt: impl std::hash::Hash, label: &str, width: f32, height: f32, enabled: bool, add_contents: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    dropdown_box_job(ui, id_salt, None, label, width, height, enabled, add_contents)
}

/// [`dropdown_box`] with an optional rich label (e.g. UI3's colored profile number), drawn instead of `label`.
#[allow(clippy::too_many_arguments)]
pub fn dropdown_box_job<R>(ui: &mut Ui, id_salt: impl std::hash::Hash, job: Option<egui::text::LayoutJob>, label: &str, width: f32, height: f32, enabled: bool, add_contents: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    let (rect, r) = ui.allocate_exact_size(vec2(width, height), if enabled { Sense::click() } else { Sense::hover() });
    let r = r.on_hover_cursor(if enabled { egui::CursorIcon::PointingHand } else { egui::CursorIcon::NotAllowed });
    let p = ui.painter();
    let th = t();
    let fill = if !enabled {
        th.ui_bg
    } else if r.is_pointer_button_down_on() {
        th.ui_active
    } else if r.hovered() {
        th.ui_hover
    } else {
        th.ui_bg
    };
    p.rect_filled(rect, 0.0, fill);
    p.rect_stroke(rect, 0.0, Stroke::new(1.0, if enabled { th.button } else { Color32::from_rgb(0x34, 0x36, 0x3A) }), StrokeKind::Inside);
    let color = if !enabled { Color32::from_gray(0x66) } else { state_color(&r, th.text) };
    let arrow = Rect::from_center_size(pos2(rect.right() - 14.0, rect.center().y), vec2(10.0, 6.0));
    paint_icon(ui, crate::icons::DOWN_ARROW, arrow, color);
    let clip = Rect::from_min_max(rect.min + vec2(10.0, 0.0), pos2(arrow.left() - 6.0, rect.bottom()));
    let galley = match job {
        Some(j) => ui.painter().layout_job(j),
        None => ui.painter().layout(label.to_string(), FontId::proportional(16.0), color, f32::INFINITY),
    };
    ui.painter().with_clip_rect(clip).galley(pos2(clip.left(), rect.center().y - galley.size().y / 2.0), galley, color);
    let _ = id_salt;
    egui::Popup::menu(&r)
        .width(width.max(160.0))
        .frame(list_frame())
        .show(|ui| {
            ui.set_min_width(width - 2.0);
            egui::ScrollArea::vertical().max_height(ui.ctx().content_rect().height() * 0.7).show(ui, add_contents).inner
        })
        .map(|ir| ir.inner)
}

/// Frame of `.dropdown_list` popups.
pub fn list_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(t().dd_bg)
        .stroke(Stroke::new(1.0, t().button))
        .shadow(egui::Shadow { offset: [0, 5], blur: 15, spread: 5, color: Color32::BLACK })
        .inner_margin(egui::Margin::same(0))
}

/// Item colors for `.dropdown_list` entries.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Normal,
    /// `goldenLarger`
    Golden,
    /// `blueLarger`
    Blue,
}

/// One `.dropdown_list` row: optional 24px icon, 16px label (20px for golden/blue items), 10px padding.
pub fn list_item(ui: &mut Ui, icon: Option<Icon>, label: &str, selected: bool, kind: ItemKind) -> Response {
    let th = t();
    let size = if kind == ItemKind::Normal { 16.0 } else { 20.0 };
    let icon_w = if icon.is_some() { 34.0 } else { 0.0 };
    let galley = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(size), Color32::WHITE);
    let h = galley.size().y.max(if icon.is_some() { 24.0 } else { 0.0 }) + 20.0;
    let w = (galley.size().x + icon_w + 20.0).max(ui.available_width());
    let (rect, r) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let down = r.is_pointer_button_down_on();
    if down {
        ui.painter().rect_filled(rect, 0.0, th.dd_active);
    } else if r.hovered() || selected {
        ui.painter().rect_filled(rect, 0.0, th.dd_hover);
    }
    let hot = r.hovered() || selected;
    let color = match kind {
        ItemKind::Golden => {
            if hot {
                th.dd_golden_hover
            } else {
                th.dd_golden
            }
        }
        ItemKind::Blue => {
            if hot {
                th.dd_blue_hover
            } else {
                th.dd_blue
            }
        }
        ItemKind::Normal => {
            if down {
                th.text_active
            } else if hot {
                th.text_hover
            } else {
                th.dd_text
            }
        }
    };
    if let Some(i) = icon {
        paint_icon(ui, i, Rect::from_min_size(pos2(rect.left() + 10.0, rect.center().y - 12.0), vec2(24.0, 24.0)), color);
    }
    ui.painter().galley_with_override_text_color(pos2(rect.left() + 10.0 + icon_w, rect.center().y - galley.size().y / 2.0), galley, color);
    r
}

/// A `.dropdown_list` row with a rich label.
pub fn list_item_job(ui: &mut Ui, job: egui::text::LayoutJob, selected: bool) -> Response {
    let galley = ui.painter().layout_job(job);
    let (rect, r) = ui.allocate_exact_size(vec2((galley.size().x + 20.0).max(ui.available_width()), galley.size().y + 20.0), Sense::click());
    if r.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, 0.0, t().dd_active);
    } else if r.hovered() || selected {
        ui.painter().rect_filled(rect, 0.0, t().dd_hover);
    }
    ui.painter().galley(pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0), galley, t().dd_text);
    r
}

/// Builds a one-line rich label from (text, color, bold, background) runs at `size`.
pub fn job(parts: &[(&str, Color32, bool, Option<Color32>)], size: f32) -> egui::text::LayoutJob {
    let mut j = egui::text::LayoutJob::default();
    for (text, color, bold, bg) in parts {
        let font = if *bold { style::bold(size) } else { FontId::proportional(size) };
        let mut f = egui::TextFormat::simple(font, *color);
        if let Some(b) = bg {
            f.background = *b;
        }
        j.append(text, 0.0, f);
    }
    j
}

/// `.dropdown_list > div.separator`
pub fn list_separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, t().dd_text);
}

/// `.leftBarHeading` with UI3's collapsible behaviour; returns true while expanded.
pub fn left_heading(ui: &mut Ui, text: &str, open: &mut bool) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::click());
    let color = state_color(&r, t().text);
    ui.painter().text(rect.left_center(), Align2::LEFT_CENTER, text, FontId::proportional(16.0), color);
    if r.hovered() {
        let tri = if *open { "▾" } else { "▸" };
        ui.painter().text(pos2(rect.left() - 12.0, rect.center().y), Align2::CENTER_CENTER, tri, FontId::proportional(12.0), color);
    }
    if r.clicked() {
        *open = !*open;
    }
    r
}

/// `.statusBar`: 50px label, 100x16 progress bar, amount text (15px).
pub fn status_bar(ui: &mut Ui, label: &str, frac: f32, amount: &str, tip: &str) -> Response {
    let font = FontId::proportional(15.0);
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::click());
    let p = ui.painter();
    let th = t();
    p.with_clip_rect(Rect::from_min_size(rect.min, vec2(50.0, 16.0))).text(rect.left_center(), Align2::LEFT_CENTER, label, font.clone(), th.text);
    let bar_w = (rect.width() - 50.0 - 70.0).clamp(60.0, 100.0);
    let outer = Rect::from_min_size(pos2(rect.left() + 50.0, rect.top()), vec2(bar_w, 16.0));
    p.rect_filled(outer, 0.0, th.progress_outer);
    p.rect_filled(Rect::from_min_size(outer.min, vec2(bar_w * frac.clamp(0.0, 1.0), 16.0)), 0.0, th.status_inner);
    p.text(pos2(outer.right() + 5.0, rect.center().y), Align2::LEFT_CENTER, amount, font, th.text);
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}

/// `.squareButton`: 36x36 (or `size`) bordered button with an icon or text.
pub fn square_button(ui: &mut Ui, size: Vec2, icon: Option<Icon>, text: &str, tip: &str, enabled: bool) -> Response {
    let (rect, r) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let th = t();
    let p = ui.painter();
    let border = if enabled { th.button } else { Color32::from_rgb(0x3E, 0x41, 0x49) };
    if enabled {
        let fill = if r.is_pointer_button_down_on() {
            th.ui_active
        } else if r.hovered() {
            th.ui_hover
        } else {
            th.ui_bg
        };
        p.rect_filled(rect, 0.0, fill);
    }
    p.rect_stroke(rect, 0.0, Stroke::new(1.0, border), StrokeKind::Inside);
    let color = if enabled { state_color(&r, th.button) } else { border };
    if let Some(i) = icon {
        let s = size.min_elem() * 0.72 * 0.75;
        paint_icon(ui, i, Rect::from_center_size(rect.center(), vec2(s, s)), color);
    } else {
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(16.0), color);
    }
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}

/// Box padding of `.leftBarBox` (0 30 15 31) as a frame around sidebar content.
pub fn left_box<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new().inner_margin(egui::Margin { left: 31, right: 30, top: 0, bottom: 15 }).show(ui, add).inner
}

/// A UI3 dialog (`.dialog_wrapper.darkTheme`): 24px title bar in the theme's title color with a 40x24 close
/// button, 1px border, content below. Drop-in for the subset of `egui::Window` the app uses.
pub struct Window<'a> {
    title: String,
    id: egui::Id,
    open: Option<&'a mut bool>,
    default_size: Option<Vec2>,
    default_width: Option<f32>,
    resizable: bool,
}

pub fn window<'a>(title: impl Into<String>) -> Window<'a> {
    let title = title.into();
    Window { id: egui::Id::new(("ui3dialog", title.clone())), title, open: None, default_size: None, default_width: None, resizable: true }
}

/// Fixed-id dialog (for titles that change while open).
pub fn dialog<'a>(title: &str, id: &str) -> Window<'a> {
    let mut w = window(title);
    w.id = egui::Id::new(("ui3dialog", id.to_string()));
    w.resizable = false;
    w
}

impl<'a> Window<'a> {
    pub fn open(mut self, open: &'a mut bool) -> Self {
        self.open = Some(open);
        self
    }
    pub fn default_size(mut self, s: impl Into<Vec2>) -> Self {
        self.default_size = Some(s.into());
        self
    }
    pub fn default_width(mut self, w: f32) -> Self {
        self.default_width = Some(w);
        self
    }
    pub fn resizable(mut self, r: bool) -> Self {
        self.resizable = r;
        self
    }
    /// Accepted for API compatibility; UI3 dialogs never collapse.
    pub fn collapsible(self, _c: bool) -> Self {
        self
    }

    pub fn show<R>(self, ctx: &egui::Context, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
        if self.open.as_ref().is_some_and(|o| !**o) {
            return None;
        }
        let th = t();
        let mut win = egui::Window::new(&self.title)
            .id(self.id)
            .title_bar(false)
            .collapsible(false)
            .resizable(self.resizable)
            .constrain(true)
            .frame(
                egui::Frame::new()
                    .fill(th.dialog_bg)
                    .stroke(Stroke::new(1.0, th.dialog_border))
                    .shadow(egui::Shadow { offset: [0, 0], blur: 35, spread: 1, color: Color32::from_black_alpha(102) })
                    .inner_margin(egui::Margin::same(0)),
            );
        if let Some(s) = self.default_size {
            win = win.default_size(s);
        }
        if let Some(w) = self.default_width {
            win = win.default_width(w);
        }
        let mut close = false;
        let title = self.title.clone();
        let r = win.show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            // Title bar.
            let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width().max(200.0), 24.0), Sense::hover());
            ui.painter().rect_filled(bar, 0.0, th.dialog_title_bg);
            ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, th.dialog_title_bg.gamma_multiply(0.5)));
            let tr = bar.shrink2(vec2(4.0, 0.0));
            ui.painter().with_clip_rect(Rect::from_min_max(tr.min, pos2(tr.right() - 40.0, tr.bottom()))).text(
                tr.left_center(),
                Align2::LEFT_CENTER,
                &title,
                FontId::proportional(16.0),
                Color32::from_rgb(0xC6, 0xC6, 0xC6),
            );
            let cb = Rect::from_min_max(pos2(bar.right() - 40.0, bar.top()), bar.max);
            let cr = ui.interact(cb, ui.id().with("dlgclose"), Sense::click());
            if cr.is_pointer_button_down_on() {
                ui.painter().rect_filled(cb, 0.0, Color32::from_rgb(0x7D, 0x04, 0x0E));
            } else if cr.hovered() {
                ui.painter().rect_filled(cb, 0.0, Color32::from_rgb(0x9B, 0x06, 0x12));
            }
            let x = Rect::from_center_size(cb.center(), vec2(10.0, 10.0));
            let xs = Stroke::new(1.5, if cr.hovered() { Color32::WHITE } else { Color32::from_rgb(0xCD, 0xCD, 0xCD) });
            ui.painter().line_segment([x.left_top(), x.right_bottom()], xs);
            ui.painter().line_segment([x.right_top(), x.left_bottom()], xs);
            if cr.clicked() {
                close = true;
            }
            ui.spacing_mut().item_spacing.y = 5.0;
            egui::Frame::new().inner_margin(egui::Margin { left: 10, right: 10, top: 8, bottom: 10 }).show(ui, |ui| {
                ui.visuals_mut().override_text_color = Some(th.dialog_text);
                add(ui)
            })
            .inner
        });
        if close
            && let Some(o) = self.open
        {
            *o = false;
        }
        r.and_then(|r| r.inner)
    }
}

/// Flat UI3 progress bar (`.progressBarOuter` / `.progressBarInner`) with text on top.
pub fn flat_bar(ui: &mut Ui, frac: f32, text: &str, width: f32) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(width.min(ui.available_width()), 20.0), Sense::hover());
    let th = t();
    ui.painter().rect_filled(rect, 0.0, th.progress_outer);
    ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(rect.width() * frac.clamp(0.0, 1.0), rect.height())), 0.0, th.progress_inner);
    ui.painter().with_clip_rect(rect).text(rect.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, text, FontId::proportional(13.0), th.text_active);
    r
}
