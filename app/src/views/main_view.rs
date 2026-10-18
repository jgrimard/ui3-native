//! The logged-in UI: top bar, side bar, video area, clip list, timeline.

use crate::app::Services;
use crate::gpu::Gpu;
use crate::player::{OpenParams, PlayMode, Player};
use crate::runtime::{self, Pending};
use crate::settings::Settings;
use crate::views::video::{VideoArea, VideoInput};
use anyhow::Result;
use bi_media::decoder::HwAccel;
use bi_proto::api::{CameraInfo, StatusData};
use bi_proto::profile::SupportedCodecs;
use bi_proto::urls::{self, AudioRequest};
use bi_proto::{BiClient, Session};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Live,
    Clips,
    Timeline,
}

impl Tab {
    pub fn from_setting(s: &str) -> Tab {
        match s {
            "clips" | "alerts" => Tab::Clips,
            "timeline" => Tab::Timeline,
            _ => Tab::Live,
        }
    }
}

/// What the video area is showing.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    None,
    Live { id: String },
    Clip { key: String },
    Timeline { id: String },
}

const UNREACHABLE_TOAST: &str = "server-unreachable";
/// Consecutive failed polls (camlist and status count separately) before the toast appears and polling slows.
const UNREACHABLE_TOAST_AFTER: u32 = 3;

pub struct MainView {
    pub client: BiClient,
    pub session: Session,
    pub cameras: Vec<CameraInfo>,
    pub status: Option<StatusData>,
    pub tab: Tab,
    /// The selected home group (UI3 "Current Group").
    pub home_group: String,
    pub content: Content,
    pub player: Player,
    pub video: VideoArea,
    pub clips: crate::views::clips::ClipsPanel,
    pub playback: crate::views::playback::Playback,
    pub timeline: crate::views::timeline::TimelinePanel,
    pub ptz: crate::views::ptz::PtzPanel,
    pub dialogs: crate::views::dialogs::Dialogs,
    pub events: crate::events::Events,
    pub mqtt: Option<crate::mqtt::Mqtt>,
    mqtt_key: String,
    /// Live streams stopped because the window is minimized (`ui3_pause_when_hidden`).
    paused_hidden: bool,
    keep_awake: bool,
    last_input: Instant,
    /// Single click waiting to see whether it becomes a double click (`(time, camera, action)`).
    pending_click: Option<(Instant, Option<String>, String)>,
    /// Idle timeout reached (`SessionTimeout`, ui3_timeout): streaming stopped until the user clicks.
    pub idle_timed_out: bool,
    login_at: Instant,
    pub mini_mode: bool,
    pub grid: crate::views::grid::Grid,
    pub maximized: bool,
    pub fullscreen: bool,
    pub controls: crate::views::controls::Controls,
    camlist_job: Option<Pending<Result<Vec<CameraInfo>>>>,
    status_job: Option<Pending<Result<StatusData>>>,
    last_camlist: Instant,
    last_status: Instant,
    /// Consecutive camlist/status polls that could not reach the server.
    unreachable_streak: u32,
    pub logout_requested: bool,
    /// Live stream reconnect bookkeeping.
    reconnect_at: Option<Instant>,
    pub last_live_open: Option<Instant>,
}

impl MainView {
    pub fn new(client: BiClient, session: Session, cameras: Vec<CameraInfo>, ctx: &egui::Context, settings: &mut Settings) -> Self {
        // UI3 titles the page "<system name> UI3" (SysNameToAppName).
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{} UI3", session.login.system_name)));
        let mut home = settings.str("ui3_defaultCameraGroupId");
        if !cameras.iter().any(|c| c.id.eq_ignore_ascii_case(&home)) {
            home = cameras.iter().find(|c| c.is_group()).or(cameras.first()).map(|c| c.id.clone()).unwrap_or_default();
        }
        let mut v = MainView {
            client,
            session,
            cameras,
            status: None,
            tab: Tab::from_setting(&settings.str("ui3_defaultTab")),
            home_group: home.clone(),
            content: Content::None,
            player: Player::new(),
            video: VideoArea::default(),
            clips: crate::views::clips::ClipsPanel::new(settings),
            playback: crate::views::playback::Playback::new(settings),
            timeline: crate::views::timeline::TimelinePanel::new(settings),
            ptz: Default::default(),
            dialogs: Default::default(),
            events: Default::default(),
            mqtt: None,
            mqtt_key: String::new(),
            paused_hidden: false,
            keep_awake: false,
            last_input: Instant::now(),
            pending_click: None,
            idle_timed_out: false,
            login_at: Instant::now(),
            mini_mode: false,
            grid: Default::default(),
            maximized: settings.bool("ui3_is_maximized"),
            fullscreen: false,
            controls: Default::default(),
            camlist_job: None,
            status_job: None,
            last_camlist: Instant::now(),
            last_status: Instant::now() - Duration::from_secs(60),
            unreachable_streak: 0,
            logout_requested: false,
            reconnect_at: None,
            last_live_open: None,
        };
        v.timeline.set_camera_sizes(&v.cameras);
        if !home.is_empty() {
            v.load_live(ctx, settings, None, &home);
        }
        v
    }

