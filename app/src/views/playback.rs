//! Recorded clip playback. Port of `PlaybackControls` (ui3.js:11692), `SeekBar` (12449), `PlaybackHeader`, and the
//! clip branch of `FetchH264VideoModule.OpenVideo` (20739-20827).
//!
//! Fast scrubbing: while the seek bar is dragged the stream is stopped and single JPEG frames are fetched with
//! `file/clips/<path>?time=<ms>` — one request in flight, newest position wins, results cached ([`ScrubLoader`]).
//! Releasing reopens the H.264/H.265 stream at the exact millisecond (`&time=`), or fetches one `speed=0` JPEG
//! when paused.

use crate::app::Services;
use crate::player::{OpenParams, PlayMode, Player};
use crate::runtime::{self, Pending};
use crate::scrub::{ScrubLoader, ScrubRequest};
use crate::settings::Settings;
use crate::views::main_view::{Content, MainView};
use crate::views::style;
use anyhow::Result;
use bi_proto::BiClient;
use bi_proto::api::{ClipItem, flags};
use bi_proto::profile;
use bi_proto::stream::EndReason;
use bi_proto::urls::{self, AudioRequest, ClipStart};
use egui::{Color32, Sense, pos2, vec2};


pub const SPEEDS: &[f64] = &[0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 6.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0];

struct Drag {
    was_playing: bool,
}

pub struct Playback {
    pub clip: Option<ClipItem>,
    /// Clip duration in ms (refined from `clipstats` for alerts).
    pub msec: i64,
    pub paused: bool,
    pub speed: f64,
    pub reverse: bool,
    /// Last known position in ms from clip start.
    pub position_ms: i64,
    /// Position the current stream was opened at, plus stream time when opened.
    opened_at_ms: i64,
    drag: Option<Drag>,
    scrub: ScrubLoader,
    hint: ScrubLoader,
    hint_tex: Option<(i64, egui::TextureHandle)>,
    stats_job: Option<Pending<Result<(i64, String)>>>,
    /// Frame interval for 1-frame stepping (ms).
    pub frame_ms: i64,
    pub export_start: Option<i64>,
    pub export_end: Option<i64>,
    pub last_scrub_latency: f64,
    /// Native resolution of the clip's camera (for request sizing).
    pub native: (i32, i32),
}

