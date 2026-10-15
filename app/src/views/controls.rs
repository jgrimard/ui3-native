//! Video overlay controls (ui3.htm #playbackControls, #playbackHeader and the `.playbackSettings` panel).
//!
//! Like UI3 the bar appears over the bottom of the video while the mouse moves (and stays while a clip is paused):
//! seek bar for clips; play/pause, previous/next clip, skip and frame-step buttons, volume and the position text on
//! the left; full screen, maximize, picture-in-picture (mini mode), export, download, flag, delete, settings,
//! auto-maximize and the speed label on the right.

use crate::app::Services;
use crate::icons;
use crate::views::main_view::{Content, MainView, Tab};
use crate::views::style::t;
use crate::views::{playback, w};
use bi_proto::api::flags;
use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, pos2, vec2};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Main,
    Speed,
    Quality,
}

pub struct Controls {
    visible_until: Instant,
    panel: Option<Panel>,
    /// The panel was opened this frame (its opening click must not close it).
    panel_opened: bool,
    volume_drag: bool,
}

impl Default for Controls {
    fn default() -> Self {
        Controls { visible_until: Instant::now() + Duration::from_secs(3), panel: None, panel_opened: false, volume_drag: false }
    }
}

impl Controls {
    pub fn poke(&mut self) {
        self.visible_until = Instant::now() + Duration::from_secs(3);
    }
}

pub fn set_fullscreen(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, on: bool) {
    view.fullscreen = on;
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    // ui3_fullscreen_videoonly: full screen also maximizes the video area.
    if svc.settings.bool("ui3_fullscreen_videoonly") {
        set_maximized(view, svc, on);
    }
}

pub fn set_maximized(view: &mut MainView, svc: &mut Services, on: bool) {
    view.maximized = on;
    svc.settings.set_bool("ui3_is_maximized", on);
}

pub fn enter_mini_mode(view: &mut MainView, ctx: &egui::Context) {
    view.mini_mode = true;
    view.maximized = true;
    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(480.0, 270.0)));
}

pub fn exit_mini_mode(view: &mut MainView, ctx: &egui::Context) {
    view.mini_mode = false;
    view.maximized = false;
    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1400.0, 860.0)));
}

/// Vertical gradient fill (CSS `linear-gradient(to bottom, top, bottom)`).
fn gradient(p: &egui::Painter, r: Rect, top: Color32, bottom: Color32) {
    let mut m = egui::Mesh::default();
    m.colored_vertex(r.left_top(), top);
    m.colored_vertex(r.right_top(), top);
    m.colored_vertex(r.right_bottom(), bottom);
    m.colored_vertex(r.left_bottom(), bottom);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(m));
}

fn fmt_clip_date(utc_ms: i64, tz: i64, h24: bool) -> String {
    chrono::DateTime::from_timestamp_millis(utc_ms + tz)
        .map(|d| {
            let d = d.naive_utc();
            if h24 { d.format("%-m/%-d/%Y %H:%M:%S").to_string() } else { d.format("%-m/%-d/%Y %-I:%M:%S %p").to_string() }
        })
        .unwrap_or_default()
}

