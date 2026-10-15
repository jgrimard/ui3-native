//! Video area: draws the current picture with digital zoom/pan, maps clicks to cameras within group images,
//! and paints overlays (camera labels, status icons, loading, stats).
//!
//! Ports `ImageRenderer` (ui3.js:23685), `zoomHandler_Adjustable` (24210), `CameraNameLabels` (24376),
//! `CornerStatusIcons` (24704), `GetCameraUnderMousePointer` (18593) and `UI3NerdStats` (37724).

use crate::app::Services;
use crate::gpu::Gpu;
use crate::player::DisplayFrame;
use crate::settings::Settings;
use crate::views::main_view::{Content, MainView};
use crate::views::style;
use bi_proto::urls::GroupArgs;
use bi_render::VideoTexture;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Vec2, pos2, vec2};

#[derive(Default)]
pub struct VideoArea {
    tex: Option<VideoTexture>,
    /// 1.0 = fit to area.
    pub zoom: f32,
    /// Image-normalized coordinate shown at the center of the area.
    pub center: Vec2,
    /// Last on-screen image rect (for hit testing from other views).
    pub last_image_rect: Option<Rect>,
    pub last_area: Option<Rect>,
}

#[derive(Default)]
pub struct VideoInput {
    /// Some(camera under pointer, if any) on single click.
    pub clicked: Option<Option<String>>,
    pub double_clicked: Option<Option<String>>,
    pub context_menu_camera: Option<Option<String>>,
}

impl VideoArea {
    pub fn reset_zoom(&mut self) {
        self.zoom = 1.0;
        self.center = vec2(0.5, 0.5);
    }

    pub fn set_frame(&mut self, gpu: &Gpu, f: &DisplayFrame) {
        let tex = self.tex.get_or_insert_with(|| gpu.new_texture());
        gpu.upload(tex, f);
    }

    /// The picture currently shown, copied back from the GPU (for local snapshots).
    pub fn capture(&self, gpu: &Gpu) -> Option<bi_media::jpeg::RgbaImage> {
        self.tex.as_ref()?.read_rgba(&gpu.device, &gpu.queue)
    }

    pub fn has_picture(&self) -> bool {
        self.tex.as_ref().is_some_and(|t| t.has_image())
    }

    pub fn picture_size(&self) -> Option<[u32; 2]> {
        self.tex.as_ref().filter(|t| t.has_image()).map(|t| t.size())
    }

    pub fn release(&mut self, gpu: &Gpu) {
        if let Some(mut t) = self.tex.take() {
            gpu.release(&mut t);
        }
    }

    fn effective_zoom(&self) -> f32 {
        if self.zoom < 1.0 { 1.0 } else { self.zoom }
    }

    /// On-screen rect of the whole image for the given area.
    pub fn image_rect(&self, area: Rect, size: [u32; 2]) -> Rect {
        let fit = bi_render::fit_rect(size, area);
        let z = self.effective_zoom();
        let s = fit.size() * z;
        let mut c = self.center;
        let half = vec2(area.width() / s.x / 2.0, area.height() / s.y / 2.0);
        c.x = if s.x <= area.width() { 0.5 } else { c.x.clamp(half.x, 1.0 - half.x) };
        c.y = if s.y <= area.height() { 0.5 } else { c.y.clamp(half.y, 1.0 - half.y) };
        let min = area.center() - vec2(c.x * s.x, c.y * s.y);
        Rect::from_min_size(min, s)
    }

    pub fn zoom_at(&mut self, area: Rect, size: [u32; 2], cursor: Pos2, factor: f32) {
        let d = self.image_rect(area, size);
        let p = (cursor - d.min) / d.size();
        let new_zoom = (self.effective_zoom() * factor).clamp(1.0, 50.0);
        let fit = bi_render::fit_rect(size, area);
        let ns = fit.size() * new_zoom;
        let nmin = cursor - vec2(p.x * ns.x, p.y * ns.y);
        self.zoom = new_zoom;
        self.center = vec2((area.center().x - nmin.x) / ns.x, (area.center().y - nmin.y) / ns.y);
    }

    pub fn pan_by(&mut self, area: Rect, size: [u32; 2], delta: Vec2) {
        let d = self.image_rect(area, size);
        self.center -= vec2(delta.x / d.width(), delta.y / d.height());
    }
}

