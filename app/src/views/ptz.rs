//! PTZ controls. Port of `PtzButtons` (ui3.js:6886: pad, zoom/focus, presets with thumbnails, IR / brightness /
//! contrast), the serial PTZ action queue (UI3 allows one PTZ request at a time; stop commands retry until they
//! succeed), `RelativePTZ` (3D positioning) and `GamepadPtzController` (8856, via gilrs).

use crate::app::Services;
use crate::runtime::{self, Pending};
use crate::views::main_view::MainView;
use anyhow::Result;
use bi_proto::BiClient;
use bi_proto::api::{PtzInfo, ptz};
use crate::icons;
use crate::views::{style, w};
use egui::vec2;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum PtzAction {
    Button(i32),
    Hold { button: i32, start: bool },
    SetPreset { preset: i32, desc: String },
    Relative { x: f64, y: f64, z: f64 },
}

/// Held PTZ inputs from one source (pad, hotkeys, gamepad). Sources are merged like UI3's `mergedState`.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
struct PtzState {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    zin: bool,
    zout: bool,
    fnear: bool,
    ffar: bool,
}

impl PtzState {
    fn from_code(code: i32) -> Self {
        let mut s = Self::default();
        match code {
            ptz::UP => s.up = true,
            ptz::DOWN => s.down = true,
            ptz::LEFT => s.left = true,
            ptz::RIGHT => s.right = true,
            ptz::UP_LEFT => (s.up, s.left) = (true, true),
            ptz::UP_RIGHT => (s.up, s.right) = (true, true),
            ptz::DOWN_LEFT => (s.down, s.left) = (true, true),
            ptz::DOWN_RIGHT => (s.down, s.right) = (true, true),
            ptz::ZOOM_IN => s.zin = true,
            ptz::ZOOM_OUT => s.zout = true,
            ptz::FOCUS_NEAR => s.fnear = true,
            ptz::FOCUS_FAR => s.ffar = true,
            _ => {}
        }
        s
    }

    fn merge(self, o: Self) -> Self {
        Self { up: self.up | o.up, down: self.down | o.down, left: self.left | o.left, right: self.right | o.right, zin: self.zin | o.zin, zout: self.zout | o.zout, fnear: self.fnear | o.fnear, ffar: self.ffar | o.ffar }
    }

    /// True if the pad button for `code` should be drawn active.
    fn has(&self, code: i32) -> bool {
        let w = Self::from_code(code);
        w != Self::default() && self.merge(w) == *self
    }

    /// The single command to run, by UI3's priority (`ptzButtonId`, ui3.js:7754).
    fn command(&self) -> Option<i32> {
        Some(if self.zout {
            ptz::ZOOM_OUT
        } else if self.zin {
            ptz::ZOOM_IN
        } else if self.ffar {
            ptz::FOCUS_FAR
        } else if self.fnear {
            ptz::FOCUS_NEAR
        } else if self.up {
            if self.left {
                ptz::UP_LEFT
            } else if self.right {
                ptz::UP_RIGHT
            } else {
                ptz::UP
            }
        } else if self.down {
            if self.left {
                ptz::DOWN_LEFT
            } else if self.right {
                ptz::DOWN_RIGHT
            } else {
                ptz::DOWN
            }
        } else if self.left {
            ptz::LEFT
        } else if self.right {
            ptz::RIGHT
        } else {
            return None;
        })
    }
}

/// UI3 retries a failed hold start/stop every 100 ms until it succeeds (`PTZ_unsafe_async_guarantee`).
const RETRY_DELAY: Duration = Duration::from_millis(100);