/// Draws the overlay over `area` (the video area). Call after the video has been painted.
pub fn show(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services, area: Rect) {
    let ctx = ui.ctx().clone();
    let is_clip = matches!(view.content, Content::Clip { .. }) && view.playback.clip.is_some();
    if is_clip {
        // Bookkeeping that must run every frame (clip end, scrub results); may switch clips or close.
        playback::update(view, &ctx, svc);
    }
    let is_clip = matches!(view.content, Content::Clip { .. }) && view.playback.clip.is_some();
    let is_live = matches!(view.content, Content::Live { .. }) && view.tab != Tab::Timeline;
    let hovered = ui.rect_contains_pointer(area);
    if hovered && ui.input(|i| i.pointer.delta().length_sq() > 0.0 || i.pointer.any_pressed()) {
        view.controls.poke();
    }
    let always = svc.settings.bool("ui3_extra_playback_controls_alwaysVisible");
    let visible = always || view.controls.panel.is_some() || view.controls.volume_drag || (is_clip && (view.playback.paused || view.playback.is_dragging())) || Instant::now() < view.controls.visible_until;

    if is_clip && visible {
        clip_header(view, ui, svc, area);
    }
    if !visible {
        ctx.request_repaint_after(Duration::from_millis(250));
        return;
    }
    if view.mini_mode {
        // Mini mode: only a restore button.
        let r = Rect::from_min_size(area.right_top() + vec2(-46.0, 4.0), vec2(42.0, 42.0));
        let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(r));
        if w::pc_button(&mut cui, icons::FULLSCREEN_EXIT, "Exit mini mode").clicked() {
            exit_mini_mode(view, &ctx);
        }
        return;
    }

    let seek_h = if is_clip { 16.0 } else { 0.0 };
    let bar_h = seek_h + 42.0 + 6.0;
    let bar = Rect::from_min_max(pos2(area.left(), area.bottom() - bar_h - 24.0), area.max);
    gradient(ui.painter(), bar, Color32::TRANSPARENT, Color32::from_black_alpha(191));
    // Block clicks from reaching the video under the bar.
    let _ = ui.interact(Rect::from_min_max(pos2(area.left(), area.bottom() - bar_h), area.max), ui.id().with("pc_block"), Sense::click_and_drag());
    if ui.rect_contains_pointer(bar) {
        view.controls.poke();
    }

    if is_clip {
        let seek = Rect::from_min_max(pos2(area.left() + 16.0, area.bottom() - bar_h), pos2(area.right() - 16.0, area.bottom() - bar_h + 16.0));
        playback::seek_bar(view, ui, svc, seek);
    }

    let row = Rect::from_min_max(pos2(area.left() + 8.0, area.bottom() - 48.0), pos2(area.right() - 8.0, area.bottom() - 6.0));
    let mut left = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)));
    left.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let mut right = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::right_to_left(egui::Align::Center)));
    right.spacing_mut().item_spacing = vec2(0.0, 0.0);

    // ---- Left side ----
    if is_clip {
        let pb = &mut view.playback;
        let client = view.client.clone();
        if w::pc_button(&mut left, if pb.paused { icons::PLAY } else { icons::PAUSE }, if pb.paused { "Play" } else { "Pause" }).clicked() {
            pb.toggle_pause(&ctx, &client, &mut view.player, svc);
        }
        let mut nav = None;
        if svc.settings.bool("ui3_pc_next_prev_buttons") {
            if w::pc_button(&mut left, icons::SKIP_BACK, "Previous Clip").clicked() {
                nav = Some(-1);
            }
            if w::pc_button(&mut left, icons::SKIP_FORWARD, "Next Clip").clicked() {
                nav = Some(1);
            }
        }
        let skip = svc.settings.f64("ui3_skipAmount");
        if svc.settings.bool("ui3_pc_seek_buttons") {
            let cap = format!("{}", skip.round() as i64);
            if w::pc_button_caption(&mut left, icons::REPLAY, &cap, "Skip Back").clicked() {
                view.playback.skip(&ctx, &client, &mut view.player, svc, -(skip * 1000.0) as i64);
            }
            if pc_button_mirrored(&mut left, icons::REPLAY, &cap, "Skip Ahead").clicked() {
                view.playback.skip(&ctx, &client, &mut view.player, svc, (skip * 1000.0) as i64);
            }
        }
        if svc.settings.bool("ui3_pc_seek_1frame_buttons") {
            if w::pc_button_caption(&mut left, icons::REPLAY, "1", "Skip Back 1 Frame").clicked() {
                view.playback.step_frame(&ctx, &client, &mut view.player, svc, -1);
            }
            if pc_button_mirrored(&mut left, icons::REPLAY, "1", "Skip Ahead 1 Frame").clicked() {
                view.playback.step_frame(&ctx, &client, &mut view.player, svc, 1);
            }
        }
        // `neighbor` direction: +1 = older (down the list). With "Oldest First" review order, Next = newer.
        let oldest_first = svc.settings.str("ui3_clip_navigation_direction") == "Oldest First";
        if let Some(dir) = nav.map(|n| if oldest_first { -n } else { n })
            && let Some(clip) = view.playback.clip.clone()
            && let Some(next) = view.clips.neighbor(&clip.path, dir)
        {
            playback::open_clip(view, &ctx, svc, next);
        }
    }
    let is_timeline = view.tab == Tab::Timeline;
    if is_timeline {
        let client = view.client.clone();
        let tl = &mut view.timeline;
        let playing = !tl.paused && !tl.is_live;
        if w::pc_button(&mut left, if tl.paused { icons::PLAY } else { icons::PAUSE }, if tl.paused { "Play" } else { "Pause" }).clicked() {
            tl.toggle_pause(&ctx, &client, &mut view.player, svc);
        }
        if w::pc_button(&mut left, icons::SKIP_BACK, "Previous Alert").clicked() {
            tl.jump(&ctx, &client, &mut view.player, svc, -1);
        }
        if w::pc_button(&mut left, icons::SKIP_FORWARD, "Next Alert").clicked() {
            tl.jump(&ctx, &client, &mut view.player, svc, 1);
        }
        let _ = playing;
    }
    if svc.settings.bool("ui3_feature_enabled_volumeBar") && view.session.login.permissions().audio {
        volume_bar(view, &mut left, svc);
    }
    if is_clip && let Some(clip) = view.playback.clip.clone() {
        let pb = &view.playback;
        w::pc_text(&mut left, &format!("{} / {}", playback::fmt_dur(pb.position_ms), playback::fmt_dur(pb.msec)), false, "");
        if svc.settings.bool("ui3_extra_playback_controls_timestamp") {
            let tz = view.session.login.tz_offset_ms();
            let start = clip.date * 1000 - if clip.is_clip() { 0 } else { clip.offset };
            let h24 = svc.settings.bool("ui3_time24hour");
            w::pc_text(&mut left, &crate::views::clips::server_time_str(start + pb.position_ms, tz, h24), false, "Frame timestamp");
        }
    }

    // ---- Right side (UI3 floats these right; the first is rightmost) ----
    let (fs_icon, fs_tip) = if view.fullscreen { (icons::FULLSCREEN_EXIT, "Exit Full Screen") } else { (icons::FULLSCREEN, "Full Screen") };
    if w::pc_button(&mut right, fs_icon, fs_tip).clicked() {
        let on = !view.fullscreen;
        set_fullscreen(view, &ctx, svc, on);
    }
    if svc.settings.bool("ui3_show_maximize_button") || view.maximized || view.fullscreen {
        let (icon, tip) = if view.maximized { (icons::COLLAPSE, "Exit Maximize Video") } else { (icons::EXPAND, "Maximize Video") };
        if w::pc_button(&mut right, icon, tip).clicked() {
            let on = !view.maximized;
            set_maximized(view, svc, on);
        }
    }
    if svc.settings.bool("ui3_show_picture_in_picture_button") && w::pc_button(&mut right, icons::PIP, "Picture-in-Picture (always-on-top mini window)").clicked() {
        enter_mini_mode(view, &ctx);
    }
    if is_clip && let Some(clip) = view.playback.clip.clone() {
        if w::pc_button(&mut right, icons::LAUNCH, "Export").clicked() {
            view.dialogs.open(crate::views::dialogs::Dialog::Export(Box::new(clip.clone())));
        }
        if w::pc_button(&mut right, icons::DOWNLOAD, "Download Clip").clicked() {
            crate::views::dialogs::download_clips(view, svc, vec![clip.clone()]);
        }
        let flagged = clip.has_flag(flags::FLAGGED);
        let fr = pc_button_colored(&mut right, icons::FLAG, if flagged { "Unflag" } else { "Flag" }, if flagged { Some(t().dd_blue) } else { None });
        if fr.clicked() {
            view.clips.set_flag(&ctx, &view.client, vec![clip.clone()], flags::FLAGGED, !flagged);
            if let Some(c) = &mut view.playback.clip {
                c.flags = if flagged { c.flags & !flags::FLAGGED } else { c.flags | flags::FLAGGED };
            }
        }
        if svc.settings.bool("ui3_pc_delete_button") && svc.settings.can_delete_clips(view.session.login.admin) && w::pc_button(&mut right, icons::ERROR, "Delete").clicked() {
            if svc.settings.delete_needs_confirm(1) {
                view.clips.delete_confirm = Some(vec![clip.clone()]);
                return;
            }
            view.clips.delete(&ctx, &view.client, vec![clip.clone()]);
            view.playback.close();
            let home = view.home_group.clone();
            view.load_live(&ctx, svc.settings, Some(svc.audio), &home);
            return;
        }
    }
    let sr = w::pc_button(&mut right, icons::SETTINGS, "Settings");
    let abbr = svc.settings.current_profile().abbr.clone();
    if !abbr.is_empty() {
        let g = ui.painter().layout_no_wrap(abbr, FontId::proportional(10.0), Color32::WHITE);
        let r = Rect::from_min_size(pos2(sr.rect.right() - 5.0 - g.size().x - 4.0, sr.rect.top() + 6.0), g.size() + vec2(4.0, 2.0));
        ui.painter().rect_filled(r, 0.0, Color32::from_rgb(0xFF, 0, 0));
        ui.painter().galley(r.min + vec2(2.0, 1.0), g, Color32::WHITE);
    }
    if sr.clicked() {
        view.controls.panel = if view.controls.panel.is_some() { None } else { Some(Panel::Main) };
        view.controls.panel_opened = view.controls.panel.is_some();
    }
    if view.tab == Tab::Timeline {
        let gr = w::pc_button(&mut right, icons::APPS, "Change Group");
        let groups: Vec<(String, String)> = view.groups().map(|g| (g.id.clone(), g.name.clone())).collect();
        let mut chosen = None;
        egui::Popup::menu(&gr).frame(w::list_frame()).align(egui::RectAlign::TOP_END).show(|ui| {
            for (id, name) in &groups {
                if w::list_item(ui, None, name, *id == view.timeline.target, w::ItemKind::Normal).clicked() {
                    chosen = Some(id.clone());
                    ui.close();
                }
            }
        });
        if let Some(id) = chosen {
            view.home_group = id.clone();
            svc.settings.set("ui3_defaultCameraGroupId", id.clone());
            view.select_source(&ctx, svc, &id);
        }
    }
    if is_live {
        let on = svc.settings.bool("ui3_prioritizeTriggered");
        let mode = match svc.settings.str("ui3_prioritizeTriggered_triggerMode").as_str() {
            "Trigger" => "triggered",
            "Motion" => "detecting motion",
            _ => "alerted or triggered or detecting motion",
        };
        let tip = format!("Auto-Maximize\n\nWhen live viewing, automatically maximize cameras that are {mode}.\n\nClick button to toggle.");
        let color = if on { t().pctext } else { Color32::from_rgb(0x73, 0x73, 0x73) };
        if pc_button_colored(&mut right, icons::AUTO_MAXIMIZE, &tip, Some(color)).clicked() {
            svc.settings.set_bool("ui3_prioritizeTriggered", !on);
        }
    }
    if is_timeline {
        let speed = svc.settings.f64("ui3_playback_speed");
        let rev = svc.settings.bool("ui3_playback_reverse");
        let label = format!("{}{}x", if rev { "-" } else { "" }, playback::fmt_speed(speed));
        if w::pc_text(&mut right, &label, true, "Playback Speed").clicked() {
            view.controls.panel = Some(Panel::Speed);
            view.controls.panel_opened = true;
        }
    }
    if is_clip {
        let speed = view.playback.speed;
        let label = format!("{}{}", if view.playback.reverse { "-" } else { "" }, if speed == 1.0 { "1x".to_string() } else { format!("{}x", playback::fmt_speed(speed)) });
        if w::pc_text(&mut right, &label, true, "Playback Speed").clicked() {
            view.controls.panel = Some(Panel::Speed);
            view.controls.panel_opened = true;
        }
    }

    settings_panel(view, ui, svc, area, bar_h);
    if is_clip && !view.playback.paused {
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    ctx.request_repaint_after(Duration::from_millis(500));
}