    pub fn camera(&self, id: &str) -> Option<&CameraInfo> {
        self.cameras.iter().find(|c| c.id.eq_ignore_ascii_case(id))
    }

    pub fn groups(&self) -> impl Iterator<Item = &CameraInfo> {
        self.cameras.iter().filter(|c| c.is_group() || c.is_cycle())
    }

    pub fn audio_request(&self, settings: &Settings, audio: &bi_media::audio::AudioOutput) -> AudioRequest {
        if !self.session.login.permissions().audio {
            AudioRequest::None
        } else {
            pick_audio(settings, audio)
        }
    }

    pub fn hw_accel(settings: &Settings) -> HwAccel {
        match settings.native.hw_decode.as_str() {
            "Software" => HwAccel::Software,
            "D3D11VA" => HwAccel::D3D11Va,
            _ => HwAccel::Auto,
        }
    }

    /// Native size of a camera/group as the server reports it.
    pub fn native_size(&self, id: &str) -> (i32, i32) {
        self.camera(id).map(|c| (c.width.max(16) as i32, c.height.max(16) as i32)).unwrap_or((1920, 1080))
    }

    pub fn profile_args(&self, settings: &Settings, id: &str) -> String {
        let p = settings.current_profile();
        let size = p.size_to_request(self.native_size(id), if self.camera(id).is_some_and(|c| c.is_group()) { Some(320) } else { None });
        let max = settings.f64("ui3_streamingProfileBitRateMax") as i32;
        p.url_args(size, SupportedCodecs::default(), Some(max))
    }

    /// `LoadLiveCamera`.
    pub fn load_live(&mut self, ctx: &egui::Context, settings: &mut Settings, audio: Option<&bi_media::audio::AudioOutput>, id: &str) {
        // Client-side grid: the group is rendered from per-camera streams (opened in `sync_grid`).
        if settings.native.client_side_grid && self.camera(id).is_some_and(|c| c.is_group()) {
            self.player.stop();
            self.content = Content::Live { id: id.to_string() };
            self.events.reset_status();
            self.reconnect_at = None;
            self.last_live_open = Some(Instant::now());
            return;
        }
        let audio_req = audio.map(|a| self.audio_request(settings, a)).unwrap_or(AudioRequest::None);
        let group_args = crate::views::video::group_args(settings, id).args();
        let path = urls::live_video(id, audio_req, &self.profile_args(settings, id), &group_args);
        self.player.open(
            &self.client,
            OpenParams { path, hold_placeholder: false, mode: PlayMode::Live, timestamp_scale: 1.0, audio: audio_req != AudioRequest::None, accel: Self::hw_accel(settings) },
            audio.cloned(),
            ctx,
        );
        self.content = Content::Live { id: id.to_string() };
        self.events.reset_status();
        self.video.reset_zoom();
        self.reconnect_at = None;
        self.last_live_open = Some(Instant::now());
        self.ptz.camera_changed(id);
    }

    /// Shows `id` in the current mode: on the Timeline tab the timeline is retargeted (keeping its time and
    /// play/pause state); elsewhere it opens live video.
    pub fn select_source(&mut self, ctx: &egui::Context, svc: &mut Services, id: &str) {
        if self.tab != Tab::Timeline {
            return self.load_live(ctx, svc.settings, Some(svc.audio), id);
        }
        self.timeline.set_target(ctx, &self.client, &mut self.player, svc, id);
        let id = id.to_string();
        self.content = if self.timeline.is_live { Content::Live { id } } else { Content::Timeline { id } };
        self.video.reset_zoom();
    }