#[derive(Default)]
pub struct PtzPanel {
    camera: String,
    pub info: Option<PtzInfo>,
    info_job: Option<Pending<Result<PtzInfo>>>,
    queue: VecDeque<(String, PtzAction)>,
    inflight: Option<(String, PtzAction, Pending<Result<()>>)>,
    /// Inputs currently held, per source. Merged into one command by [`PtzState::command`].
    gui: PtzState,
    hotkeys: Vec<(egui::Key, i32)>,
    gamepad_state: PtzState,
    /// Motor command that has been started (or is being started) on a camera and not yet stopped.
    motor: Option<(String, i32)>,
    /// Failed hold request to resend: `(when, camera, button, start)`.
    retry: Option<(Instant, String, i32, bool)>,
    /// The current video is live from a PTZ camera the user may control.
    available: bool,
    preset_thumbs: HashMap<(String, i32), Option<egui::TextureHandle>>,
    thumb_jobs: Vec<((String, i32), Pending<Result<egui::ColorImage>>)>,
    preset_edit: Option<(i32, String)>,
    gamepad: Option<gilrs::Gilrs>,
    gamepad_init: bool,
    /// Click-to-center / 3D positioning mode (`RelativePTZ`, ui3.js:8476).
    pub relative_mode: bool,
}

impl PtzPanel {
    pub fn camera_changed(&mut self, id: &str) {
        if self.camera.eq_ignore_ascii_case(id) {
            return;
        }
        // Inputs never carry over to another camera; `reconcile` stops the motor if it was running.
        self.gui = PtzState::default();
        self.hotkeys.clear();
        self.gamepad_state = PtzState::default();
        self.camera = id.to_string();
        self.info = None;
        self.info_job = None;
    }

    pub fn enqueue(&mut self, camera: &str, a: PtzAction) {
        self.queue.push_back((camera.to_string(), a));
    }

    /// Whether PTZ motion can currently be driven (live video from a PTZ camera the user may control).
    pub fn set_available(&mut self, on: bool) {
        self.available = on;
    }

    pub fn press_gui(&mut self, code: i32) {
        self.gui = PtzState::from_code(code);
    }

    pub fn release_gui(&mut self) {
        self.gui = PtzState::default();
    }

    pub fn hotkey_down(&mut self, key: egui::Key, code: i32) {
        if !self.hotkeys.iter().any(|(k, _)| *k == key) {
            self.hotkeys.push((key, code));
        }
    }

    pub fn hotkey_up(&mut self, key: egui::Key) {
        self.hotkeys.retain(|(k, _)| *k != key);
    }

    /// Drops every held input (window lost focus / minimized); the motor is stopped on the next `poll`.
    pub fn release_all_inputs(&mut self) {
        self.gui = PtzState::default();
        self.hotkeys.clear();
        self.gamepad_state = PtzState::default();
    }

    fn merged(&self) -> PtzState {
        self.hotkeys.iter().fold(self.gui.merge(self.gamepad_state), |s, (_, c)| s.merge(PtzState::from_code(*c)))
    }

    /// True if the pad button for `code` is currently driven by any input source.
    fn is_active(&self, code: i32) -> bool {
        self.merged().has(code)
    }

    /// Blocking best-effort stop for application exit.
    pub fn stop_motor_blocking(&mut self, client: &BiClient) {
        if let Some((cam, b)) = self.motor.take() {
            let c = client.clone();
            let _ = runtime::handle().block_on(async move { tokio::time::timeout(Duration::from_millis(1500), c.ptz_hold(&cam, b, false)).await });
        }
    }

    fn send(&mut self, ctx: &egui::Context, client: &BiClient, cam: String, a: PtzAction) {
        let c = client.clone();
        let (cam2, a2) = (cam.clone(), a.clone());
        let job = runtime::spawn(ctx, async move {
            match a2 {
                PtzAction::Button(b) => c.ptz(&cam2, b).await,
                PtzAction::Hold { button, start } => c.ptz_hold(&cam2, button, start).await,
                PtzAction::SetPreset { preset, desc } => c.ptz_set_preset(&cam2, preset, &desc).await,
                PtzAction::Relative { x, y, z } => c.ptz_relative(&cam2, x, y, z).await,
            }
        });
        self.inflight = Some((cam, a, job));
    }