fn pc_button_colored(ui: &mut egui::Ui, icon: crate::icons::Icon, tip: &str, color: Option<Color32>) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(42.0, 42.0), Sense::click());
    let c = if r.hovered() { t().pctext_hover } else { color.unwrap_or(t().pctext) };
    w::paint_icon(ui, icon, Rect::from_center_size(rect.center(), vec2(26.0, 26.0)), c);
    r.on_hover_text(tip)
}

/// UI3 draws "skip ahead" as the replay icon mirrored horizontally (`invertv`).
/// Audio is requested per stream, so the stream is only reopened when the audio state it was opened with no longer
/// matches what is wanted (`AudioToggleNotify`, ui3.js:21057). Muting an already audio-less stream changes nothing.
fn reopen_if_audio_changed(view: &mut MainView, ctx: &egui::Context, svc: &mut Services) {
    let wanted = view.audio_request(svc.settings, svc.audio) != bi_proto::urls::AudioRequest::None;
    if view.player.params.as_ref().is_some_and(|p| p.audio != wanted) {
        view.reload_current(ctx, svc);
    }
}

fn pc_button_mirrored(ui: &mut egui::Ui, icon: crate::icons::Icon, caption: &str, tip: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(42.0, 42.0), Sense::click());
    let c = if r.hovered() { t().pctext_hover } else { t().pctext };
    let ir = Rect::from_center_size(rect.center(), vec2(26.0, 26.0));
    w::icon_image(icon, 26.0, c).uv(Rect::from_min_max(pos2(1.0, 0.0), pos2(0.0, 1.0))).paint_at(ui, ir);
    ui.painter().text(rect.center() + vec2(0.0, 1.5), Align2::CENTER_CENTER, caption, FontId::proportional(9.0), t().pctext);
    r.on_hover_text(tip)
}