    pub fn reload_current(&mut self, ctx: &egui::Context, svc: &mut Services) {
        match self.content.clone() {
            Content::Live { id } => self.load_live(ctx, svc.settings, Some(svc.audio), &id),
            Content::Clip { .. } => self.playback.reopen(ctx, &self.client, &mut self.player, svc),
            Content::Timeline { .. } => self.timeline.reopen(ctx, &self.client, &mut self.player, svc),
            Content::None => {}
        }
    }

    pub fn current_live_id(&self) -> Option<&str> {
        match &self.content {
            Content::Live { id } => Some(id),
            _ => None,
        }
    }

    /// Camera/group shown in the video area, live or on the timeline.
    pub fn current_source_id(&self) -> Option<&str> {
        match &self.content {
            Content::Live { id } | Content::Timeline { id } => Some(id),
            _ => None,
        }
    }

    /// Camera to use for PTZ / camera actions: the live single camera, if any.
    pub fn current_single_camera(&self) -> Option<&CameraInfo> {
        let id = match &self.content {
            Content::Live { id } => id.clone(),
            Content::Clip { .. } => self.playback.camera_id().unwrap_or_default(),
            _ => return None,
        };
        self.camera(&id).filter(|c| !c.is_group() && !c.is_cycle())
    }

    /// Clears the new-alert counters on the server (camlist reset=2) and reloads the camera list (UI3 resetNewAlertCounters).
    pub fn reset_new_alert_counters(&mut self, ctx: &egui::Context) {
        self.last_camlist = Instant::now();
        let c = self.client.clone();
        self.camlist_job = Some(runtime::spawn(ctx, async move { c.camlist(true).await }));
    }

    /// `HandleChangesInStatus` (ui3.js:16428): profile changes and Blue Iris restarts are announced.
    fn notice_status_changes(&self, old: &bi_proto::api::StatusData, new: &bi_proto::api::StatusData, svc: &mut Services) {
        if old.profile != new.profile {
            let name = |n: i64| usize::try_from(n).ok().and_then(|i| self.session.login.profiles.get(i)).cloned().unwrap_or_else(|| n.to_string());
            svc.toasts.info(format!("Your profile has changed from \"{}\" to \"{}\"", name(old.profile), name(new.profile)));
        }
        // Uptime is days:hours:minutes:seconds; a server that keeps running only reports larger values.
        let secs = |u: &str| -> Option<i64> {
            let parts: Vec<&str> = u.split(':').collect();
            if !(2..=4).contains(&parts.len()) {
                return None;
            }
            parts.iter().rev().zip([1i64, 60, 3600, 86400]).try_fold(0i64, |t, (p, m)| p.trim().parse::<i64>().ok().filter(|n| *n >= 0).map(|n| t + n * m))
        };
        if let (Some(o), Some(n)) = (secs(&old.uptime), secs(&new.uptime))
            && n < o
        {
            svc.toasts.info("Blue Iris has restarted.");
        }
    }

    fn poll_reached(&mut self, svc: &mut Services) {
        self.unreachable_streak = 0;
        if svc.toasts.clear_sticky(UNREACHABLE_TOAST) {
            svc.toasts.success("Reconnected to Blue Iris.");
        }
    }

