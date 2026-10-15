//! Timeline tab. Port of `TimelineDataLoader` (ui3.js:9471), `ClipTimeline` (9670, canvas drawing, pan/zoom/pinch,
//! seek preview via `/time/<id>?jpeg`, `userDidSetTime`, go-live at the keep-out zone) and `timelineSync`.

use crate::app::Services;
use crate::player::{OpenParams, PlayMode, Player};
use crate::runtime::{self, Pending};
use crate::scrub::{ScrubLoader, ScrubRequest};
use crate::settings::Settings;
use crate::views::main_view::{Content, MainView};
use crate::icons;
use crate::views::{style, w};
use anyhow::Result;
use bi_proto::BiClient;
use bi_proto::api::{TimelineData, bi_color_to_rgb, flags};
use bi_proto::profile;
use bi_proto::urls::{self, AudioRequest};
use egui::{Color32, FontId, Pos2, Rect, Sense, Stroke, pos2, vec2};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const KEEP_OUT_MS: i64 = 15_000;
const MAX_ZOOM: f64 = 30.0;
const MIN_SAVED_ZOOM: f64 = 6.0;
const MAX_SAVED_ZOOM: f64 = 19.0;

#[derive(Clone)]
struct TlAlert {
    time: i64,
    len: i64,
    is_flag: bool,
    is_person: bool,
    is_vehicle: bool,
    is_wildlife: bool,
    tracks: i64,
    rec_id: Option<String>,
}

#[derive(Clone)]
struct TlClip {
    time: i64,
    len: i64,
    track: usize,
}

#[derive(Default, Clone)]
struct Processed {
    colors: Vec<Color32>,
    alerts: Vec<TlAlert>,
    clips: Vec<TlClip>,
}

#[derive(Clone, PartialEq)]
struct Loaded {
    camera: String,
    left: i64,
    right: i64,
    zoom: f64,
}

pub struct TimelinePanel {
    pub target: String,
    /// Native size of every camera/group by lower-case id, so streams ask for the source's own size like UI3's
    /// `GetSizeToRequest` (ui3.js:23752) instead of a fixed one.
    native_sizes: HashMap<String, (i32, i32)>,
    pub zoom_scaler: f64,
    /// Unbounded time the user wants (UI3 `lastSetTime`).
    pub last_set_time: i64,
    width_px: f32,
    data: Option<Processed>,
    loaded: Option<Loaded>,
    job: Option<(Loaded, Pending<Result<TimelineData>>)>,
    last_failure: Option<Instant>,
    pub error: Option<String>,
    drag_start_x: Option<f32>,
    dragging: bool,
    wheel_pan_until: Option<Instant>,
    zoom_accum: f64,
    pub paused: bool,
    did_pause: bool,
    pub is_live: bool,
    seek: ScrubLoader,
    thumbs: HashMap<String, Option<egui::TextureHandle>>,
    thumb_jobs: Vec<(String, Pending<Result<egui::ColorImage>>)>,
    pub server_offset_ms: i64,
    hover_x: Option<f32>,
    pub export_range: Option<(i64, i64)>,
    export_mark_start: Option<i64>,
}

impl TimelinePanel {
    pub fn new(settings: &Settings) -> Self {
        TimelinePanel {
            target: String::new(),
            zoom_scaler: settings.f64("ui3_timelineZoomScaler").clamp(MIN_SAVED_ZOOM, MAX_SAVED_ZOOM),
            last_set_time: now_ms(),
            width_px: 1000.0,
            data: None,
            loaded: None,
            native_sizes: HashMap::new(),
            job: None,
            last_failure: None,
            error: None,
            drag_start_x: None,
            dragging: false,
            wheel_pan_until: None,
            zoom_accum: 0.0,
            paused: false,
            did_pause: false,
            is_live: true,
            seek: ScrubLoader::new(settings.native.scrub_cache_frames),
            thumbs: HashMap::new(),
            thumb_jobs: Vec::new(),
            server_offset_ms: 0,
            hover_x: None,
            export_range: None,
            export_mark_start: None,
        }
    }

    pub fn set_camera_sizes(&mut self, cams: &[bi_proto::api::CameraInfo]) {
        self.native_sizes = cams.iter().map(|c| (c.id.to_ascii_lowercase(), (c.width.max(16) as i32, c.height.max(16) as i32))).collect();
    }

    /// Native size of the current target (1920x1080 until the camera list is known).
    fn native_size(&self) -> (i32, i32) {
        self.native_sizes.get(&self.target.to_ascii_lowercase()).copied().unwrap_or((1920, 1080))
    }

    pub fn set_export_start(&mut self) {
        let t = self.current_time();
        self.export_mark_start = Some(t);
        self.export_range = Some((t, t));
    }

    pub fn set_export_end(&mut self) {
        let t = self.current_time();
        let s = self.export_mark_start.unwrap_or(t);
        self.export_range = Some((s.min(t), s.max(t)));
    }

    pub fn clear_export(&mut self) {
        self.export_range = None;
        self.export_mark_start = None;
    }

    /// Re-opens playback after a speed / direction change (no-op while live or paused).
    pub fn speed_changed(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services) {
        if !self.paused && !self.is_live {
            self.user_did_set_time(ctx, client, player, svc);
        }
    }

    fn server_now(&self) -> i64 {
        now_ms() + self.server_offset_ms
    }

    fn zoom_factor(&self) -> f64 {
        2f64.powf(self.zoom_scaler)
    }

    pub fn current_time(&self) -> i64 {
        self.last_set_time.clamp(1, self.server_now())
    }

    fn left(&self) -> i64 {
        self.current_time() - (self.width_px as f64 * self.zoom_factor() / 2.0) as i64
    }