/// `#volumeBar`: speaker icon (click toggles mute) and a 50x16 draggable level bar. Green while audio plays.
fn volume_bar(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services) {
    let ctx = ui.ctx().clone();
    let vol = svc.audio.volume();
    let muted = svc.audio.muted();
    let playing = view.player.stats().audio_kbps > 0.0;
    let icon = if muted {
        icons::VOLUME_MUTE
    } else if vol <= 0.0 {
        icons::VOLUME_OFF
    } else if vol < 0.5 {
        icons::VOLUME_DOWN
    } else {
        icons::VOLUME_UP
    };
    let base = if playing && !muted { t().profile_active } else { t().button };
    let (rect, r) = ui.allocate_exact_size(vec2(36.0 + 8.0, 42.0), Sense::click());
    let c = if r.hovered() { t().text_hover } else { base };
    w::paint_icon(ui, icon, Rect::from_center_size(rect.center() + vec2(4.0, 0.0), vec2(26.0, 26.0)), c);
    if r.on_hover_text(if muted { "Unmute" } else { "Mute" }).clicked() {
        let m = !muted;
        svc.audio.set_muted(m);
        svc.settings.set_bool("ui3_audioMute", m);
        reopen_if_audio_changed(view, &ctx, svc);
    }
    let (brect, br) = ui.allocate_exact_size(vec2(50.0 + 12.0, 42.0), Sense::click_and_drag());
    let outer = Rect::from_min_size(pos2(brect.left(), brect.center().y - 8.0), vec2(50.0, 16.0));
    ui.painter().rect_filled(outer, 0.0, t().progress_outer);
    let shown = if muted { 0.0 } else { vol };
    ui.painter().rect_filled(Rect::from_min_size(outer.min, vec2(50.0 * shown, 16.0)), 0.0, t().progress_inner);
    // Drag handle (.statusBarDragHandle): 25%-wide bar centered on the level.
    let hx = outer.left() + 50.0 * shown;
    ui.painter().rect_filled(Rect::from_center_size(pos2(hx, outer.center().y), vec2(6.0, 21.0)), 0.0, t().button);
    if br.is_pointer_button_down_on()
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
    {
        view.controls.volume_drag = true;
        let v = ((p.x - outer.left()) / 50.0).clamp(0.0, 1.0);
        svc.audio.set_volume(v);
        svc.settings.set("ui3_audioVolume", serde_json::json!(v));
        if muted && v > 0.0 {
            svc.audio.set_muted(false);
            svc.settings.set_bool("ui3_audioMute", false);
            reopen_if_audio_changed(view, &ctx, svc);
        }
    } else if view.controls.volume_drag && !ui.input(|i| i.pointer.any_down()) {
        view.controls.volume_drag = false;
    }
    br.on_hover_text(format!("Volume {}%", (vol * 100.0).round()));
}