    /// One failed poll is usually a dropped keep-alive connection, so the toast waits for a few in a row.
    fn poll_unreachable(&mut self, e: &anyhow::Error, svc: &mut Services) {
        self.unreachable_streak += 1;
        tracing::debug!("poll could not reach server ({}): {e:#}", self.unreachable_streak);
        if self.unreachable_streak >= UNREACHABLE_TOAST_AFTER {
            let host = self.client.base_url().trim_start_matches("http://").trim_start_matches("https://").trim_end_matches('/');
            let msg = format!("Can't reach Blue Iris at {host}: {}
Retrying every 15 seconds.", e.root_cause());
            svc.toasts.set_sticky(UNREACHABLE_TOAST, crate::toasts::Level::Error, msg);
        }
    }

    fn poll_background(&mut self, ctx: &egui::Context, svc: &mut Services) {
        if let Some(j) = &mut self.camlist_job
            && let Some(r) = j.poll()
        {
            self.camlist_job = None;
            match r {
                Ok(c) if !c.is_empty() => {
                    self.poll_reached(svc);
                    self.cameras = c;
                    self.on_camlist_updated(ctx, svc);
                }
                Ok(_) => {
                    self.poll_reached(svc);
                    svc.toasts.error("Camera list is empty!");
                }
                Err(e) if bi_proto::client::is_unreachable(&e) => self.poll_unreachable(&e, svc),
                Err(e) => svc.toasts.warn(format!("Camera list update failed: {e:#}")),
            }
        }
        if let Some(j) = &mut self.status_job
            && let Some(r) = j.poll()
        {
            self.status_job = None;
            match r {
                Ok(s) => {
                    self.poll_reached(svc);
                    if let Some(old) = &self.status {
                        self.notice_status_changes(old, &s, svc);
                    }
                    self.status = Some(s);
                }
                Err(e) if bi_proto::client::is_unreachable(&e) => self.poll_unreachable(&e, svc),
                Err(e) => svc.toasts.warn(format!("Status update failed: {e:#}")),
            }
        }
        // Poll less often while the server is unreachable.
        let interval = Duration::from_secs(if self.unreachable_streak >= UNREACHABLE_TOAST_AFTER { 15 } else { 5 });
        if self.camlist_job.is_none() && self.last_camlist.elapsed() > interval {
            self.last_camlist = Instant::now();
            let c = self.client.clone();
            self.camlist_job = Some(runtime::spawn(ctx, async move { c.camlist(false).await }));
        }
        if self.status_job.is_none() && self.last_status.elapsed() > interval {
            self.last_status = Instant::now();
            let c = self.client.clone();
            self.status_job = Some(runtime::spawn(ctx, async move { c.status(None).await }));
        }
        ctx.request_repaint_after(Duration::from_secs(1));

        // Live stream ended unexpectedly: reconnect (UI3 reconnects after a short delay).
        if let Content::Live { id } = &self.content
            && let Some(reason) = self.player.ended()
        {
            match self.reconnect_at {
                None => {
                    tracing::warn!("live stream ended: {reason:?}; reconnecting");
                    let delay = if matches!(reason, bi_proto::stream::EndReason::SessionLost) { 500 } else { 2000 };
                    self.reconnect_at = Some(Instant::now() + Duration::from_millis(delay));
                }
                Some(t) if Instant::now() >= t => {
                    let id = id.clone();
                    let c = self.client.clone();
                    if matches!(reason, bi_proto::stream::EndReason::SessionLost) {
                        runtime::spawn_detached(async move {
                            let _ = c.login().await;
                        });
                    }
                    self.load_live(ctx, svc.settings, Some(svc.audio), &id);
                }
                _ => ctx.request_repaint_after(Duration::from_millis(200)),
            }
        }
    }

    /// Cameras visible in the current live view (group members or the single camera).
    fn visible_cameras(&self) -> Vec<&CameraInfo> {
        let Some(id) = self.current_live_id() else { return vec![] };
        match self.camera(id) {
            Some(c) if c.is_group() => c.group.as_ref().unwrap().iter().filter_map(|m| self.camera(m)).collect(),
            Some(c) => vec![c],
            None => vec![],
        }
    }

    fn on_camlist_updated(&mut self, ctx: &egui::Context, svc: &mut Services) {
        self.timeline.set_camera_sizes(&self.cameras);
        let client = self.client.clone();
        let vis: Vec<CameraInfo> = self.visible_cameras().into_iter().cloned().collect();
        let refs: Vec<&CameraInfo> = vis.iter().collect();
        let grid_active = self.grid.is_active();
        self.events.on_camlist(ctx, &client, svc.settings, svc.audio, &refs, grid_active);
        // Auto-maximize (`UpdatedCurrentCameraData`, ui3.js:18398).
        let Some(cur) = self.current_live_id().map(str::to_string) else { return };
        if self.tab != Tab::Live {
            return;
        }
        let was_auto = self.events.pt_current.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(&cur));
        if svc.settings.bool("ui3_prioritizeTriggered") {
            if let Some(p) = self.events.pt_current.clone()
                && !self.camera(&p).is_some_and(|c| crate::events::cam_is_considered_triggered(c, svc.settings))
            {
                self.events.pt_current = None;
            }
            if self.events.pt_current.is_none() {
                let members = self.camera(&self.home_group).and_then(|g| g.group.clone()).unwrap_or_default();
                let hit = members.iter().find(|m| self.camera(m).is_some_and(|c| crate::events::cam_is_considered_triggered(c, svc.settings))).cloned();
                if let Some(id) = hit {
                    self.events.pt_current = Some(id.clone());
                    if !id.eq_ignore_ascii_case(&cur) {
                        self.load_live(ctx, svc.settings, Some(svc.audio), &id);
                    }
                }
            }
        } else {
            self.events.pt_current = None;
        }
        if was_auto && self.events.pt_current.is_none() {
            let home = self.home_group.clone();
            self.load_live(ctx, svc.settings, Some(svc.audio), &home);
        }
    }