/// Per-group overrides from `ui3_groupCfg` (`GroupCfg`, ui3.js:25472).
pub fn group_args(settings: &Settings, id: &str) -> GroupArgs {
    let cfg: serde_json::Value = serde_json::from_str(&settings.str("ui3_groupCfg")).unwrap_or_default();
    let g = &cfg[id.to_ascii_lowercase()];
    let v = |k: &str| g.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as u8;
    GroupArgs { show_camera_names: v("scn"), show_camera_borders: v("scb"), show_hidden_cameras: v("shc"), hide_disabled_cameras: v("hdc"), hide_inactive_cameras: v("hicwv") }
}

/// Camera rects inside the current image: (camera id, rect in image pixels), and the image's native size.
pub fn camera_rects(view: &MainView, frame_size: Option<[u32; 2]>) -> (Vec<(String, [i32; 4])>, [f32; 2]) {
    let id = match &view.content {
        Content::Live { id } | Content::Timeline { id } => id.clone(),
        _ => return (vec![], [1.0, 1.0]),
    };
    let headers = view.player.headers();
    if !headers.camlist.is_empty() && headers.camlist.len() == headers.reclist.len()
        && let Some(fs) = frame_size
    {
        let r = headers.camlist.iter().cloned().zip(headers.reclist.iter().copied()).collect();
        return (r, [fs[0] as f32, fs[1] as f32]);
    }
    if let Some(cam) = view.camera(&id) {
        if let (Some(g), Some(rects)) = (&cam.group, &cam.rects)
            && g.len() == rects.len()
        {
            return (g.iter().cloned().zip(rects.iter().copied()).collect(), [cam.width.max(1) as f32, cam.height.max(1) as f32]);
        }
        let fs = frame_size.unwrap_or([cam.width as u32, cam.height as u32]);
        return (vec![(cam.id.clone(), [0, 0, fs[0] as i32, fs[1] as i32])], [fs[0].max(1) as f32, fs[1].max(1) as f32]);
    }
    (vec![], [1.0, 1.0])
}

fn camera_at(view: &MainView, img_rect: Rect, pos: Pos2, frame_size: Option<[u32; 2]>) -> Option<String> {
    let (rects, native) = camera_rects(view, frame_size);
    let rel = (pos - img_rect.min) / img_rect.size();
    let (x, y) = (rel.x * native[0], rel.y * native[1]);
    rects.into_iter().find(|(_, r)| x > r[0] as f32 && y > r[1] as f32 && x < r[2] as f32 && y < r[3] as f32).map(|(id, _)| id)
}