/// `#playbackHeader`: close button and the clip's camera name + date over the top of the video.
fn clip_header(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services, area: Rect) {
    let Some(clip) = view.playback.clip.clone() else { return };
    let ctx = ui.ctx().clone();
    let r = Rect::from_min_size(area.min, vec2(area.width(), 64.0));
    gradient(ui.painter(), r, Color32::from_black_alpha(64), Color32::TRANSPARENT);
    let close = Rect::from_min_size(area.min + vec2(16.0, 12.0), vec2(32.0, 32.0));
    let cr = ui.interact(close, ui.id().with("closeClip"), Sense::click()).on_hover_text("Close clip");
    let color = if cr.is_pointer_button_down_on() {
        Color32::from_rgb(0xFF, 0x88, 0x00)
    } else if cr.hovered() {
        Color32::from_rgb(0xFF, 0xCC, 0x00)
    } else {
        Color32::from_rgb(0xEE, 0xEE, 0xEE)
    };
    w::paint_icon(ui, icons::CLOSE_BTN, close.translate(vec2(2.0, 2.0)), Color32::from_black_alpha(200));
    w::paint_icon(ui, icons::CLOSE_BTN, close, color);
    if svc.settings.bool("ui3_feature_enabled_clipNameLabel") {
        let cam = view.camera(&clip.camera).map(|c| c.name.clone()).unwrap_or_else(|| clip.camera.clone());
        let tz = view.session.login.tz_offset_ms();
        let text = format!("{cam} {}", fmt_clip_date(clip.date * 1000, tz, svc.settings.bool("ui3_time24hour")));
        let p = ui.painter().with_clip_rect(Rect::from_min_max(pos2(close.right() + 16.0, r.top()), pos2(r.right() - 20.0, r.bottom())));
        let pos = pos2(close.right() + 16.0, close.center().y);
        p.text(pos + vec2(2.0, 2.0), Align2::LEFT_CENTER, &text, FontId::proportional(24.0), Color32::BLACK);
        p.text(pos, Align2::LEFT_CENTER, &text, FontId::proportional(24.0), Color32::from_rgb(0xEE, 0xEE, 0xEE));
    }
    if cr.clicked() {
        view.playback.close();
        let home = view.home_group.clone();
        view.load_live(&ctx, svc.settings, Some(svc.audio), &home);
    }
}