    /// MQTT commands, window-visibility pausing, keep-awake, event sounds from status blocks.
    fn housekeeping(&mut self, ctx: &egui::Context, svc: &mut Services) {
        // MQTT client lifecycle follows settings.
        let key = crate::mqtt::Mqtt::config_key(svc.settings);
        if key != self.mqtt_key {
            self.mqtt_key = key;
            self.mqtt = None;
            match crate::mqtt::Mqtt::start(svc.settings, ctx) {
                Ok(m) => self.mqtt = m,
                Err(e) => svc.toasts.error(format!("MQTT: {e:#}")),
            }
        }
        while let Some(cmd) = self.mqtt.as_ref().and_then(|m| m.try_recv()) {
            use crate::mqtt::MqttCommand::*;
            match cmd {
                Video(id) => {
                    if self.camera(&id).is_some() && self.current_source_id().is_none_or(|c| !c.eq_ignore_ascii_case(&id)) {
                        self.select_source(ctx, svc, &id);
                    }
                }
                Maximize(m) => {
                    self.maximized = m;
                    svc.settings.set_bool("ui3_is_maximized", m);
                }
                Volume(v) => {
                    svc.audio.set_volume(v);
                    svc.audio.set_muted(v == 0.0);
                    svc.settings.set("ui3_audioVolume", serde_json::json!(v));
                    svc.settings.set_bool("ui3_audioMute", v == 0.0);
                }
                PlayAudio(f) => {
                    let c = self.client.clone();
                    self.events.play_file(ctx, &c, svc.settings, svc.audio, &f);
                }
                Speak(t) => self.events.speak(svc.settings, &t),
                Toast { kind, msg, timeout_ms } => {
                    let lvl = match kind.as_str() {
                        "success" => crate::toasts::Level::Success,
                        "warning" => crate::toasts::Level::Warning,
                        "error" => crate::toasts::Level::Error,
                        _ => crate::toasts::Level::Info,
                    };
                    svc.toasts.add(lvl, msg, timeout_ms.unwrap_or(10_000));
                }
            }
        }
        let live_id = self.current_live_id().map(str::to_string);
        if let Some(m) = &mut self.mqtt {
            if let Some(id) = live_id {
                m.publish_state("vid", &id);
            }
            m.publish_state("maximize", if self.maximized { "1" } else { "0" });
            m.publish_state("volume", &format!("{}", (svc.audio.volume() * 100.0).round()));
        }
        // Event sounds from status blocks; status tones on disconnect/reconnect.
        self.events.poll(svc.audio);
        if let Some(st) = self.player.status() {
            let c = self.client.clone();
            self.events.on_status(ctx, &c, svc.settings, svc.audio, &st);
        }
        let connected = !matches!(self.content, Content::Live { .. }) || self.player.ended().is_none();
        self.events.connection_state(svc.settings, svc.audio, connected);
        // A silent re-login inside the client replaces its session; pick up the new login data (permissions etc.).
        if let Some(s) = self.client.session()
            && s.id != self.session.id
        {
            self.session = s;
        }
        // Idle timeout (`ui3_timeout` minutes, 0 = disabled).
        if ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving()) {
            self.last_input = Instant::now();
        }
        // UI3 clamps a configured timeout below 30 s up to 30 s (ui3.js:36012).
        let timeout_min = svc.settings.f64("ui3_timeout");
        let timeout_min = if timeout_min > 0.0 { timeout_min.max(0.5) } else { timeout_min };
        if !self.idle_timed_out && timeout_min > 0.0 && self.last_input.elapsed() > Duration::from_secs_f64(timeout_min * 60.0) {
            self.idle_timed_out = true;
            self.player.stop();
            svc.audio.clear();
        }
        // Server-imposed time limits for limited users (`ServerTimeLimiter`, ui3.js:24941).
        let ex = &self.session.login.extra;
        if ex.get("timelimits").is_some_and(bi_proto::lenient::value_to_bool) {
            let n = |k: &str| ex.get(k).and_then(bi_proto::lenient::value_to_f64).unwrap_or(0.0);
            let elapsed = self.login_at.elapsed().as_secs_f64();
            if (n("sessionlimit") > 0.0 && elapsed >= n("sessionlimit")) || (n("daylimit") > 0.0 && n("dayused") + elapsed >= n("daylimit")) {
                svc.toasts.warn("Your Blue Iris session time limit has been reached.");
                self.logout_requested = true;
            }
            if n("streamlimit") > 0.0
                && let Some(t) = self.last_live_open
                && t.elapsed().as_secs_f64() >= n("streamlimit")
                && self.player.is_streaming()
            {
                self.player.stop();
                svc.toasts.info("Video has stopped because your user account has a stream time limit. Select a camera to resume.");
            }
        }
        // Pause live video while minimized (`ui3_pause_when_hidden`).
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        if svc.settings.bool("ui3_pause_when_hidden") {
            if minimized && !self.paused_hidden && matches!(self.content, Content::Live { .. }) {
                self.paused_hidden = true;
                self.player.stop();
                svc.audio.clear();
            } else if !minimized && self.paused_hidden {
                self.paused_hidden = false;
                self.reload_current(ctx, svc);
            }
        }
        // Keep the display awake while video plays (`ui3_preventSleep`).
        let want_awake = svc.settings.bool("ui3_preventSleep") && self.player.is_streaming();
        if want_awake != self.keep_awake {
            self.keep_awake = want_awake;
            use windows::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
            unsafe {
                SetThreadExecutionState(if want_awake { ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
            }
        }
    }

    /// Opens/closes the client-side grid to match the current content. Never on the Timeline tab: the timeline
    /// always plays through `player` (even at the live edge), so a grid there would be a second, duplicate stream.
    fn sync_grid(&mut self, ctx: &egui::Context, svc: &mut Services) {
        let want = match &self.content {
            Content::Live { id } if self.tab != Tab::Timeline && svc.settings.native.client_side_grid && self.camera(id).is_some_and(|c| c.is_group()) && !self.idle_timed_out && !self.paused_hidden => Some(id.clone()),
            _ => None,
        };
        match (want, self.grid.group.clone()) {
            (Some(w), Some(g)) if w.eq_ignore_ascii_case(&g) => {}
            (Some(w), _) => {
                let group = self.camera(&w).cloned().unwrap();
                let cams = self.cameras.clone();
                let client = self.client.clone();
                self.grid.open(ctx, &client, &group, &cams, svc);
            }
            (None, Some(_)) => self.grid.close(svc.gpu),
            (None, None) => {}
        }
    }

    /// Returns true when the user logged out.
    pub fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame, svc: &mut Services) -> bool {
        let ctx = ui.ctx().clone();
        crate::views::style::sync_theme(&ctx, &svc.settings.str("ui3_color_theme"), &self.session.login.version);
        self.poll_background(&ctx, svc);
        self.housekeeping(&ctx, svc);
        crate::views::hotkeys::handle(self, &ctx, svc);
        // PTZ queue runs even when the PTZ panel is hidden (hotkeys, gamepad).
        let client = self.client.clone();
        // Like UI3, stop a moving PTZ motor when the window loses focus or is minimized.
        if ctx.input(|i| i.viewport().focused == Some(false) || i.viewport().minimized == Some(true)) {
            self.ptz.release_all_inputs();
        }
        let ptz_ok = matches!(self.content, Content::Live { .. }) && self.session.login.permissions().ptz && self.current_single_camera().is_some_and(|c| c.ptz);
        self.ptz.set_available(ptz_ok);
        self.ptz.poll(&ctx, &client, svc);

        if !self.maximized {
            crate::views::topbar::show(self, ui, svc);
            crate::views::sidebar::show(self, ui, svc);
        }
        if self.tab == Tab::Timeline {
            crate::views::timeline::show_panel(self, ui, svc);
        }
        self.sync_grid(&ctx, svc);
        // Upload the newest decoded frame before drawing.
        if let Some(f) = self.player.take_new_frame() {
            self.video.set_frame(svc.gpu, &f);
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(crate::views::style::VIDEO_BG)).show(ui, |ui| {
            let area = ui.available_rect_before_wrap();
            if self.grid.is_active() {
                self.grid.update(svc.gpu);
                let cams = self.cameras.clone();
                let (click, dbl, ctxm) = self.grid.show(ui, &cams, svc.settings.native.show_stats_overlay);
                let input = crate::views::video::VideoInput { clicked: click.map(Some), double_clicked: dbl.map(Some), context_menu_camera: ctxm.map(Some) };
                self.handle_video_input(&ctx, svc, input);
                ctx.request_repaint_after(Duration::from_millis(500));
            } else {
                let input = crate::views::video::show(self, ui, svc);
                self.handle_video_input(&ctx, svc, input);
            }
            crate::views::controls::show(self, ui, svc, area);
        });
        crate::views::dialogs::show(self, &ctx, svc);
        crate::views::clips::memo_and_delete_dialogs(self, &ctx, svc);
        if self.idle_timed_out {
            let mut resume = false;
            egui::Modal::new(egui::Id::new("idle")).show(&ctx, |ui| {
                ui.heading("Paused due to inactivity");
                ui.label(format!("Video stopped after {} minutes without input (Idle Timeout setting).", svc.settings.f64("ui3_timeout")));
                if ui.button("Resume").clicked() {
                    resume = true;
                }
            });
            if resume {
                self.idle_timed_out = false;
                self.last_input = Instant::now();
                self.reload_current(&ctx, svc);
            }
        }
        if self.logout_requested {
            let c = self.client.clone();
            runtime::spawn_detached(async move {
                let _ = c.logout().await;
            });
            return true;
        }
        let _ = frame;
        false
    }