pub fn show(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services) -> VideoInput {
    let mut input = VideoInput::default();
    let area = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(area, Sense::click_and_drag());
    view.video.last_area = Some(area);
    let painter = ui.painter_at(area);
    let size = view.video.picture_size();

    if let (Some(size), Some(tex_id)) = (size, view.video.tex.as_ref().and_then(|t| t.texture_id())) {
        let img_rect = view.video.image_rect(area, size);
        view.video.last_image_rect = Some(img_rect);
        painter.image(tex_id, img_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);

        // 3D positioning PTZ: click centers, wheel zooms (instead of digital zoom).
        let relative_cam = if view.ptz.relative_mode { view.current_single_camera().filter(|c| c.ptz).map(|c| c.id.clone()) } else { None };
        if let Some(cam) = &relative_cam {
            if resp.clicked()
                && let Some(p) = resp.interact_pointer_pos()
            {
                let rel = (p - img_rect.min) / img_rect.size();
                view.ptz.enqueue(cam, crate::views::ptz::PtzAction::Relative { x: rel.x as f64, y: rel.y as f64, z: 0.0 });
            }
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if resp.hovered()
                && scroll.abs() > 20.0
                && let Some(p) = resp.hover_pos()
            {
                let rel = (p - img_rect.min) / img_rect.size();
                view.ptz.enqueue(cam, crate::views::ptz::PtzAction::Relative { x: rel.x as f64, y: rel.y as f64, z: if scroll > 0.0 { 0.25 } else { -0.25 } });
            }
            draw_camera_labels(view, &painter, img_rect, Some(size), svc.settings);
            draw_status_icons(view, ui, area, svc.settings);
            return input;
        }
        // Digital zoom: wheel at cursor, drag to pan (ui3 "Digital Zoom" settings).
        // Single cameras only: grids and cycles don't zoom.
        let can_zoom = match &view.content {
            Content::Live { id } | Content::Timeline { id } => view.camera(id).is_some_and(|c| !c.is_group() && !c.is_cycle()),
            _ => view.current_single_camera().is_some(),
        };
        if can_zoom && resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y + i.zoom_delta().ln() * 300.0);
            if scroll.abs() > 0.1
                && let Some(p) = resp.hover_pos()
            {
                let rev = svc.settings.bool("ui3_wheelZoomReverse");
                // Proportional to the scroll amount (egui spreads one wheel notch over several frames), scaled by the speed setting.
                let speed = (svc.settings.f64("ui3_wheelAdjustableSpeed").clamp(20.0, 2000.0) / 400.0) as f32;
                let step = 1.0 + (scroll.abs() * speed / 400.0).min(0.5);
                let zoom_in = (scroll > 0.0) != rev;
                view.video.zoom_at(area, size, p, if zoom_in { step } else { 1.0 / step });
            }
        }
        if can_zoom && resp.dragged() && view.video.effective_zoom() > 1.0 {
            view.video.pan_by(area, size, resp.drag_delta());
        }
        let frame_size = Some(size);
        if resp.double_clicked() {
            input.double_clicked = Some(resp.interact_pointer_pos().and_then(|p| camera_at(view, img_rect, p, frame_size)));
        } else if resp.clicked() {
            input.clicked = Some(resp.interact_pointer_pos().and_then(|p| camera_at(view, img_rect, p, frame_size)));
        }
        if resp.secondary_clicked() {
            input.context_menu_camera = Some(resp.interact_pointer_pos().and_then(|p| camera_at(view, img_rect, p, frame_size)));
        }

        draw_camera_labels(view, &painter, img_rect, frame_size, svc.settings);
        if matches!(view.content, Content::Live { .. }) {
            draw_camera_overlay_icons(view, &painter, img_rect, frame_size, svc.settings);
        }
        draw_status_icons(view, ui, area, svc.settings);
    } else {
        view.video.last_image_rect = None;
    }

    // Loading overlay / errors.
    let stats = view.player.stats();
    let loading = view.player.is_streaming() && stats.frames_decoded == 0 && !view.video.has_picture();
    if loading || (view.player.is_streaming() && stats.first_frame_ms.is_none() && view.player.params.is_some() && stats.frames_decoded == 0) {
        // #camimg_loading_anim: svg_stroke_loading_circle spinning once per second at 50% opacity.
        let c = area.center();
        let t = ui.input(|i| i.time) as f32;
        let start = t * std::f32::consts::TAU + 258f32.to_radians();
        let sweep = 164.93 / (2.0 * std::f32::consts::PI * 35.0) * std::f32::consts::TAU;
        let pts: Vec<Pos2> = (0..=40).map(|k| {
            let a = start + sweep * k as f32 / 40.0;
            c + vec2(a.cos(), a.sin()) * 35.0
        }).collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(10.0, Color32::from_rgba_unmultiplied(0xDD, 0xDD, 0xDD, 128))));
        ui.ctx().request_repaint();
    }
    if let Some(err) = view.player.error() {
        painter.text(area.left_bottom() + vec2(8.0, -8.0), Align2::LEFT_BOTTOM, err, FontId::proportional(13.0), Color32::LIGHT_RED);
    }
    if svc.settings.native.show_stats_overlay {
        draw_stats(view, &painter, area, svc);
    }
    if matches!(view.content, Content::None) && !view.video.has_picture() {
        painter.text(area.center(), Align2::CENTER_CENTER, "No video", FontId::proportional(18.0), style::t().text);
    }
    input
}