    fn right(&self) -> i64 {
        self.current_time() + (self.width_px as f64 * self.zoom_factor() / 2.0) as i64
    }

    fn min_zoom(settings: &Settings) -> f64 {
        settings.f64("ui3_timeline_minZoomScaler").clamp(4.0, 8.0)
    }

    fn set_zoom(&mut self, settings: &mut Settings, z: f64) {
        self.zoom_scaler = z.clamp(Self::min_zoom(settings), MAX_ZOOM);
        settings.set("ui3_timelineZoomScaler", serde_json::json!(self.zoom_scaler.clamp(MIN_SAVED_ZOOM, MAX_SAVED_ZOOM)));
    }

    fn assign_last_set_time(&mut self, t: i64) {
        let now = self.server_now();
        self.last_set_time = if t < 1 {
            1
        } else if t > now - KEEP_OUT_MS {
            now
        } else {
            t
        };
    }

    fn safety_ms(&self) -> f64 {
        (200.0 * self.zoom_factor()).max((self.right() - self.left()) as f64 * 0.25)
    }

    /// `RequiresUpdatedView` + request (ui3.js:9490-9597).
    fn update_data(&mut self, ctx: &egui::Context, client: &BiClient) {
        if self.target.is_empty() {
            return;
        }
        if let Some((want, j)) = &mut self.job {
            if let Some(r) = j.poll() {
                let want = want.clone();
                self.job = None;
                match r {
                    Ok(d) => {
                        self.error = None;
                        self.data = Some(process(&d, want.left, want.zoom));
                        self.loaded = Some(want);
                    }
                    Err(e) => {
                        self.error = Some(format!("{e:#}"));
                        self.last_failure = Some(Instant::now());
                    }
                }
            } else {
                return;
            }
        }
        if self.last_failure.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        let safety = self.safety_ms();
        let live = self.server_now();
        let mut left = (self.left() as f64 - safety * 2.0) as i64;
        let mut right = (self.right() as f64 + safety * 2.0) as i64;
        if left >= live {
            return;
        }
        right = right.min(live).max(128_000);
        left = left.max(1);
        let needs = match &self.loaded {
            None => true,
            Some(l) if l.camera != self.target => true,
            Some(l) if (l.zoom - self.zoom_scaler).abs() > 0.25 => true,
            Some(l) => {
                let load_left = ((self.left() as f64 - safety) as i64).max(1);
                let load_right = ((self.right() as f64 + safety) as i64).max(128_000);
                if load_left < l.left {
                    true
                } else if load_right > l.right {
                    if live >= load_right {
                        true
                    } else {
                        let significant = (self.zoom_factor() * 5.0).max(10_000.0) as i64;
                        l.right < live - significant
                    }
                } else {
                    false
                }
            }
        };
        if !needs {
            return;
        }
        let want = Loaded { camera: self.target.clone(), left, right, zoom: self.zoom_scaler };
        let msecpp = 2f64.powf(self.zoom_scaler).round() as i64;
        let c = client.clone();
        let cam = self.target.clone();
        self.job = Some((want, runtime::spawn(ctx, async move { c.timeline(&cam, left, right, msecpp).await })));
    }

    pub fn activate(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services, home: &str) {
        if self.target.is_empty() {
            self.target = home.to_string();
        }
        self.loaded = None;
        // UI3 opens the timeline at the live edge for the current group.
        self.go_live(ctx, client, player, svc);
    }

    pub fn set_target(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services, id: &str) {
        self.target = id.to_string();
        self.loaded = None;
        self.data = None;
        self.thumbs.clear();
        self.user_did_set_time(ctx, client, player, svc);
    }

    /// Opens timeline video at the current time (or live / a still frame).
    pub fn user_did_set_time(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services) {
        self.assign_last_set_time(self.last_set_time);
        let now = self.server_now();
        if self.current_time() >= now - KEEP_OUT_MS {
            // Live.
            self.is_live = true;
            self.paused = false;
            return self.open_live(ctx, client, player, svc);
        }
        self.is_live = false;
        let start_paused = self.paused && !self.did_pause;
        self.open_at(ctx, client, player, svc, self.current_time(), !start_paused, 0);
    }

    fn open_live(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services) {
        let audio = crate::views::main_view::pick_audio(svc.settings, svc.audio);
        let prof = svc.settings.current_profile();
        let args = prof.url_args(prof.size_to_request(self.native_size(), Some(320)), profile::SupportedCodecs::default(), Some(svc.settings.f64("ui3_streamingProfileBitRateMax") as i32));
        let group = crate::views::video::group_args(svc.settings, &self.target).args();
        let path = urls::live_video(&self.target, audio, &args, &group);
        player.open(client, OpenParams { path, hold_placeholder: false, mode: PlayMode::Live, timestamp_scale: 1.0, audio: audio != AudioRequest::None, accel: MainView::hw_accel(svc.settings) }, Some(svc.audio.clone()), ctx);
    }