impl Playback {
    pub fn new(settings: &Settings) -> Self {
        let speed = settings.f64("ui3_playback_speed");
        Playback {
            clip: None,
            msec: 0,
            paused: false,
            speed: if SPEEDS.contains(&speed) { speed } else { 1.0 },
            reverse: settings.bool("ui3_playback_reverse"),
            position_ms: 0,
            opened_at_ms: 0,
            drag: None,
            scrub: ScrubLoader::new(settings.native.scrub_cache_frames),
            hint: ScrubLoader::new(64),
            hint_tex: None,
            stats_job: None,
            frame_ms: 100,
            export_start: None,
            export_end: None,
            last_scrub_latency: 0.0,
            native: (1920, 1080),
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn camera_id(&self) -> Option<String> {
        self.clip.as_ref().map(|c| c.camera.clone())
    }

    pub fn close(&mut self) {
        self.clip = None;
        self.drag = None;
        self.export_start = None;
        self.export_end = None;
    }

    fn signed_speed(&self) -> i32 {
        let s = (self.speed * 100.0).round() as i32;
        if self.reverse { -s } else { s }
    }

    /// Opens the clip stream at `ms` (`play` = false fetches a single JPEG).
    pub fn open_at(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services, ms: i64, play: bool) {
        let Some(clip) = self.clip.clone() else { return };
        let ms = ms.clamp(0, (self.msec - 1).max(0));
        let speed = if play { self.signed_speed() } else { 0 };
        let audio = if speed == 100 { crate::views::main_view::pick_audio(svc.settings, svc.audio) } else { AudioRequest::None };
        let prof = svc.settings.playback_profile();
        let pargs = prof.url_args(prof.size_to_request(self.native, None), profile::SupportedCodecs::default(), Some(svc.settings.f64("ui3_streamingProfileBitRateMax") as i32));
        let fwd = svc.settings.str("ui3_forward_speed_substream_threshold").parse().unwrap_or(4.0);
        let rev = svc.settings.str("ui3_reverse_speed_substream_threshold").parse().unwrap_or(2.0);
        let sub = urls::speed_substream_arg(self.speed, self.reverse, fwd, rev);
        let overlay = crate::views::dialogs::overlay_args(svc.settings, &clip.camera).args();
        let path = urls::clip_video(clip.video_path(), ClipStart::Time(ms), speed, audio, &pargs, &overlay, sub);
        player.open(
            client,
            OpenParams {
                path,
                hold_placeholder: false,
                mode: if play { PlayMode::Paced } else { PlayMode::Live },
                timestamp_scale: if speed == 0 { 1.0 } else { speed as f64 / 100.0 },
                audio: audio != AudioRequest::None,
                accel: MainView::hw_accel(svc.settings),
            },
            Some(svc.audio.clone()),
            ctx,
        );
        self.paused = !play;
        self.position_ms = ms;
        self.opened_at_ms = ms;
    }

    pub fn reopen(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services) {
        let ms = self.position_ms;
        let play = !self.paused;
        self.open_at(ctx, client, player, svc, ms, play);
    }

    pub fn toggle_pause(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services) {
        if self.clip.is_none() {
            return;
        }
        if self.paused {
            let mut ms = self.position_ms;
            // At the end (or start in reverse): restart from the other end.
            if !self.reverse && ms >= self.msec - 50 {
                ms = 0;
            } else if self.reverse && ms <= 50 {
                ms = self.msec - 1;
            }
            self.open_at(ctx, client, player, svc, ms, true);
        } else {
            player.stop();
            svc.audio.clear();
            self.paused = true;
        }
    }

    pub fn seek_to(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services, ms: i64) {
        let play = !self.paused;
        self.open_at(ctx, client, player, svc, ms, play);
    }

    pub fn skip(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services, delta_ms: i64) {
        let ms = self.position_ms + delta_ms;
        self.seek_to(ctx, client, player, svc, ms);
    }

    /// `SkipAhead1Frame` / `SkipBack1Frame`: pause and step by the camera's frame interval.
    pub fn step_frame(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services, dir: i64) {
        let ms = self.position_ms + dir * self.frame_ms.max(1);
        self.open_at(ctx, client, player, svc, ms, false);
    }

    pub fn change_speed(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services, step: i32) {
        let i = SPEEDS.iter().position(|s| *s == self.speed).unwrap_or(3) as i32 + step;
        if i >= 0 && (i as usize) < SPEEDS.len() {
            self.set_speed(ctx, client, player, svc, SPEEDS[i as usize]);
        }
    }

    pub fn set_speed(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services, speed: f64) {
        self.speed = speed;
        svc.settings.set("ui3_playback_speed", serde_json::json!(speed));
        svc.toasts.info(format!("Playback speed: {}x", fmt_speed(speed)));
        if !self.paused {
            self.reopen(ctx, client, player, svc);
        }
    }

    pub fn toggle_reverse(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services) {
        self.reverse = !self.reverse;
        svc.settings.set_bool("ui3_playback_reverse", self.reverse);
        if !self.paused {
            self.reopen(ctx, client, player, svc);
        }
    }

    fn update_position(&mut self, player: &Player) {
        if self.drag.is_some() || self.paused && player.ended().is_some() {
            return;
        }
        let m = player.meta();
        if player.params.is_some() && (m.pos > 0 || m.time_ms > 0) {
            self.position_ms = ((m.pos as f64 / 10000.0) * self.msec as f64) as i64;
        }
    }
}

pub fn fmt_speed(s: f64) -> String {
    if s < 1.0 { format!("{}", s) } else { format!("{}", s as i64) }
}

pub fn fmt_dur(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Opens a clip/alert from the list (`ClipClicked` -> `videoPlayer.LoadClip`).
pub fn open_clip(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, clip: ClipItem) {
    view.clips.select_only(&clip.path);
    let pb = &mut view.playback;
    pb.clip = Some(clip.clone());
    pb.msec = clip.duration_ms().max(1);
    pb.export_start = None;
    pb.export_end = None;
    pb.scrub.clear_cache();
    pb.hint.clear_cache();
    pb.hint_tex = None;
    if let Some(cam) = view.cameras.iter().find(|c| c.id.eq_ignore_ascii_case(&clip.camera)) {
        pb.frame_ms = if cam.fps > 0.5 { (1000.0 / cam.fps).round() as i64 } else { 100 };
        pb.native = (cam.width.max(16) as i32, cam.height.max(16) as i32);
    }
    // Alerts start at their offset when Blue Iris supplied it in ms (ALERT_OFFSETTIME); reverse starts at the end.
    let mut start = 0;
    if !clip.is_clip() && !clip.is_snapshot() && clip.has_flag(flags::ALERT_OFFSETTIME) {
        start = clip.offset;
        if pb.reverse {
            start += clip.rough_length_ms();
        }
        // Alert durations are rough; the real clip length comes from clipstats.
        pb.msec = pb.msec.max(start + 1);
        let c = view.client.clone();
        let path = clip.video_path().to_string();
        pb.stats_job = Some(runtime::spawn(ctx, async move { c.clip_stats(&path).await }));
    } else if pb.reverse {
        start = pb.msec - 1;
    }
    view.content = Content::Clip { key: clip.path.clone() };
    view.video.reset_zoom();
    // UI3 always starts playing a clip when it is opened (ui3_playback_autoplay governs advancing to the next clip).
    view.playback.open_at(ctx, &view.client, &mut view.player, svc, start, true);
    view.controls.poke();
}

/// Per-frame clip bookkeeping: clipstats refinement, end-of-clip (loop / autoplay), scrub and hint results.
pub fn update(view: &mut MainView, ctx: &egui::Context, svc: &mut Services) {
    let Some(clip) = view.playback.clip.clone() else { return };

    // clipstats refinement for alerts.
    if let Some(j) = &mut view.playback.stats_job
        && let Some(r) = j.poll()
    {
        view.playback.stats_job = None;
        if let Ok((ms, _)) = r
            && ms > 0
        {
            view.playback.msec = ms;
        }
    }
    view.playback.update_position(&view.player);

    // End of clip handling (loop / autoplay next).
    if !view.playback.paused
        && let Some(reason) = view.player.take_ended()
    {
        if reason == EndReason::Natural || reason == EndReason::Eof {
            let at_end = if view.playback.reverse { 0 } else { view.playback.msec - 1 };
            view.playback.position_ms = at_end;
            if svc.settings.bool("ui3_playback_loop") {
                let start = if view.playback.reverse { view.playback.msec - 1 } else { 0 };
                view.playback.open_at(ctx, &view.client, &mut view.player, svc, start, true);
            } else if svc.settings.bool("ui3_playback_autoplay") {
                // `neighbor` direction: +1 = older. "Oldest First" review order advances toward newer clips.
                let dir = if svc.settings.str("ui3_clip_navigation_direction") == "Oldest First" { -1 } else { 1 };
                let dir = if view.playback.reverse { -dir } else { dir };
                if let Some(next) = view.clips.neighbor(&clip.path, dir) {
                    open_clip(view, ctx, svc, next);
                    return;
                }
                view.playback.paused = true;
            } else {
                view.playback.paused = true;
            }
        } else {
            svc.toasts.warn(format!("Playback stopped: {reason:?}"));
            view.playback.paused = true;
        }
    }

    // Scrub results.
    if let Some(f) = view.playback.scrub.poll(ctx, &view.client) {
        view.playback.last_scrub_latency = f.latency_ms;
        view.player.set_still(f.image);
    }
    if let Some(f) = view.playback.hint.poll(ctx, &view.client) {
        let img = egui::ColorImage::from_rgba_unmultiplied([f.image.width as usize, f.image.height as usize], &f.image.rgba);
        view.playback.hint_tex = Some((f.tag, ctx.load_texture("seekhint", img, egui::TextureOptions::LINEAR)));
    }
}

/// UI3's `#seekBarWrapper` (16px tall hit area, 4px track that grows to 8px with a square handle on hover/drag).
/// Dragging scrubs with single JPEG frames; releasing reopens the stream at that millisecond.
pub fn seek_bar(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services, rect: egui::Rect) {
    let ctx = ui.ctx().clone();
    let Some(clip) = view.playback.clip.clone() else { return };
    let resp = ui.interact(rect, ui.id().with("seekbar"), Sense::click_and_drag());
    let focus = resp.hovered() || view.playback.drag.is_some();
    let th = style::t();
    let track_h = if focus { 8.0 } else { 4.0 };
    let track = egui::Rect::from_center_size(rect.center(), vec2(rect.width(), track_h));
    let p = ui.painter();
    p.rect_filled(track, 0.0, Color32::from_rgb(0x37, 0x3B, 0x46));
    let msec = view.playback.msec.max(1);
    let frac = (view.playback.position_ms as f32 / msec as f32).clamp(0.0, 1.0);
    let x_at = |f: f32| track.left() + track.width() * f.clamp(0.0, 1.0);
    if let Some(hp) = resp.hover_pos() {
        p.rect_filled(egui::Rect::from_min_max(track.min, pos2(hp.x.clamp(track.left(), track.right()), track.bottom())), 0.0, Color32::from_white_alpha(38));
    }
    p.rect_filled(egui::Rect::from_min_max(track.min, pos2(x_at(frac), track.bottom())), 0.0, th.seek);
    // Alert range within the clip (UI3 highlights the alert's span).
    if !clip.is_clip() {
        let a0 = clip.offset as f32 / msec as f32;
        let a1 = (clip.offset + clip.rough_length_ms()) as f32 / msec as f32;
        p.rect_filled(egui::Rect::from_min_max(pos2(x_at(a0), track.top() - 2.0), pos2(x_at(a1), track.top())), 0.0, style::MOTION_ORANGE);
    }
    // Export range markers (green start, red end flag poles).
    for (m, c) in [(view.playback.export_start, Color32::from_rgba_unmultiplied(0, 255, 0, 160)), (view.playback.export_end, Color32::from_rgba_unmultiplied(255, 0, 0, 160))] {
        if let Some(ms) = m {
            let x = x_at(ms as f32 / msec as f32);
            p.line_segment([pos2(x, rect.top() - 10.0), pos2(x, rect.bottom())], egui::Stroke::new(2.0, c));
        }
    }
    if focus {
        // #seekBarHandle: 8x8 box with a 4px #E8E8E8 border.
        let h = egui::Rect::from_center_size(pos2(x_at(frac), track.center().y), vec2(16.0, 16.0));
        p.rect_filled(h, 0.0, Color32::from_rgb(0xE8, 0xE8, 0xE8));
        p.rect_filled(h.shrink(4.0), 0.0, Color32::from_rgb(0x18, 0x18, 0x18));
    }

    let ms_at = |x: f32| -> i64 { (((x - track.left()) / track.width()).clamp(0.0, 1.0) as f64 * (msec - 1) as f64) as i64 };
    let tz = view.session.login.tz_offset_ms();
    let h24 = svc.settings.bool("ui3_time24hour");
    if resp.drag_started() || (resp.is_pointer_button_down_on() && view.playback.drag.is_none()) {
        let was_playing = !view.playback.paused;
        view.player.stop();
        svc.audio.clear();
        view.playback.paused = true;
        view.playback.drag = Some(Drag { was_playing });
    }
    if view.playback.drag.is_some()
        && let Some(pp) = ui.input(|i| i.pointer.interact_pos())
    {
        let ms = ms_at(pp.x);
        if ms != view.playback.position_ms || resp.drag_started() {
            view.playback.position_ms = ms;
            let prof = svc.settings.current_profile();
            let mbps = view.player.stats().video_kbps / 1000.0;
            let mut q = profile::seek_preview_quality_args(&prof, view.playback.native, mbps, false);
            if svc.settings.bool("ui3_seek_with_substream") {
                q.push_str("&decode=-1");
            }
            let path = urls::clip_frame_jpeg(clip.video_path(), ms, &q, "");
            if let Some(f) = view.playback.scrub.request(&ctx, &view.client, ScrubRequest { path, tag: ms }) {
                view.player.set_still(f.image);
            }
        }
        if !ui.input(|i| i.pointer.any_down()) {
            // Released.
            let d = view.playback.drag.take().unwrap();
            view.playback.scrub.cancel_queued();
            let ms = view.playback.position_ms;
            view.playback.open_at(&ctx, &view.client, &mut view.player, svc, ms, d.was_playing);
        }
        ctx.request_repaint();
    }
    // Hover: #seekhint — time label plus a 160px thumbnail while paused.
    let hint_x = if view.playback.drag.is_some() { Some(x_at(frac)) } else { resp.hover_pos().map(|p| p.x) };
    if let Some(hx) = hint_x {
        let ms = if view.playback.drag.is_some() { view.playback.position_ms } else { ms_at(hx) };
        let clip_start_utc = clip.date * 1000 - if clip.is_clip() { 0 } else { clip.offset };
        let text = if view.playback.drag.is_some() { crate::views::clips::server_time_str(clip_start_utc + ms, tz, h24) } else { fmt_dur(ms) };
        let show_thumb = view.playback.paused && view.playback.drag.is_none();
        if show_thumb {
            let bucket = ms / (msec / 200).max(1) * (msec / 200).max(1);
            let path = urls::clip_frame_jpeg(clip.video_path(), bucket, "&w=160&h=160&q=50&stream=0", "&decode=-1");
            if let Some(f) = view.playback.hint.request(&ctx, &view.client, ScrubRequest { path, tag: bucket }) {
                let img = egui::ColorImage::from_rgba_unmultiplied([f.image.width as usize, f.image.height as usize], &f.image.rgba);
                view.playback.hint_tex = Some((f.tag, ctx.load_texture("seekhint", img, egui::TextureOptions::LINEAR)));
            }
        }
        let thumb_h = if show_thumb && view.playback.hint_tex.is_some() { 90.0 } else { 0.0 };
        let tip_h = thumb_h + 26.0;
        let tip = egui::Rect::from_min_size(pos2((hx - 82.0).clamp(rect.left(), rect.right() - 164.0), rect.top() - tip_h - 10.0), vec2(164.0, tip_h));
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("seektip")));
        painter.rect_filled(tip, 2.0, Color32::from_black_alpha(204));
        painter.rect_stroke(tip, 2.0, egui::Stroke::new(2.0, Color32::BLACK), egui::StrokeKind::Inside);
        if thumb_h > 0.0
            && let Some((_, t)) = &view.playback.hint_tex
        {
            let r = egui::Rect::from_min_size(tip.min + vec2(2.0, 2.0), vec2(160.0, 90.0));
            painter.image(t.id(), bi_render::fit_rect([t.size()[0] as u32, t.size()[1] as u32], r), egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        painter.text(pos2(tip.center().x, tip.bottom() - 12.0), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(16.0), Color32::WHITE);
    }
}