fn draw_camera_labels(view: &MainView, painter: &egui::Painter, img_rect: Rect, frame_size: Option<[u32; 2]>, s: &Settings) {
    if !s.bool("ui3_cameraLabels_enabled") {
        return;
    }
    let (rects, native) = camera_rects(view, frame_size);
    let multi = rects.len() > 1;
    if (multi && !s.bool("ui3_cameraLabels_multiCameras")) || (!multi && !s.bool("ui3_cameraLabels_singleCameras")) {
        return;
    }
    if matches!(view.content, Content::Clip { .. }) {
        return;
    }
    let pos = s.str("ui3_cameraLabels_position");
    let text_mode = s.str("ui3_cameraLabels_text");
    let font_pct = s.f64("ui3_cameraLabels_fontSize").max(1.0) as f32;
    let min_font = s.f64("ui3_cameraLabels_minimumFontSize").max(4.0) as f32;
    let bg_opacity = (s.f64("ui3_cameraLabels_backgroundOpacity") as f32 / 100.0).clamp(0.0, 1.0);
    let text_opacity = (s.f64("ui3_cameraLabels_textOpacity") as f32 / 100.0).clamp(0.0, 1.0);
    let use_cam_color = s.bool("ui3_cameraLabels_cameraColor");
    for (id, r) in rects {
        let Some(cam) = view.camera(&id) else { continue };
        let sx = img_rect.width() / native[0];
        let sy = img_rect.height() / native[1];
        let cr = Rect::from_min_max(pos2(img_rect.min.x + r[0] as f32 * sx, img_rect.min.y + r[1] as f32 * sy), pos2(img_rect.min.x + r[2] as f32 * sx, img_rect.min.y + r[3] as f32 * sy));
        let text = match text_mode.as_str() {
            "Short Name" => cam.id.clone(),
            "Name (Short Name)" => format!("{} ({})", cam.name, cam.id),
            _ => cam.name.clone(),
        };
        let font = (cr.height() * font_pct / 100.0).max(min_font);
        let bg = if use_cam_color { style::rgb3(cam.color_rgb()) } else { style::hex(&s.str("ui3_cameraLabels_backgroundColor")) };
        let fg = style::hex(&s.str("ui3_cameraLabels_textColor"));
        let galley = painter.layout_no_wrap(text, FontId::proportional(font), fg.gamma_multiply(text_opacity));
        let h = galley.size().y + 4.0;
        let y = match pos.as_str() {
            "Bottom" => cr.max.y - h,
            "Below" => cr.max.y,
            "Above" => cr.min.y - h,
            _ => cr.min.y,
        };
        let label_rect = Rect::from_min_size(pos2(cr.center().x - galley.size().x / 2.0 - 4.0, y), vec2(galley.size().x + 8.0, h));
        painter.rect_filled(label_rect, 2.0, bg.gamma_multiply(bg_opacity));
        painter.galley(label_rect.min + vec2(4.0, 2.0), galley, fg);
    }
}

/// Per-camera overlay icons within a group image (`ui3_camera_overlay_icon_*`), from camera-list state.
fn draw_camera_overlay_icons(view: &MainView, painter: &egui::Painter, img_rect: Rect, frame_size: Option<[u32; 2]>, s: &Settings) {
    let (rects, native) = camera_rects(view, frame_size);
    if rects.len() < 2 {
        return;
    }
    // UI3 treats the setting as a scale (0.1-2.0, fallback 1) applied to a base icon size (ui3.js:24505).
    let scale = s.f64("ui3_overlay_icon_size");
    let size = 32.0 * if scale.is_finite() && scale > 0.0 { scale as f32 } else { 1.0 };
    for (id, r) in rects {
        let Some(cam) = view.camera(&id) else { continue };
        let sx = img_rect.width() / native[0];
        let sy = img_rect.height() / native[1];
        let mut x = img_rect.min.x + r[2] as f32 * sx - size * 0.7;
        let y = img_rect.min.y + r[1] as f32 * sy + size * 0.7;
        let mut icon = |on: bool, key: &str, glyph: &str, color: Color32| {
            if on && s.bool(key) {
                painter.circle_filled(pos2(x, y), size * 0.55, Color32::from_black_alpha(140));
                painter.text(pos2(x, y), Align2::CENTER_CENTER, glyph, FontId::proportional(size * 0.8), color);
                x -= size * 1.2;
            }
        };
        icon(cam.is_motion && !cam.is_triggered, "ui3_camera_overlay_icon_motion_trigger", "🏃", style::MOTION_ORANGE);
        icon(cam.is_triggered, "ui3_camera_overlay_icon_generic_trigger", "⚡", style::REC_RED);
        icon(cam.is_recording, "ui3_camera_overlay_icon_recording", "⏺", style::REC_RED);
        icon(cam.is_paused, "ui3_camera_overlay_icon_paused", "⏸", Color32::LIGHT_BLUE);
        icon(!cam.webcast(), "ui3_camera_overlay_icon_webcasting_disabled", "🚫", Color32::GRAY);
        icon(cam.newalerts > 0, "ui3_camera_overlay_icon_new_alerts", "🔔", Color32::from_rgb(0xFF, 0xD5, 0x4F));
    }
}