    #[allow(clippy::too_many_arguments)]
    fn open_at(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services, utc: i64, play: bool, jump: i32) {
        let prof = svc.settings.current_profile();
        let group = crate::views::video::group_args(svc.settings, &self.target).args();
        let overlay = crate::views::dialogs::overlay_args(svc.settings, "*ui3_timeline_pseudocam").args();
        if !play {
            // Paused: one preview JPEG (UI3 `downloadSeekPreview`).
            player.stop();
            let q = profile::seek_preview_quality_args(&prof, self.native_size(), 0.0, true);
            let path = urls::timeline_frame_jpeg(&self.target, client.preview_opaque(), utc, jump, &q, &group, &overlay);
            self.seek.request(ctx, client, ScrubRequest { path, tag: utc });
            self.paused = true;
            return;
        }
        let prof = svc.settings.playback_profile();
        let speed_f = svc.settings.f64("ui3_playback_speed").max(0.125);
        let reverse = svc.settings.bool("ui3_playback_reverse");
        let speed = ((speed_f * 100.0).round() as i32) * if reverse { -1 } else { 1 };
        let audio = if speed == 100 { crate::views::main_view::pick_audio(svc.settings, svc.audio) } else { AudioRequest::None };
        let args = prof.url_args(prof.size_to_request(self.native_size(), Some(320)), profile::SupportedCodecs::default(), Some(svc.settings.f64("ui3_streamingProfileBitRateMax") as i32));
        let skip = match svc.settings.str("ui3_playback_skipDeadAir").as_str() {
            "2" => Some(true),
            "1" => Some(false),
            _ => None,
        };
        let fwd = svc.settings.str("ui3_forward_speed_substream_threshold").parse().unwrap_or(4.0);
        let rev = svc.settings.str("ui3_reverse_speed_substream_threshold").parse().unwrap_or(2.0);
        let sub = urls::speed_substream_arg(speed_f, reverse, fwd, rev);
        let path = urls::timeline_video(&self.target, &client.new_stream_opaque(), utc, jump, audio, &args, &group, speed, skip, &overlay, sub);
        player.open(
            client,
            OpenParams { path, hold_placeholder: true, mode: PlayMode::Paced, timestamp_scale: speed as f64 / 100.0, audio: audio != AudioRequest::None, accel: MainView::hw_accel(svc.settings) },
            Some(svc.audio.clone()),
            ctx,
        );
        self.paused = false;
    }

    pub fn reopen(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services) {
        self.user_did_set_time(ctx, client, player, svc);
    }

    pub fn go_live(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services) {
        self.last_set_time = self.server_now();
        self.paused = false;
        self.user_did_set_time(ctx, client, player, svc);
    }

    pub fn toggle_pause(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services) {
        if self.paused {
            self.paused = false;
            if self.is_live {
                self.last_set_time = self.server_now() - KEEP_OUT_MS - 1000;
            }
            self.user_did_set_time(ctx, client, player, svc);
        } else {
            player.stop();
            svc.audio.clear();
            self.paused = true;
            self.is_live = false;
        }
    }

    /// Previous/next alert (`TimelineJump`, `&jump=±1`).
    pub fn jump(&mut self, ctx: &egui::Context, client: &BiClient, player: &mut Player, svc: &mut Services, dir: i32) {
        let t = self.current_time();
        let play = !self.paused;
        self.is_live = false;
        self.open_at(ctx, client, player, svc, t, play, dir);
    }