    fn handle_video_input(&mut self, ctx: &egui::Context, svc: &mut Services, input: VideoInput) {
        let click_action = |s: &Settings, tab: Tab, double: bool| -> String {
            let key = match (tab, double) {
                (Tab::Live, false) => "ui3_singleClick_live",
                (Tab::Live, true) => "ui3_doubleClick_live",
                (Tab::Clips, false) => "ui3_singleClick_clips",
                (Tab::Clips, true) => "ui3_doubleClick_clips",
                (Tab::Timeline, false) => "ui3_singleClick_timeline",
                (Tab::Timeline, true) => "ui3_doubleClick_timeline",
            };
            s.str(key)
        };
        // UI3 (and BI) treat the live edge of the Timeline tab like the timeline itself.
        let tab_for_click = match &self.content {
            Content::Clip { .. } => Tab::Clips,
            Content::Timeline { .. } => Tab::Timeline,
            Content::Live { .. } if self.tab == Tab::Timeline => Tab::Timeline,
            _ => Tab::Live,
        };
        // Single clicks wait for the double-click interval when a double-click action is configured (ui3.js:18365).
        let delay = Duration::from_secs_f32(ctx.options(|o| o.input_options.max_double_click_delay) as f32);
        if let Some(cam) = input.double_clicked {
            self.pending_click = None;
            let a = click_action(svc.settings, tab_for_click, true);
            self.click_act(ctx, svc, a, cam);
        } else if let Some(cam) = input.clicked {
            let a = click_action(svc.settings, tab_for_click, false);
            if click_action(svc.settings, tab_for_click, true) == "None" {
                self.click_act(ctx, svc, a, cam);
            } else {
                self.pending_click = Some((Instant::now(), cam, a));
                ctx.request_repaint_after(delay);
            }
        }
        if let Some((t, _, _)) = &self.pending_click {
            if t.elapsed() >= delay {
                let (_, cam, a) = self.pending_click.take().unwrap();
                self.click_act(ctx, svc, a, cam);
            } else {
                ctx.request_repaint_after(delay);
            }
        }
        if let Some(ctx_cam) = input.context_menu_camera {
            self.dialogs.context_camera = Some(ctx_cam);
        }
    }