    /// One step of UI3's `ptzChangeRespond`: with no request in flight, start / stop / switch the motor to match the
    /// merged inputs, or run the next one-time action once the motor is stopped.
    fn reconcile(&mut self, ctx: &egui::Context, client: &BiClient) {
        if self.inflight.is_some() {
            return;
        }
        if let Some((at, ..)) = &self.retry {
            let now = Instant::now();
            if now < *at {
                ctx.request_repaint_after(*at - now);
                return;
            }
            let (_, cam, button, start) = self.retry.take().unwrap();
            self.send(ctx, client, cam, PtzAction::Hold { button, start });
            return;
        }
        let desired = if self.available && self.queue.is_empty() && !self.camera.is_empty() { self.merged().command() } else { None };
        match (desired, self.motor.clone()) {
            (None, Some((cam, button))) => {
                self.motor = None;
                self.send(ctx, client, cam, PtzAction::Hold { button, start: false });
            }
            (None, None) => {
                if let Some((cam, a)) = self.queue.pop_front() {
                    self.send(ctx, client, cam, a);
                }
            }
            (Some(d), Some((cam, button))) => {
                if !(d == button && cam.eq_ignore_ascii_case(&self.camera)) {
                    self.motor = None;
                    self.send(ctx, client, cam, PtzAction::Hold { button, start: false });
                }
            }
            (Some(d), None) => {
                let cam = self.camera.clone();
                self.motor = Some((cam.clone(), d));
                self.send(ctx, client, cam, PtzAction::Hold { button: d, start: true });
            }
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context, client: &BiClient, svc: &mut Services) {
        if !self.camera.is_empty() && self.info.is_none() && self.info_job.is_none() {
            let (c, cam) = (client.clone(), self.camera.clone());
            self.info_job = Some(runtime::spawn(ctx, async move { c.ptz_info(&cam).await }));
        }
        if let Some(j) = &mut self.info_job
            && let Some(r) = j.poll()
        {
            self.info_job = None;
            match r {
                Ok(i) => self.info = Some(i),
                Err(_) => self.info = Some(PtzInfo::default()),
            }
        }
        if let Some((cam, a, j)) = &mut self.inflight
            && let Some(r) = j.poll()
        {
            let (cam, a) = (cam.clone(), a.clone());
            self.inflight = None;
            match (r, &a) {
                (Err(e), PtzAction::Hold { button, start }) => {
                    // Starts and stops are retried until they succeed so a motor is never left running.
                    tracing::warn!("PTZ hold {} failed ({e:#}); retrying", if *start { "start" } else { "stop" });
                    self.retry = Some((Instant::now() + RETRY_DELAY, cam, *button, *start));
                }
                (Err(e), _) => svc.toasts.error(format!("PTZ command failed. {e:#}")),
                (Ok(()), PtzAction::SetPreset { preset, .. }) => {
                    svc.toasts.success(format!("Preset {preset} set successfully."));
                    self.preset_thumbs.remove(&(cam.clone(), *preset));
                    self.info = None;
                }
                (Ok(()), PtzAction::Button(b)) if (ptz::IR_OFF..=ptz::IR_AUTO).contains(b) => {
                    if let Some(i) = &mut self.info {
                        i.irmode = (*b - ptz::IR_OFF) as i64;
                    }
                }
                (Ok(()), PtzAction::Button(b)) if (ptz::BRIGHTNESS_BASE..ptz::BRIGHTNESS_BASE + 16).contains(b) => {
                    if let Some(i) = &mut self.info {
                        i.brightness = (*b - ptz::BRIGHTNESS_BASE) as i64;
                    }
                }
                (Ok(()), PtzAction::Button(b)) if (ptz::CONTRAST_BASE..ptz::CONTRAST_BASE + 7).contains(b) => {
                    if let Some(i) = &mut self.info {
                        i.contrast = (*b - ptz::CONTRAST_BASE) as i64;
                    }
                }
                _ => {}
            }
        }
        self.reconcile(ctx, client);
        // Preset thumbnails.
        self.thumb_jobs.retain_mut(|(k, j)| match j.poll() {
            Some(Ok(img)) => {
                let t = ctx.load_texture(format!("preset-{}-{}", k.0, k.1), img, egui::TextureOptions::LINEAR);
                self.preset_thumbs.insert(k.clone(), Some(t));
                false
            }
            Some(Err(_)) => false,
            None => true,
        });
        self.poll_gamepad(svc);
    }

    fn preset_thumb(&mut self, ctx: &egui::Context, client: &BiClient, preset: i32) -> Option<egui::TextureHandle> {
        let k = (self.camera.clone(), preset);
        if let Some(t) = self.preset_thumbs.get(&k) {
            return t.clone();
        }
        self.preset_thumbs.insert(k.clone(), None);
        let c = client.clone();
        let path = bi_proto::urls::ptz_preset_image(&self.camera, preset);
        self.thumb_jobs.push((k, runtime::spawn(ctx, async move { crate::views::clips::fetch_color_image(&c, &path, None).await })));
        None
    }

    /// Gamepad PTZ (ui3_gamepad_ptz_enabled): left stick pans/tilts, right stick Y zooms (press-and-hold semantics).
    fn poll_gamepad(&mut self, svc: &mut Services) {
        if !svc.settings.bool("ui3_gamepad_ptz_enabled") || self.camera.is_empty() {
            return;
        }
        if !self.gamepad_init {
            self.gamepad_init = true;
            self.gamepad = gilrs::Gilrs::new().ok();
        }
        let Some(g) = self.gamepad.as_mut() else { return };
        while g.next_event().is_some() {}
        let dead = svc.settings.f64("ui3_gamepad_analog_deadzone").clamp(0.05, 0.9) as f32;
        let mut dir = None;
        let mut zoom = None;
        if let Some((_, pad)) = g.gamepads().next() {
            let x = pad.value(gilrs::Axis::LeftStickX);
            let y = pad.value(gilrs::Axis::LeftStickY);
            let rz = pad.value(gilrs::Axis::RightStickY);
            let (r, u, l, d) = (x > dead, y > dead, x < -dead, y < -dead);
            dir = match (u, d, l, r) {
                (true, _, true, _) => Some(ptz::UP_LEFT),
                (true, _, _, true) => Some(ptz::UP_RIGHT),
                (_, true, true, _) => Some(ptz::DOWN_LEFT),
                (_, true, _, true) => Some(ptz::DOWN_RIGHT),
                (true, ..) => Some(ptz::UP),
                (_, true, ..) => Some(ptz::DOWN),
                (_, _, true, _) => Some(ptz::LEFT),
                (_, _, _, true) => Some(ptz::RIGHT),
                _ => None,
            };
            if rz > dead || pad.is_pressed(gilrs::Button::RightTrigger) {
                zoom = Some(ptz::ZOOM_IN);
            } else if rz < -dead || pad.is_pressed(gilrs::Button::LeftTrigger) {
                zoom = Some(ptz::ZOOM_OUT);
            }
        }
        self.gamepad_state = dir.map(PtzState::from_code).unwrap_or_default().merge(zoom.map(PtzState::from_code).unwrap_or_default());
    }
}

/// UI3's PTZ pad graphic: (svg id, icon, layout rect within the 190x190 pad).
const PAD_GRAPHICS: &[(&str, crate::icons::Icon, [f32; 4])] = &[
    ("PTZzoomIn", icons::PTZZOOM_IN, [67.0, 53.0, 56.0, 26.0]),
    ("PTZzoomOut", icons::PTZZOOM_OUT, [67.0, 111.0, 56.0, 26.0]),
    ("PTZfocusSmall", icons::PTZFOCUS_SMALL, [111.0, 67.0, 26.0, 56.0]),
    ("PTZfocusLarge", icons::PTZFOCUS_LARGE, [53.0, 67.0, 26.0, 56.0]),
    ("PTZstop", icons::PTZSTOP, [78.0, 78.0, 34.0, 34.0]),
    ("PTZcardinalUp", icons::PTZCARDINAL_UP, [83.0, 24.0, 24.0, 15.0]),
    ("PTZcardinalRight", icons::PTZCARDINAL_RIGHT, [151.0, 83.0, 15.0, 24.0]),
    ("PTZcardinalDown", icons::PTZCARDINAL_DOWN, [83.0, 151.0, 24.0, 15.0]),
    ("PTZcardinalLeft", icons::PTZCARDINAL_LEFT, [24.0, 83.0, 15.0, 24.0]),
    ("PTZordinalNE", icons::PTZORDINAL_NE, [136.0, 47.0, 7.0, 7.0]),
    ("PTZordinalNW", icons::PTZORDINAL_NW, [47.0, 47.0, 7.0, 7.0]),
    ("PTZordinalSW", icons::PTZORDINAL_SW, [47.0, 136.0, 7.0, 7.0]),
    ("PTZordinalSE", icons::PTZORDINAL_SE, [136.0, 136.0, 7.0, 7.0]),
];

/// Hit polygons from `PtzButtons` (ui3.js:6940), in 190x190 pad coordinates.
const HIT_POLYS: &[(&str, &[[f32; 2]])] = &[
    ("PTZzoomIn", &[[64., 64.], [82., 82.], [91., 77.], [99., 77.], [106., 81.], [126., 64.], [116., 58.], [105., 53.], [86., 53.], [74., 58.]]),
    ("PTZzoomOut", &[[64., 126.], [82., 108.], [91., 113.], [99., 113.], [106., 109.], [126., 126.], [116., 132.], [105., 137.], [86., 137.], [74., 132.]]),
    ("PTZfocusLarge", &[[64., 64.], [82., 82.], [77., 91.], [77., 99.], [81., 106.], [64., 126.], [58., 116.], [53., 105.], [53., 86.], [58., 74.]]),
    ("PTZfocusSmall", &[[126., 64.], [108., 82.], [113., 91.], [113., 99.], [109., 106.], [126., 126.], [132., 116.], [137., 105.], [137., 86.], [132., 74.]]),
    ("PTZstop", &[[82., 82.], [91., 77.], [99., 77.], [108., 82.], [113., 91.], [113., 99.], [108., 108.], [99., 113.], [91., 113.], [82., 108.], [77., 99.], [77., 91.]]),
    ("PTZcardinalUp", &[[52., 9.], [74., 58.], [86., 53.], [105., 53.], [116., 58.], [138., 9.], [96., 0.]]),
    ("PTZcardinalRight", &[[181., 52.], [132., 74.], [138., 86.], [138., 105.], [132., 116.], [181., 138.], [190., 96.]]),
    ("PTZcardinalDown", &[[52., 181.], [74., 132.], [86., 138.], [105., 138.], [116., 132.], [138., 181.], [96., 190.]]),
    ("PTZcardinalLeft", &[[9., 52.], [58., 74.], [53., 86.], [53., 105.], [58., 116.], [9., 138.], [0., 96.]]),
    ("PTZordinalNE", &[[138., 9.], [116., 58.], [124., 63.], [127., 66.], [132., 74.], [181., 52.], [171., 19.]]),
    ("PTZordinalNW", &[[52., 9.], [74., 58.], [66., 63.], [63., 66.], [58., 74.], [9., 52.], [19., 19.]]),
    ("PTZordinalSW", &[[52., 181.], [74., 132.], [66., 127.], [63., 124.], [58., 116.], [9., 138.], [19., 171.]]),
    ("PTZordinalSE", &[[138., 181.], [116., 132.], [124., 127.], [127., 124.], [132., 116.], [181., 138.], [171., 171.]]),
];

fn pad_cmd(id: &str, swap_focus: bool) -> (i32, &'static str) {
    match id {
        "PTZzoomIn" => (ptz::ZOOM_IN, "Zoom In"),
        "PTZzoomOut" => (ptz::ZOOM_OUT, "Zoom Out"),
        "PTZfocusLarge" if swap_focus => (ptz::FOCUS_FAR, "Focus Far"),
        "PTZfocusLarge" => (ptz::FOCUS_NEAR, "Focus Near"),
        "PTZfocusSmall" if swap_focus => (ptz::FOCUS_NEAR, "Focus Near"),
        "PTZfocusSmall" => (ptz::FOCUS_FAR, "Focus Far"),
        "PTZstop" => (ptz::STOP, "Stop"),
        "PTZcardinalUp" => (ptz::UP, "Up"),
        "PTZcardinalRight" => (ptz::RIGHT, "Right"),
        "PTZcardinalDown" => (ptz::DOWN, "Down"),
        "PTZcardinalLeft" => (ptz::LEFT, "Left"),
        "PTZordinalNE" => (ptz::UP_RIGHT, "Up Right"),
        "PTZordinalNW" => (ptz::UP_LEFT, "Up Left"),
        "PTZordinalSW" => (ptz::DOWN_LEFT, "Down Left"),
        _ => (ptz::DOWN_RIGHT, "Down Right"),
    }
}

fn point_in_polygon(p: [f32; 2], poly: &[[f32; 2]]) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, yi, xj, yj) = (poly[i][0], poly[i][1], poly[j][0], poly[j][1]);
        if (yi > p[1]) != (yj > p[1]) && p[0] < (xj - xi) * (p[1] - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

pub fn show(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services) {
    let ctx = ui.ctx().clone();
    // UI3 keeps the PTZ box visible but greyed out when the current video has no PTZ camera (ui3.js:7247).
    let single = view.current_single_camera().filter(|c| c.ptz).map(|c| c.id.clone());
    let enabled = single.is_some();
    let cam = single.unwrap_or_default();
    if enabled {
        view.ptz.camera_changed(&cam);
    } else {
        ui.disable();
    }
    let client = view.client.clone();
    view.ptz.poll(&ctx, &client, svc);
    let p = &mut view.ptz;
    let swap = svc.settings.bool("ui3_swap_ptz_focus_buttons");
    let th = style::t();

    // ---- Pad (190x190, UI3 #ptzGraphicWrapper) ----
    let (pad, resp) = ui.allocate_exact_size(vec2(190.0, 190.0), egui::Sense::click_and_drag());
    let resp = resp.on_disabled_hover_text("PTZ not available for current camera");
    let hit = |pos: egui::Pos2| -> Option<&'static str> {
        let q = [pos.x - pad.left(), pos.y - pad.top()];
        HIT_POLYS.iter().find(|(_, poly)| point_in_polygon(q, poly)).map(|(id, _)| *id)
    };
    let hovered = resp.hover_pos().and_then(hit);
    let pressed = if resp.is_pointer_button_down_on() { ui.input(|i| i.pointer.interact_pos()).and_then(hit) } else { None };
    let r = |l: [f32; 4]| egui::Rect::from_min_size(pad.min + vec2(l[0], l[1]), vec2(l[2], l[3]));
    w::paint_icon(ui, icons::PTZBACKGROUND, r([0.0, 0.0, 190.0, 190.0]), if enabled { th.ptzpad } else { th.ptzpad_disabled });
    w::paint_icon(ui, icons::PTZOUTSIDE_RING, r([2.0, 2.0, 186.0, 186.0]), th.panel);
    for (id, icon, l) in PAD_GRAPHICS {
        let (code, _) = pad_cmd(id, swap);
        let active = pressed == Some(*id) || p.is_active(code);
        let color = if active {
            th.text_active
        } else if hovered == Some(*id) {
            th.text_hover
        } else {
            th.panel
        };
        w::paint_icon(ui, *icon, r(*l), color);
    }
    if let Some(id) = hovered {
        resp.clone().on_hover_text(pad_cmd(id, swap).1);
    }
    let mut released = !ui.input(|i| i.pointer.any_down());
    if let Some(id) = pressed {
        let (code, _) = pad_cmd(id, swap);
        if code == ptz::STOP {
            if ui.input(|i| i.pointer.primary_pressed()) {
                p.enqueue(&cam, PtzAction::Button(ptz::STOP));
            }
        } else {
            p.press_gui(code);
            released = false;
        }
    } else if resp.is_pointer_button_down_on() {
        // Dragged off the buttons: stop moving.
        released = true;
    }
    if released {
        p.release_gui();
    }
    ui.add_space(15.0);

    // ---- IR / brightness / contrast (#ptzIrBrightnessContrast) ----
    if svc.settings.bool("ui3_ir_brightness_contrast") {
        let info = p.info.clone().unwrap_or_default();
        let ir = match info.irmode {
            1 => "IR On",
            2 => "IR Auto",
            _ => "IR Off",
        };
        let (r1, r2, r3) = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let r1 = w::square_button(ui, vec2(38.0, 38.0), Some(icons::LIGHTBULB), "", ir, true);
                ui.label(egui::RichText::new(ir).size(13.0));
                let r2 = w::square_button(ui, vec2(38.0, 38.0), Some(icons::BRIGHTNESS), "", "Brightness", true);
                ui.label(egui::RichText::new(info.brightness.max(0).to_string()).size(13.0));
                let r3 = w::square_button(ui, vec2(38.0, 38.0), Some(icons::CONTRAST), "", "Contrast", true);
                ui.label(egui::RichText::new(info.contrast.max(0).to_string()).size(13.0));
                (r1, r2, r3)
            })
            .inner;
        egui::Popup::menu(&r1).frame(w::list_frame()).show(|ui| {
            for (code, l) in [(ptz::IR_OFF, "IR Off"), (ptz::IR_ON, "IR On"), (ptz::IR_AUTO, "IR Auto")] {
                if w::list_item(ui, None, l, l == ir, w::ItemKind::Normal).clicked() {
                    p.enqueue(&cam, PtzAction::Button(code));
                    ui.close();
                }
            }
        });
        egui::Popup::menu(&r2).frame(w::list_frame()).show(|ui| {
            for n in 0..=15 {
                if w::list_item(ui, None, &n.to_string(), n == info.brightness, w::ItemKind::Normal).clicked() {
                    p.enqueue(&cam, PtzAction::Button(ptz::BRIGHTNESS_BASE + n as i32));
                    ui.close();
                }
            }
        });
        egui::Popup::menu(&r3).frame(w::list_frame()).show(|ui| {
            for n in 0..=6 {
                if w::list_item(ui, None, &n.to_string(), n == info.contrast, w::ItemKind::Normal).clicked() {
                    p.enqueue(&cam, PtzAction::Button(ptz::CONTRAST_BASE + n as i32));
                    ui.close();
                }
            }
        });
        ui.add_space(15.0);
    }

    // ---- PTZ Home (#ptzHome, ui3_ptzHome) ----
    if svc.settings.bool("ui3_ptzHome") {
        if w::square_button(ui, vec2(190.0, 38.0), Some(icons::HOME), "PTZ Home", "Sends a PTZ home command to the camera.  Results may vary.", true).clicked() {
            p.enqueue(&cam, PtzAction::Button(ptz::HOME));
        }
        ui.add_space(15.0);
    }

    // ---- Toggle 3D Positioning (#ptzRelativeToggle) ----
    let label = if p.relative_mode { "Disable 3D Positioning" } else { "Toggle 3D Positioning" };
    let (rect, r) = ui.allocate_exact_size(vec2(190.0, 28.0), egui::Sense::click());
    if r.hovered() {
        ui.painter().rect_stroke(rect.shrink(1.0), 0.0, egui::Stroke::new(2.0, egui::Color32::from_white_alpha(128)), egui::StrokeKind::Outside);
    }
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(16.0), if p.relative_mode { th.dd_blue } else { th.text });
    if r.on_hover_text("3D positioning: click the video to center that point; use the mouse wheel to zoom").clicked() {
        p.relative_mode = !p.relative_mode;
    }
    ui.add_space(15.0);

    // ---- Presets: rows of five square buttons (.fiveByTwoButtonSet) ----
    let count = (svc.settings.f64("ui3_ptzPresetShowCount") as i32).clamp(0, 40);
    let descs = p.info.as_ref().map(|i| i.preset_map()).unwrap_or_default();
    let can_set = view.session.login.permissions().ptz_preset_set;
    let mut n = 1;
    while n <= count {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for _ in 0..5 {
                if n > count {
                    break;
                }
                let desc = descs.get(&(n as i64)).cloned().filter(|d| !d.is_empty() && d != "(undefined)").unwrap_or_else(|| format!("Preset {n}"));
                let thumb = if enabled { p.preset_thumb(&ctx, &client, n) } else { None };
                let num = n.to_string();
                let r = w::square_button(ui, vec2(38.0, 38.0), None, if thumb.is_some() { "" } else { &num }, "", true);
                if let Some(t) = thumb {
                    let fit = bi_render::fit_rect([t.size()[0] as u32, t.size()[1] as u32], r.rect.shrink(1.0));
                    ui.painter().image(t.id(), fit, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                    if r.hovered() {
                        ui.painter().rect_filled(r.rect, 0.0, th.ui_hover);
                    }
                }
                let r = r.on_hover_text(format!("{n}: {desc}"));
                if r.clicked() {
                    p.enqueue(&cam, PtzAction::Button(ptz::PRESET_BASE + n));
                }
                if can_set {
                    let d2 = desc.clone();
                    let nn = n;
                    let mut edit = None;
                    r.context_menu(|ui| {
                        if w::list_item(ui, None, &format!("Set preset {nn} to the current position…"), false, w::ItemKind::Normal).clicked() {
                            edit = Some((nn, d2.clone()));
                            ui.close();
                        }
                    });
                    if edit.is_some() {
                        p.preset_edit = edit;
                    }
                }
                n += 1;
            }
        });
    }
    let mut close = false;
    if let Some((n, desc)) = &mut p.preset_edit {
        w::dialog(&format!("Set preset {n}"), "ptz_preset_edit").show(&ctx, |ui| {
            ui.label("Description:");
            ui.text_edit_singleline(desc);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    let d = if desc.trim().is_empty() { format!("Preset {n}") } else { desc.clone() };
                    let a = PtzAction::SetPreset { preset: *n, desc: d };
                    p.queue.push_back((cam.clone(), a));
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
    }
    if close {
        p.preset_edit = None;
    }
    if p.motor.is_some() || p.retry.is_some() || p.inflight.is_some() || !p.queue.is_empty() {
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_directions_become_diagonals() {
        let s = PtzState::from_code(ptz::UP).merge(PtzState::from_code(ptz::RIGHT));
        assert_eq!(s.command(), Some(ptz::UP_RIGHT));
        assert!(s.has(ptz::UP) && s.has(ptz::RIGHT) && s.has(ptz::UP_RIGHT) && !s.has(ptz::DOWN));
        assert_eq!(PtzState::from_code(ptz::UP).command(), Some(ptz::UP));
        assert_eq!(PtzState::default().command(), None);
    }

    #[test]
    fn zoom_outranks_direction() {
        let s = PtzState::from_code(ptz::LEFT).merge(PtzState::from_code(ptz::ZOOM_IN));
        assert_eq!(s.command(), Some(ptz::ZOOM_IN));
    }
}