    fn thumb(&mut self, ctx: &egui::Context, client: &BiClient, rec_id: &str) -> Option<egui::TextureHandle> {
        if let Some(t) = self.thumbs.get(rec_id) {
            return t.clone();
        }
        self.thumbs.insert(rec_id.to_string(), None);
        let c = client.clone();
        let path = urls::thumbnail(&format!("@{rec_id}"));
        self.thumb_jobs.push((rec_id.to_string(), runtime::spawn(ctx, async move { crate::views::clips::fetch_color_image(&c, &path, None).await })));
        None
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn process(d: &TimelineData, left: i64, zoom: f64) -> Processed {
    let msecpp = 2f64.powf(zoom).round() as i64;
    let offset = (left / 1000) * 1000;
    let colors = d
        .colors
        .iter()
        .map(|c| {
            let v = bi_proto::lenient::value_to_f64(c).unwrap_or(0.0) as i64;
            let rgb = bi_color_to_rgb(v);
            Color32::from_rgb(rgb[0], rgb[1], rgb[2])
        })
        .collect::<Vec<_>>();
    let alerts = d
        .alerts
        .iter()
        .map(|a| TlAlert {
            time: offset + a.x1 * msecpp,
            len: (a.x2 - a.x1) * msecpp,
            is_flag: flags::has(a.kind, flags::FLAGGED),
            is_person: flags::has(a.kind, flags::AI_PERSON),
            is_vehicle: flags::has(a.kind, flags::AI_VEHICLE),
            is_wildlife: flags::has(a.kind, flags::AI_WILDLIFE),
            tracks: a.tracks,
            rec_id: (!a.record.is_empty()).then(|| bi_proto::api::rec_id_from_path(&a.record).to_string()),
        })
        .collect();
    let clips = d.clips.iter().map(|c| TlClip { time: offset + c.x1 * msecpp, len: (c.x2 - c.x1) * msecpp, track: c.track.max(0) as usize }).collect();
    Processed { colors: if colors.is_empty() { vec![Color32::GRAY] } else { colors }, alerts, clips }
}

/// Deterministic per-star randomness for the starfield (stable as the timeline pans).
fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Tick spacing for the legend.
fn tick_interval(ms_per_px: f64) -> i64 {
    const C: &[i64] = &[
        1_000, 2_000, 5_000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000, 900_000, 1_800_000, 3_600_000, 7_200_000, 10_800_000, 21_600_000, 43_200_000, 86_400_000, 172_800_000,
        604_800_000,
    ];
    for &c in C {
        if c as f64 / ms_per_px >= 90.0 {
            return c;
        }
    }
    *C.last().unwrap()
}

pub fn show_panel(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services) {
    let ctx = ui.ctx().clone();
    if view.timeline.target.is_empty() {
        view.timeline.target = view.home_group.clone();
    }
    if let Some(st) = &view.status
        && let Some(t) = st.time
    {
        view.timeline.server_offset_ms = (t as i64 - now_ms()).clamp(-86_400_000, 86_400_000);
    }
    view.timeline.update_data(&ctx, &view.client);

    // Seek preview results -> picture.
    if let Some(f) = view.timeline.seek.poll(&ctx, &view.client) {
        view.player.set_still(f.image);
        if let Some(u) = f.utc
            && !view.timeline.dragging
        {
            view.timeline.last_set_time = u;
        }
    }
    // Thumbnail jobs.
    view.timeline.thumb_jobs.retain_mut(|(id, j)| match j.poll() {
        Some(Ok(img)) => {
            let t = ctx.load_texture(format!("tlthumb-{id}"), img, egui::TextureOptions::LINEAR);
            view.timeline.thumbs.insert(id.clone(), Some(t));
            false
        }
        Some(Err(_)) => false,
        None => true,
    });
    // Follow the playing video's frame time (FrameRendered) and go live at the keep-out zone.
    if !view.timeline.dragging && !view.timeline.paused && !view.timeline.is_live && matches!(view.content, Content::Timeline { .. }) {
        let m = view.player.meta();
        if m.utc_ms > 0 {
            view.timeline.last_set_time = m.utc_ms as i64;
            if m.utc_ms as i64 > view.timeline.server_now() - KEEP_OUT_MS {
                let (c, p) = (view.client.clone(), &mut view.player);
                view.timeline.go_live(&ctx, &c, p, svc);
                view.content = Content::Live { id: view.timeline.target.clone() };
            }
        }
    }
    if view.timeline.is_live && !view.timeline.dragging {
        view.timeline.last_set_time = view.timeline.server_now();
    }

    egui::Panel::bottom("timeline").resizable(false).exact_size(96.0).frame(egui::Frame::new().fill(style::t().panel)).show(ui, |ui| {
        draw(view, ui, svc);
    });
    ctx.request_repaint_after(Duration::from_millis(66));
}

fn draw(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services) {
    let ctx = ui.ctx().clone();
    let full = ui.available_rect_before_wrap();
    let th = style::t();
    // .timelineButtonBar: Go Live, Zoom In, Zoom Out (36px column on the right).
    let bar = Rect::from_min_max(pos2(full.right() - 36.0, full.top()), full.max);
    ui.painter().rect_filled(bar, 0.0, th.panel);
    ui.painter().line_segment([bar.left_top(), bar.right_top()], Stroke::new(1.0, th.dialog_border));
    ui.painter().line_segment([bar.left_top(), bar.left_bottom()], Stroke::new(1.0, th.dialog_border));
    {
        let (client, player) = (view.client.clone(), &mut view.player);
        let tl = &mut view.timeline;
        let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(0.0, 1.0)).translate(vec2(1.0, 1.0))).layout(egui::Layout::top_down(egui::Align::Min)));
        bui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let live_col = if tl.is_live { th.profile_active } else { th.button };
        let r = w::icon_button_colored(&mut bui, icons::CLOCK, vec2(22.0, 22.0), vec2(6.5, 4.5), "Go Live", live_col, false);
        if r.clicked() {
            tl.go_live(&ctx, &client, player, svc);
        }
        if w::icon_button(&mut bui, icons::ZOOM_IN_CROP, vec2(22.0, 22.0), vec2(6.5, 4.5), "Zoom In").clicked() {
            let z = tl.zoom_scaler - 1.0;
            tl.set_zoom(svc.settings, z);
        }
        if w::icon_button(&mut bui, icons::ZOOM_OUT_CROP, vec2(22.0, 22.0), vec2(6.5, 4.5), "Zoom Out").clicked() {
            let z = tl.zoom_scaler + 1.0;
            tl.set_zoom(svc.settings, z);
        }
    }
    let full = Rect::from_min_max(full.min, pos2(bar.left(), full.bottom()));
    let controls = Rect::from_min_size(full.min, vec2(full.width(), 0.0));
    let legend = Rect::from_min_max(pos2(full.left(), controls.bottom()), pos2(full.right(), controls.bottom() + 16.0));
    let canvas = Rect::from_min_max(pos2(full.left(), legend.bottom()), full.max);
    let resp = ui.allocate_rect(Rect::from_min_max(legend.min, canvas.max), Sense::click_and_drag());
    let target_cam = view.camera(&view.timeline.target).cloned();
    let cam_names: std::collections::HashMap<String, String> = view.cameras.iter().map(|c| (c.id.clone(), c.name.clone())).collect();
    let tl = &mut view.timeline;
    tl.width_px = canvas.width().max(10.0);
    let zf = tl.zoom_factor();
    let left = tl.left();
    let now = tl.server_now();
    let p = ui.painter_at(full);
    let x_of = |t: i64| canvas.left() + ((t - left) as f64 / zf) as f32;

    // ---- Interaction ----
    let tz = view.session.login.tz_offset_ms();
    if resp.hovered() || resp.dragged() {
        tl.hover_x = resp.hover_pos().map(|p| p.x);
        let (scroll_x, scroll_y) = ui.input(|i| (i.smooth_scroll_delta.x, i.smooth_scroll_delta.y));
        let rev = if svc.settings.bool("ui3_wheelZoomReverse") { -1.0 } else { 1.0 };
        if scroll_y.abs() > 0.1 {
            let speed = svc.settings.f64("ui3_wheelAdjustableSpeed").clamp(20.0, 2000.0) / 400.0;
            tl.zoom_accum += -(scroll_y as f64) / 50.0 * rev;
            let z = tl.zoom_scaler + tl.zoom_accum * speed;
            tl.zoom_accum = 0.0;
            tl.set_zoom(svc.settings, z);
        }
        if scroll_x.abs() > 0.1 {
            // `ui3_wheelZoomReverse` affects zoom only, not horizontal pan (ui3.js:9995).
            tl.last_set_time += (scroll_x as f64 * zf) as i64;
            tl.wheel_pan_until = Some(Instant::now() + Duration::from_millis(200));
            tl.is_live = false;
        }
        let zoom_delta = ui.input(|i| i.zoom_delta());
        if (zoom_delta - 1.0).abs() > 0.001 {
            let z = tl.zoom_scaler - (zoom_delta as f64).log2();
            tl.set_zoom(svc.settings, z);
        }
    } else {
        tl.hover_x = None;
    }
    if resp.drag_started() {
        tl.drag_start_x = resp.interact_pointer_pos().map(|p| p.x);
        tl.did_pause = !tl.paused;
    }
    if resp.dragged()
        && let (Some(sx), Some(p)) = (tl.drag_start_x, resp.interact_pointer_pos())
    {
        let d = p.x - sx;
        if d.abs() > 3.0 || tl.dragging {
            if !tl.dragging {
                tl.dragging = true;
                tl.is_live = false;
                view.player.stop();
                svc.audio.clear();
            }
            tl.drag_start_x = Some(p.x);
            tl.last_set_time += (-(d as f64) * zf) as i64;
            // Seek preview while panning (single in-flight request, newest wins).
            let t = tl.current_time();
            if t < now - KEEP_OUT_MS {
                let prof = svc.settings.current_profile();
                let mut q = profile::seek_preview_quality_args(&prof, tl.native_size(), 0.0, true);
                if svc.settings.bool("ui3_seek_with_substream") {
                    q.push_str("&decode=-1");
                }
                let group = crate::views::video::group_args(svc.settings, &tl.target).args();
                let overlay = crate::views::dialogs::overlay_args(svc.settings, "*ui3_timeline_pseudocam").args();
                // Quantize to the pixel so we do not request the same frame repeatedly.
                // In f64 throughout: truncating zf before multiplying by ~1e8 pixels lands hours/days away.
                let tq = ((t as f64 / zf).round() * zf).round() as i64;
                let path = urls::timeline_frame_jpeg(&tl.target, view.client.preview_opaque(), tq, 0, &q, &group, &overlay);
                if let Some(f) = tl.seek.request(&ctx, &view.client, ScrubRequest { path, tag: tq }) {
                    view.player.set_still(f.image);
                }
            }
        }
    }
    let wheel_pan_done = tl.wheel_pan_until.is_some_and(|t| Instant::now() >= t);
    if resp.drag_stopped() || wheel_pan_done {
        tl.wheel_pan_until = None;
        let was_drag = tl.dragging;
        tl.dragging = false;
        tl.drag_start_x = None;
        tl.seek.cancel_queued();
        if was_drag || wheel_pan_done {
            let keep_paused = tl.paused && !tl.did_pause;
            tl.paused = keep_paused;
            let (c, pl) = (view.client.clone(), &mut view.player);
            tl.user_did_set_time(&ctx, &c, pl, svc);
            tl.did_pause = false;
        }
    } else if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let mut t = left + ((pos.x - canvas.left()) as f64 * zf) as i64;
        // Only when alert thumbnails are actually shown (`shouldShowAlertThumbnails`, ui3.js:9955).
        let single = target_cam.as_ref().is_none_or(|c| !c.is_group());
        let thumbs_shown = tl.zoom_scaler <= svc.settings.f64("ui3_timeline_alertThumbnailsAppearAtZoomLevel") && (single || svc.settings.bool("ui3_timeline_alertThumbnailsAppearForGroups"));
        if thumbs_shown
            && svc.settings.bool("ui3_timeline_alertThumbnailClickLoadsAlertStartTime")
            && let Some(d) = &tl.data
            && let Some(a) = d.alerts.iter().rev().find(|a| a.rec_id.is_some() && t >= a.time && t <= a.time + (100.0 * zf) as i64)
        {
            t = a.time;
        }
        tl.assign_last_set_time(t);
        tl.is_live = false;
        let (c, pl) = (view.client.clone(), &mut view.player);
        tl.user_did_set_time(&ctx, &c, pl, svc);
    }
    // Keep `content` (variant and id) in step with the timeline's mode and target.
    let on_target = |id: &String| id.eq_ignore_ascii_case(&tl.target);
    if !tl.is_live && !matches!(&view.content, Content::Timeline { id } if on_target(id)) && view.player.params.as_ref().is_some_and(|p| p.path.starts_with("time/")) {
        view.content = Content::Timeline { id: tl.target.clone() };
    } else if tl.is_live && !matches!(&view.content, Content::Live { id } if on_target(id)) && view.player.params.as_ref().is_some_and(|p| p.path.starts_with("video/")) {
        view.content = Content::Live { id: tl.target.clone() };
    }

    // ---- Drawing ----
    p.rect_filled(Rect::from_min_max(legend.min, canvas.max), 0.0, th.timeline_bg);
    if svc.settings.bool("ui3_timeline_starfield") {
        // Stars drift with the timeline (UI3 timelineStarfieldOffset), 80px blocks at 0.3% density.
        // The offset is ~1e9 px, so block/position math stays in f64: in f32 it snaps stars into columns.
        let offset = tl.current_time() as f64 / zf;
        let block = 80.0f64;
        let per_block = (block * block * 0.003).round() as u64;
        let first_block = (offset / block).floor() as i64;
        let blocks_wide = (canvas.width() as f64 / block).ceil() as i64 + 1;
        let blocks_high = (canvas.height() as f64 / block).ceil() as i64;
        for bx in first_block..first_block + blocks_wide {
            for by in 0..blocks_high {
                for i in 0..per_block {
                    let mut seed = (bx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (by as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ i;
                    let mut rand = || {
                        seed = splitmix64(seed);
                        (seed >> 11) as f64 / (1u64 << 53) as f64
                    };
                    let x = canvas.left() + ((bx as f64 + rand()) * block - offset) as f32;
                    let y = canvas.top() + ((by as f64 + rand()) * block) as f32;
                    // Same star parameters as UI3's StarfieldGenerator.
                    let m = (rand() * 1.2) as f32;
                    let hue = [0.0, 60.0, 240.0][(rand() * 3.0) as usize];
                    let sat = (rand() * 31.0).floor() as f32;
                    let lig = 20.0 + (rand() * 40.0).floor() as f32;
                    if x < canvas.left() || x > canvas.right() || y > canvas.bottom() {
                        continue;
                    }
                    p.circle_filled(pos2(x, y), m, style::hsl(hue, sat, lig));
                }
            }
        }
        // Partially-transparent black overlay behind the loadable timeline area.
        let overlay_right = x_of(now - (KEEP_OUT_MS as f64 * 0.8) as i64).min(canvas.right());
        let opacity = ((MAX_ZOOM - tl.zoom_scaler) / 10.0).clamp(0.0, 0.8) + 0.2;
        if overlay_right > canvas.left() {
            p.rect_filled(Rect::from_min_max(canvas.min, pos2(overlay_right, canvas.bottom())), 0.0, Color32::from_black_alpha((opacity * 255.0) as u8));
        }
    }
    // Legend (.timelineLegendBar): labels with a 1px left border; day boundaries are major tags.
    p.rect_filled(legend, 0.0, th.timeline_legend);
    let step = tick_interval(zf);
    let first = ((left + tz) / step + 1) * step - tz;
    let mut t = first;
    let lp = p.with_clip_rect(legend);
    let h24 = svc.settings.bool("ui3_time24hour");
    while t < tl.right() {
        let x = x_of(t);
        let major = (t + tz).rem_euclid(86_400_000) == 0 || step >= 86_400_000;
        let label = chrono::DateTime::from_timestamp_millis(t + tz)
            .map(|d| {
                let d = d.naive_utc();
                if major {
                    d.format("%b %-d").to_string()
                } else if step < 60_000 {
                    if h24 { d.format("%H:%M:%S").to_string() } else { d.format("%-I:%M:%S %p").to_string() }
                } else if h24 {
                    d.format("%H:%M").to_string()
                } else {
                    d.format("%-I:%M %p").to_string()
                }
            })
            .unwrap_or_default();
        let color = if major { th.text_active } else { th.text };
        let g = lp.layout_no_wrap(label, FontId::proportional(12.0), color);
        let r = Rect::from_min_size(pos2(x, legend.top() + 1.0), vec2(g.size().x + 6.0, 15.0));
        lp.rect_filled(r, 0.0, th.timeline_legend);
        lp.line_segment([r.left_top(), r.left_bottom()], Stroke::new(1.0, color));
        lp.galley(pos2(r.left() + 3.0, r.center().y - g.size().y / 2.0), g, color);
        t += step;
    }
    let alert_space = 12.0;
    if let Some(d) = tl.data.clone() {
        let ntracks = d.colors.len().max(1) as f32;
        let bar_h = (canvas.height() - alert_space) / ntracks;
        // Clip bars.
        for c in &d.clips {
            let x = x_of(c.time);
            let w = (c.len as f64 / zf) as f32;
            if x < canvas.right() && x + w > canvas.left() {
                let y1 = canvas.top() + alert_space + c.track as f32 * bar_h;
                let col = d.colors.get(c.track).copied().unwrap_or(Color32::GRAY);
                p.rect_filled(Rect::from_min_size(pos2(x, y1), vec2(w.max(1.0), bar_h - 1.0)), 2.0, col);
            }
        }
        // Alert bars.
        let alert_col = style::hex_or(&svc.settings.str("ui3_timeline_alertBarColor"), "#8E3510");
        for a in &d.alerts {
            let x = x_of(a.time);
            let w = (a.len as f64 / zf) as f32;
            if x < canvas.right() && x + w > canvas.left() {
                p.rect_filled(Rect::from_min_size(pos2(x, canvas.top() + 1.5), vec2(w.max(1.0), alert_space - 3.0)), 2.0, alert_col);
            }
        }
        // Alert thumbnails at deep zoom.
        let single = target_cam.as_ref().is_none_or(|c| !c.is_group());
        let show_thumbs = tl.zoom_scaler <= svc.settings.f64("ui3_timeline_alertThumbnailsAppearAtZoomLevel") && (single || svc.settings.bool("ui3_timeline_alertThumbnailsAppearForGroups"));
        if show_thumbs {
            let y = canvas.top() + alert_space + 4.0;
            let h = canvas.height() - alert_space - 8.0;
            let client = view.client.clone();
            // Visible thumbnails as (rect, texture); the last one under the pointer is the "hovered" alert.
            let mut thumbs: Vec<(Rect, egui::TextureHandle)> = Vec::new();
            for a in &d.alerts {
                let Some(id) = &a.rec_id else { continue };
                let x = x_of(a.time);
                if x > canvas.right() || x < canvas.left() - h * 2.0 {
                    continue;
                }
                if let Some(t) = tl.thumb(&ctx, &client, id) {
                    let s = t.size_vec2();
                    let w = h * s.x / s.y.max(1.0);
                    thumbs.push((Rect::from_min_size(pos2(x, y), vec2(w, h)), t));
                }
            }
            // `ui3_timeline_drawHoveredAlertOnTop` / `...Only` (ui3.js:10175).
            let on_top = svc.settings.bool("ui3_timeline_drawHoveredAlertOnTop");
            let only_hovered = svc.settings.bool("ui3_timeline_drawHoveredAlertOnly");
            let hovered = tl.hover_x.and_then(|hx| thumbs.iter().rposition(|(r, _)| hx >= r.left() && hx <= r.right()));
            let full_uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            for (i, (r, t)) in thumbs.iter().enumerate() {
                let is_hovered = hovered == Some(i);
                if (on_top || only_hovered) && is_hovered {
                    continue; // drawn last
                }
                if only_hovered {
                    continue;
                }
                p.rect_filled(r.translate(vec2(-4.0, 2.0)), 2.0, Color32::from_black_alpha(160));
                p.image(t.id(), *r, full_uv, Color32::WHITE);
            }
            if (on_top || only_hovered) && let Some(i) = hovered {
                let (r, t) = &thumbs[i];
                p.rect_filled(r.translate(vec2(-4.0, 2.0)), 2.0, Color32::from_black_alpha(160));
                p.image(t.id(), *r, full_uv, Color32::WHITE);
                if on_top {
                    p.rect_stroke(*r, 0.0, Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Inside);
                }
            }
        }
        // Alert track markers + icons.
        if svc.settings.bool("ui3_timeline_alertTrackMarkers") {
            for a in &d.alerts {
                let x = x_of(a.time);
                if x >= canvas.left() && x < canvas.right() {
                    for tr in 0..d.colors.len() {
                        if a.tracks & (1 << tr) != 0 {
                            let y1 = canvas.top() + alert_space + tr as f32 * bar_h;
                            p.rect_filled(Rect::from_min_size(pos2(x, y1), vec2(1.0, bar_h)), 0.0, alert_col);
                        }
                    }
                }
            }
        }
        for a in &d.alerts {
            let x = x_of(a.time);
            if x >= canvas.left() - 6.0 && x < canvas.right() {
                let r = Rect::from_center_size(pos2(x, canvas.top() + alert_space / 2.0), vec2(11.0, 11.0));
                let (icon, col) = if a.is_flag {
                    (icons::FLAG, Color32::from_rgb(0xF4, 0x43, 0x36))
                } else if a.is_person {
                    (icons::MAN, Color32::from_rgb(0xFF, 0x5F, 0x00))
                } else if a.is_vehicle {
                    (icons::DIRECTIONS_CAR, Color32::from_rgb(0xFF, 0x5F, 0x00))
                } else if a.is_wildlife {
                    (icons::WILDLIFE, Color32::from_rgb(0xFF, 0x5F, 0x00))
                } else {
                    (icons::ALERT1, Color32::from_rgb(0xFF, 0x70, 0x43))
                };
                w::paint_icon(ui, icon, r, col);
            }
        }
        // Camera names on tracks (helps orientation for groups).
        if let Some(g) = target_cam.as_ref().and_then(|c| c.group.clone())
            && g.len() == d.colors.len()
            && bar_h >= 11.0
        {
            for (i, id) in g.iter().enumerate() {
                let name = cam_names.get(id).cloned().unwrap_or_else(|| id.clone());
                p.text(pos2(canvas.left() + 4.0, canvas.top() + alert_space + i as f32 * bar_h + bar_h / 2.0), egui::Align2::LEFT_CENTER, name, FontId::proportional((bar_h * 0.7).min(11.0)), Color32::from_white_alpha(150));
            }
        }
    }
    // Live keep-out gradient.
    if now > left && now - KEEP_OUT_MS <= tl.right() {
        let x = x_of(now - KEEP_OUT_MS);
        let w = (KEEP_OUT_MS as f64 / zf) as f32;
        let w = w.max(2.0);
        // UI3's color stops (ui3.js:10268).
        let stops = [
            (0.0, Color32::from_rgba_unmultiplied(123, 133, 180, 0)),
            (0.2, Color32::from_rgba_unmultiplied(123, 133, 180, 77)),
            (0.8, Color32::from_rgba_unmultiplied(70, 130, 255, 204)),
            (0.9, Color32::from_rgba_unmultiplied(100, 255, 255, 230)),
            (1.0, Color32::WHITE),
        ];
        let mut mesh = egui::Mesh::default();
        for (k, &(t, c)) in stops.iter().enumerate() {
            let sx = x + w * t;
            mesh.colored_vertex(pos2(sx, canvas.top()), c);
            mesh.colored_vertex(pos2(sx, canvas.bottom()), c);
            if k > 0 {
                let i = 2 * k as u32;
                mesh.add_triangle(i - 2, i, i + 1);
                mesh.add_triangle(i - 2, i + 1, i - 1);
            }
        }
        p.add(egui::Shape::mesh(mesh));
        if zf > 1200.0 {
            p.rect_filled(Rect::from_min_size(pos2(x + w - 1.0, canvas.top()), vec2(2.0, canvas.height())), 0.0, Color32::from_rgb(100, 255, 255));
        }
    }
    // Export range.
    if let Some((a, b)) = tl.export_range {
        let (xa, xb) = (x_of(a), x_of(b));
        p.rect_filled(Rect::from_min_max(pos2(xa.min(xb), canvas.top()), pos2(xa.max(xb), canvas.bottom())), 0.0, Color32::from_rgba_unmultiplied(255, 200, 50, 46));
        p.rect_filled(Rect::from_min_size(pos2(xa - 1.0, canvas.top()), vec2(2.0, canvas.height())), 0.0, Color32::from_rgb(80, 255, 80));
        p.rect_filled(Rect::from_min_size(pos2(xb - 1.0, canvas.top()), vec2(2.0, canvas.height())), 0.0, Color32::from_rgb(255, 80, 80));
    }
    // Center bar (.timelineCenterBar) and the selected time box above the legend (.timelineCurrentTime).
    let cx = canvas.center().x;
    p.rect_filled(Rect::from_min_max(pos2(cx - 1.0, legend.top()), pos2(cx + 1.0, canvas.bottom())), 0.0, th.timeline_center);
    if !tl.is_live || tl.dragging {
        let ct = tl.current_time();
        let txt = chrono::DateTime::from_timestamp_millis(ct + tz)
            .map(|d| {
                let d = d.naive_utc();
                format!("{}\n{}", d.format("%a %b %-d %Y"), if h24 { d.format("%H:%M:%S").to_string() } else { d.format("%-I:%M:%S %p").to_string() })
            })
            .unwrap_or_default();
        let tp = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("tl_curtime")));
        let g = tp.layout(txt, FontId::proportional(12.0), th.text_active, 200.0);
        let r = Rect::from_center_size(pos2(cx, legend.top() - 15.0), vec2(g.size().x.max(110.0) + 10.0, 30.0));
        tp.rect_filled(r.expand(2.0), 0.0, Color32::from_black_alpha(60));
        tp.rect_filled(r, 0.0, Color32::from_black_alpha(204));
        tp.rect_stroke(r, 0.0, Stroke::new(1.0, th.button), egui::StrokeKind::Inside);
        tp.galley(pos2(r.center().x - g.size().x / 2.0, r.center().y - g.size().y / 2.0), g, th.text_active);
    }
    // Hover time.
    if let Some(hx) = tl.hover_x
        && !tl.dragging
    {
        let ht = left + ((hx - canvas.left()) as f64 * zf) as i64;
        p.line_segment([pos2(hx, canvas.top()), pos2(hx, canvas.bottom())], Stroke::new(1.0, Color32::from_white_alpha(80)));
        let s = crate::views::video::fmt_utc(ht, tz);
        p.text(Pos2::new(hx + 4.0, canvas.bottom() - 4.0), egui::Align2::LEFT_BOTTOM, &s[..s.len().min(19)], FontId::proportional(11.0), Color32::WHITE);
    }
    timeline_context_menu(view, ui, svc, &resp);
    // Load failures are shown in the top-left of the canvas (`.timelineError`, ui3.js:9722).
    if let Some(e) = &view.timeline.error {
        let pos = canvas.min + vec2(4.0, 2.0);
        let font = FontId::proportional(12.0);
        p.text(pos + vec2(1.0, 1.0), egui::Align2::LEFT_TOP, e, font.clone(), Color32::BLACK);
        p.text(pos, egui::Align2::LEFT_TOP, e, font, Color32::from_rgb(255, 120, 120));
    }
}