    fn click_act(&mut self, ctx: &egui::Context, svc: &mut Services, action: String, cam: Option<String>) {
        match action.as_str() {
            "Maximize/Unmaximize Camera" => self.toggle_maximize_camera(ctx, svc, cam),
            "Play/Pause" => {
                if matches!(self.content, Content::Clip { .. }) {
                    self.playback.toggle_pause(ctx, &self.client, &mut self.player, svc);
                } else if self.tab == Tab::Timeline {
                    self.timeline.toggle_pause(ctx, &self.client, &mut self.player, svc);
                }
            }
            "Fullscreen" => {
                let on = !self.fullscreen;
                crate::views::controls::set_fullscreen(self, ctx, svc, on);
            }
            _ => {}
        }
    }

    /// `ImgClick_Camera`: clicking a camera in a group maximizes it; clicking the maximized camera returns to the group.
    pub fn toggle_maximize_camera(&mut self, ctx: &egui::Context, svc: &mut Services, cam: Option<String>) {
        let Some(id) = self.current_source_id().map(str::to_string) else { return };
        let is_group = self.camera(&id).is_some_and(|c| c.is_group());
        if is_group {
            if let Some(c) = cam
                && !c.eq_ignore_ascii_case(&id)
                && self.camera(&c).is_some_and(|c| !c.is_cycle())
            {
                self.select_source(ctx, svc, &c);
            }
        } else {
            let home = self.home_group.clone();
            if !home.eq_ignore_ascii_case(&id) {
                self.select_source(ctx, svc, &home);
            }
        }
    }