/// `.playbackSettings`: translucent panel above the settings button with toggles and Speed / Quality submenus.
fn settings_panel(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services, area: Rect, bar_h: f32) {
    let Some(panel) = view.controls.panel else { return };
    let ctx = ui.ctx().clone();
    let is_clip = matches!(view.content, Content::Clip { .. }) && view.playback.clip.is_some();
    let anchor = pos2(area.right() - 20.0, area.bottom() - bar_h - 12.0);
    let mut close = false;
    let mut next_panel = None;
    let resp = egui::Area::new(egui::Id::new("playbackSettings")).order(egui::Order::Foreground).pivot(Align2::RIGHT_BOTTOM).fixed_pos(anchor).show(&ctx, |ui| {
        egui::Frame::new().fill(Color32::from_black_alpha(170)).stroke(Stroke::new(1.0, Color32::from_gray(0x66))).inner_margin(egui::Margin::symmetric(0, 10)).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            ui.set_min_width(260.0);
            ui.set_max_height(area.height() - bar_h - 40.0);
            let line = |ui: &mut egui::Ui, left: &str, right: &str, selected: bool, heading: bool| -> egui::Response {
                let font = FontId::proportional(18.4);
                let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width().max(260.0), 40.0), if heading { Sense::hover() } else { Sense::click() });
                if !heading && (r.hovered() || selected) {
                    ui.painter().rect_filled(rect, 0.0, Color32::from_white_alpha(77));
                }
                ui.painter().text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, left, font.clone(), t().text_active);
                if !right.is_empty() {
                    ui.painter().text(pos2(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER, right, font, t().text_active);
                }
                if heading {
                    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, Color32::from_gray(0x66)));
                }
                r
            };
            match panel {
                Panel::Main => {
                    if is_clip {
                        let mut ap = svc.settings.bool("ui3_playback_autoplay");
                        if slider_cb(ui, "Autoplay", &mut ap) {
                            svc.settings.set_bool("ui3_playback_autoplay", ap);
                        }
                        let mut rev = view.playback.reverse;
                        if slider_cb(ui, "Reverse", &mut rev) {
                            let client = view.client.clone();
                            view.playback.toggle_reverse(&ctx, &client, &mut view.player, svc);
                        }
                        let mut lp = svc.settings.bool("ui3_playback_loop");
                        if slider_cb(ui, "Loop", &mut lp) {
                            svc.settings.set_bool("ui3_playback_loop", lp);
                        }
                        let s = view.playback.speed;
                        if line(ui, "Speed", &format!("{}  ›", if s == 1.0 { "Normal".to_string() } else { playback::fmt_speed(s) }), false, false).clicked() {
                            next_panel = Some(Panel::Speed);
                        }
                    }
                    if view.tab == Tab::Timeline {
                        let mut rev = svc.settings.bool("ui3_playback_reverse");
                        if slider_cb(ui, "Reverse", &mut rev) {
                            svc.settings.set_bool("ui3_playback_reverse", rev);
                            let client = view.client.clone();
                            view.timeline.speed_changed(&ctx, &client, &mut view.player, svc);
                        }
                        let s = svc.settings.f64("ui3_playback_speed");
                        if line(ui, "Speed", &format!("{}  ›", if s == 1.0 { "Normal".to_string() } else { playback::fmt_speed(s) }), false, false).clicked() {
                            next_panel = Some(Panel::Speed);
                        }
                        if line(ui, "Export: set start here", "", false, false).clicked() {
                            view.timeline.set_export_start();
                            close = true;
                        }
                        if line(ui, "Export: set end here", "", false, false).clicked() {
                            view.timeline.set_export_end();
                            close = true;
                        }
                        if let Some((a, b)) = view.timeline.export_range
                            && b > a
                        {
                            if line(ui, "Export selected range…", "", false, false).clicked() {
                                view.dialogs.open(crate::views::dialogs::Dialog::TimelineExport { camera: view.timeline.target.clone(), start: a, end: b });
                                close = true;
                            }
                            if line(ui, "Clear export range", "", false, false).clicked() {
                                view.timeline.clear_export();
                                close = true;
                            }
                        }
                    }
                    let q = svc.settings.current_profile().display_name();
                    if line(ui, "Quality", &format!("{q}  ›"), false, false).clicked() {
                        next_panel = Some(Panel::Quality);
                    }
                    let mut stats = svc.settings.native.show_stats_overlay;
                    if slider_cb(ui, "Stats for nerds", &mut stats) {
                        svc.settings.native.show_stats_overlay = stats;
                        svc.settings.mark_dirty();
                    }
                }
                Panel::Speed => {
                    if line(ui, "‹  Speed", "", false, false).clicked() {
                        next_panel = Some(Panel::Main);
                    }
                    let timeline = view.tab == Tab::Timeline;
                    let cur = if timeline { svc.settings.f64("ui3_playback_speed") } else { view.playback.speed };
                    for s in playback::SPEEDS.iter().rev() {
                        let label = if *s == 1.0 { "Normal".to_string() } else { playback::fmt_speed(*s) };
                        if line(ui, &label, "", *s == cur, false).clicked() {
                            let client = view.client.clone();
                            if timeline {
                                svc.settings.set("ui3_playback_speed", serde_json::json!(*s));
                                view.timeline.speed_changed(&ctx, &client, &mut view.player, svc);
                            } else {
                                view.playback.set_speed(&ctx, &client, &mut view.player, svc, *s);
                            }
                            close = true;
                        }
                    }
                }
                Panel::Quality => {
                    if line(ui, "‹  Quality", "", false, false).clicked() {
                        next_panel = Some(Panel::Main);
                    }
                    let cur = svc.settings.current_profile().name.clone();
                    let profiles = svc.settings.streaming_profiles.clone();
                    let mut chosen = None;
                    egui::ScrollArea::vertical().max_height(area.height() - bar_h - 120.0).show(ui, |ui| {
                        for p in &profiles {
                            if line(ui, &p.display_name(), &p.abbr, p.name == cur, false).on_hover_text(p.tooltip()).clicked() {
                                chosen = Some(p.name.clone());
                            }
                        }
                    });
                    if let Some(n) = chosen {
                        svc.settings.set("ui3_streamingQuality", n);
                        view.reload_current(&ctx, svc);
                        close = true;
                    }
                }
            }
        });
    });
    view.controls.poke();
    if let Some(p) = next_panel {
        view.controls.panel = Some(p);
    } else if close {
        view.controls.panel = None;
    } else if !view.controls.panel_opened && (resp.response.clicked_elsewhere() || ui.input(|i| i.key_pressed(egui::Key::Escape))) {
        view.controls.panel = None;
    }
    view.controls.panel_opened = false;
}

/// `.sliderCb`: a label with a toggle switch on the right. Returns true when toggled.
fn slider_cb(ui: &mut egui::Ui, label: &str, on: &mut bool) -> bool {
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width().max(260.0), 44.0), Sense::click());
    if r.hovered() {
        ui.painter().rect_filled(rect, 0.0, Color32::from_white_alpha(77));
    }
    ui.painter().text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, label, FontId::proportional(18.4), t().text_active);
    let track = Rect::from_center_size(pos2(rect.right() - 30.0, rect.center().y), vec2(34.0, 14.0));
    let col = if *on { t().link } else { Color32::from_gray(0x80) };
    ui.painter().rect_filled(track, 7.0, col.gamma_multiply(0.5));
    let kx = if *on { track.right() - 7.0 } else { track.left() + 7.0 };
    ui.painter().circle_filled(pos2(kx, track.center().y), 10.0, if *on { col } else { Color32::from_gray(0xDD) });
    if r.clicked() {
        *on = !*on;
        return true;
    }
    false
}