/// Right-click menu of the timeline strip (`optionTimeline`, ui3.js:10905): dead-air skipping, starfield, export.
fn timeline_context_menu(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services, resp: &egui::Response) {
    let ctx = ui.ctx().clone();
    let mut reopen = false;
    let mut export = None; // (start_here, end_here) requests
    egui::Popup::context_menu(resp).frame(w::list_frame()).width(220.0).show(|ui| {
        ui.set_min_width(220.0);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        // Three-state "Skip dead-air": 0 = no preference, 1 = off, 2 = on.
        let cur = match svc.settings.str("ui3_playback_skipDeadAir").as_str() {
            "1" => 1,
            "2" => 2,
            _ => 0,
        };
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.label(egui::RichText::new("Skip dead-air").size(14.0));
            ui.add_space(6.0);
            for (v, n) in [(0, "Default"), (1, "Off"), (2, "On")] {
                if ui.selectable_label(cur == v, n).clicked() && cur != v {
                    svc.settings.set("ui3_playback_skipDeadAir", v.to_string());
                    reopen = true;
                }
            }
        });
        ui.add_space(4.0);
        let star = svc.settings.bool("ui3_timeline_starfield");
        let icon = if star { icons::CB_CHECKED } else { icons::CB_UNCHECKED };
        if w::list_item(ui, Some(icon), "Starfield Background", false, w::ItemKind::Normal)
            .on_hover_text("The starfield background adds a little visual flair outside the timeline boundaries and when zoomed out a long way.")
            .clicked()
        {
            svc.settings.set_bool("ui3_timeline_starfield", !star);
            ui.close();
        }
        w::list_separator(ui);
        // These need a timeline stream (not live video).
        let on_timeline = !view.timeline.is_live;
        ui.add_enabled_ui(on_timeline, |ui| {
            if w::list_item(ui, Some(icons::LAUNCH), "Convert/export", false, w::ItemKind::Normal).clicked() {
                export = Some((false, false));
                ui.close();
            }
            if w::list_item(ui, Some(icons::DOWNLOAD), "Set start frame", false, w::ItemKind::Normal).clicked() {
                export = Some((true, false));
                ui.close();
            }
            if w::list_item(ui, Some(icons::DOWNLOAD), "Set end frame", false, w::ItemKind::Normal).clicked() {
                export = Some((false, true));
                ui.close();
            }
        });
    });
    if reopen && !view.timeline.is_live {
        let client = view.client.clone();
        view.timeline.user_did_set_time(&ctx, &client, &mut view.player, svc);
    }
    if let Some((start_here, end_here)) = export {
        let tl = &mut view.timeline;
        if start_here {
            tl.set_export_start();
        } else if end_here {
            tl.set_export_end();
        } else if tl.export_range.is_none_or(|(a, b)| b <= a) {
            // UI3 default: a 30 s range ending at the current position (ui3.js:12887).
            let t = tl.current_time();
            tl.export_range = Some(((t - 30_000).max(1), t));
        }
        if let Some((a, b)) = view.timeline.export_range
            && b > a
            && !start_here
            && !end_here
        {
            if !view.timeline.paused {
                let client = view.client.clone();
                view.timeline.toggle_pause(&ctx, &client, &mut view.player, svc);
            }
            view.dialogs.open(crate::views::dialogs::Dialog::TimelineExport { camera: view.timeline.target.clone(), start: a, end: b });
        }
    }
}