    pub fn set_tab(&mut self, ctx: &egui::Context, svc: &mut Services, tab: Tab) {
        if self.tab == tab {
            return;
        }
        // The timeline's stream (even at its live edge) is its own `player` stream, never the client-side grid,
        // so leaving the Timeline tab always reopens live video the normal way.
        let from_timeline = self.tab == Tab::Timeline;
        self.tab = tab;
        match tab {
            Tab::Live => {
                if from_timeline {
                    let id = self.current_source_id().map(str::to_string).unwrap_or_else(|| self.home_group.clone());
                    self.load_live(ctx, svc.settings, Some(svc.audio), &id);
                } else if !matches!(self.content, Content::Live { .. }) {
                    let home = self.home_group.clone();
                    self.playback.close();
                    self.load_live(ctx, svc.settings, Some(svc.audio), &home);
                }
            }
            Tab::Clips => {
                if from_timeline {
                    let home = self.home_group.clone();
                    self.load_live(ctx, svc.settings, Some(svc.audio), &home);
                }
                self.clips.ensure_loaded(ctx, &self.client, svc.settings, &self.home_group)
            }
            Tab::Timeline => {
                self.playback.close();
                let target = match &self.content {
                    Content::Live { id } => id.clone(),
                    _ => self.home_group.clone(),
                };
                self.timeline.target = target.clone();
                self.timeline.activate(ctx, &self.client, &mut self.player, svc, &target);
                self.content = Content::Live { id: target };
            }
        }
    }

    pub fn force_status_refresh(&mut self) {
        self.last_status = Instant::now() - Duration::from_secs(60);
        self.status_job = None;
    }

    pub fn shutdown(&mut self, gpu: &Gpu) {
        let client = self.client.clone();
        self.ptz.stop_motor_blocking(&client);
        self.player.clear();
        self.grid.close(gpu);
        self.video.release(gpu);
        self.clips.release(gpu);
    }
}

/// `&audio=` for a new stream: off when muted / volume 0 (UI3 `pcmPlayer.AudioEnabled`), else the configured codec.
pub fn pick_audio(settings: &Settings, audio: &bi_media::audio::AudioOutput) -> AudioRequest {
    if audio.muted() || audio.volume() <= 0.0 {
        AudioRequest::None
    } else if settings.str("ui3_audio_codec") == "FLAC" {
        AudioRequest::Flac
    } else {
        AudioRequest::MuLaw
    }
}