/// `CornerStatusIcons` (ui3.js:24704): recording / trigger / motion icons in the top-right corner of the video,
/// each enabled by its `ui3_icon_*` setting (off by default, as in UI3).
fn draw_status_icons(view: &MainView, ui: &egui::Ui, area: Rect, s: &Settings) {
    let Some(st) = view.player.status() else { return };
    let icons = [
        (st.rec, "ui3_icon_recording", crate::icons::STOPLIGHT, Color32::from_rgb(255, 0, 0), "A camera is recording"),
        (st.triggered, "ui3_icon_trigger", crate::icons::ALERT1, Color32::from_rgb(255, 64, 64), "A camera is triggered"),
        (st.motion, "ui3_icon_motion", crate::icons::RUN, Color32::from_rgb(120, 205, 255), "Motion is detected"),
    ];
    let mut x = area.right() - 5.0;
    for (on, key, icon, color, _tip) in icons {
        if !on || !s.bool(key) {
            continue;
        }
        let r = Rect::from_min_size(pos2(x - 40.0, area.top() + 5.0), vec2(40.0, 40.0));
        let ir = r.shrink(5.0);
        crate::views::w::paint_icon(ui, icon, ir.translate(vec2(1.0, 1.0)), Color32::from_black_alpha(200));
        crate::views::w::paint_icon(ui, icon, ir, color);
        x -= 50.0;
    }
}

fn draw_stats(view: &MainView, painter: &egui::Painter, area: Rect, svc: &Services) {
    let s = view.player.stats();
    let m = view.player.meta();
    let codec = s.codec.map(|c| c.name()).unwrap_or("-");
    let mut lines = vec![
        format!("Codec: {codec}   Decoder: {}", if s.decoder.is_empty() { "-" } else { &s.decoder }),
        format!("Resolution: {}x{}   FPS: {:.1}", s.width, s.height, s.fps),
        format!("Video: {:.0} Kbps   Audio: {:.0} Kbps {}", s.video_kbps, s.audio_kbps, s.audio_format.clone().unwrap_or_default()),
        format!("Decode: {:.2} ms avg   Frames: {} (skipped {})", s.decode_ms_avg, s.frames_decoded, s.frames_dropped),
        format!("Audio buffer: {:.0} ms   Late: {:.0} ms", s.audio_buffer_ms, s.late_ms),
        format!("Frame UTC: {}   pos {}   t {} ms", fmt_utc(m.utc_ms as i64, view.session.login.tz_offset_ms()), m.pos, m.time_ms),
        format!("GPU: {} ({})", svc.gpu.adapter_name, svc.gpu.backend),
    ];
    if let Some(ff) = s.first_frame_ms {
        lines.push(format!("Time to first frame: {ff:.0} ms"));
    }
    if let Some(p) = &view.player.params {
        let mut path = p.path.clone();
        if path.len() > 110 {
            path.truncate(110);
            path.push('…');
        }
        lines.push(path);
    }
    let text = lines.join("\n");
    let galley = painter.layout_no_wrap(text, FontId::monospace(11.5), Color32::WHITE);
    let r = Rect::from_min_size(area.min + vec2(8.0, 8.0), galley.size() + vec2(12.0, 10.0));
    painter.rect_filled(r, 4.0, Color32::from_black_alpha(170));
    painter.galley(r.min + vec2(6.0, 5.0), galley, Color32::WHITE);
}

/// Formats a UTC ms timestamp in the *server's* time zone (UI3 `GetServerDate`).
pub fn fmt_utc(utc_ms: i64, tz_offset_ms: i64) -> String {
    if utc_ms <= 0 {
        return "-".into();
    }
    chrono::DateTime::from_timestamp_millis(utc_ms + tz_offset_ms).map(|d| d.naive_utc().format("%Y-%m-%d %H:%M:%S%.3f").to_string()).unwrap_or_default()
}
