//! Dialogs and menus: UI settings (generated from UI3's settings table), streaming profiles editor, camera list
//! and properties, server status / disk usage, system log, users, devices, export (server API and client-side
//! MP4), export queue, server control, update check, about, clip properties, camera context menu, snapshots
//! and clip downloads.

use crate::app::Services;
use crate::runtime::{self, Pending};
use crate::settings::{CATEGORIES, DEFS, Dv, Hotkey, Input, Settings};
use crate::views::main_view::{Content, MainView};
use crate::views::style;
use crate::views::w;
use anyhow::Result;
use bi_proto::api::{ClipItem, flags};
use bi_proto::commands::{CamConfig, ExportRequest};
use bi_proto::profile::{CodecPref, RateControl, StreamingProfile, VCodec};
use bi_proto::urls::OverlayArgs;
use egui::{Color32, RichText};
use serde_json::Value;
use std::time::{Duration, Instant};

pub enum Dialog {
    Settings,
    StreamingProfiles,
    CameraList,
    CameraProperties(String),
    CameraPause(String),
    ServerStatus,
    SystemLog,
    UserList,
    DeviceList,
    ExportList,
    ServerControl,
    UpdateCheck,
    About,
    ClipProperties(Box<ClipItem>),
    Export(Box<ClipItem>),
    TimelineExport { camera: String, start: i64, end: i64 },
}

#[derive(Default)]
pub struct Dialogs {
    pub open: Vec<Dialog>,
    pub context_camera: Option<Option<String>>,
    settings_category: Option<&'static str>,
    settings_filter: String,
    capturing_hotkey: Option<&'static str>,
    profile_sel: usize,
    json_job: Option<(String, Pending<Result<Value>>)>,
    json_cache: std::collections::HashMap<String, (Value, Instant)>,
    export_opts: Option<ExportOpts>,
    export_job: Option<Pending<Result<String>>>,
    client_export: Option<ClientExport>,
    camcfg: Option<(String, Value)>,
    camcfg_job: Option<Pending<Result<Value>>>,
    download_jobs: Vec<Pending<Result<String>>>,
    pause_secs: i64,
}

impl Dialogs {
    pub fn open(&mut self, d: Dialog) {
        // One instance of each kind.
        let same = |a: &Dialog, b: &Dialog| std::mem::discriminant(a) == std::mem::discriminant(b);
        self.open.retain(|x| !same(x, &d));
        if let Dialog::Export(_) | Dialog::TimelineExport { .. } = d {
            self.export_opts = None;
        }
        self.open.push(d);
    }
    pub fn close_top(&mut self) -> bool {
        self.open.pop().is_some()
    }
}

/// Per-camera overlay overrides (`ClipOverlayCfg`, stored as JSON in ui3_clipOverlayCfg).
pub fn overlay_args(s: &Settings, cam: &str) -> OverlayArgs {
    let cfg: Value = serde_json::from_str(&s.str("ui3_clipOverlayCfg")).unwrap_or_default();
    let c = &cfg[cam];
    OverlayArgs { motion: c["m"].as_u64().unwrap_or(0) as u8, text: c["t"].as_u64().unwrap_or(0) as u8 }
}

fn set_overlay(s: &mut Settings, cam: &str, key: &str, v: u8) {
    let mut cfg: Value = serde_json::from_str(&s.str("ui3_clipOverlayCfg")).unwrap_or_else(|_| serde_json::json!({}));
    if !cfg.is_object() {
        cfg = serde_json::json!({});
    }
    let e = cfg.as_object_mut().unwrap().entry(cam.to_string()).or_insert_with(|| serde_json::json!({}));
    if v == 0 {
        e.as_object_mut().map(|o| o.remove(key));
    } else {
        e[key] = v.into();
    }
    s.set("ui3_clipOverlayCfg", serde_json::to_string(&cfg).unwrap_or_default());
}

/// Live-camera context menu (`CanvasContextMenu`, ui3.js:27466), drawn as a UI3 `.dropdown_list`.
pub fn camera_menu(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services, cam_id: &str) {
    let ctx = ui.ctx().clone();
    let Some(cam) = view.camera(cam_id).cloned() else {
        ui.label("Unknown camera");
        return;
    };
    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
    ui.set_min_width(260.0);
    ui.set_max_width(300.0);
    let item = |ui: &mut egui::Ui, icon: crate::icons::Icon, label: &str| w::list_item(ui, Some(icon), label, false, w::ItemKind::Normal);
    // Heading: camera name in the camera's color.
    let (hr, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::hover());
    ui.painter().rect_filled(egui::Rect::from_min_size(hr.min, egui::vec2(4.0, hr.height())), 0.0, style::rgb3(cam.color_rgb()));
    ui.painter().text(hr.left_center() + egui::vec2(14.0, 0.0), egui::Align2::LEFT_CENTER, &cam.name, style::bold(16.0), style::t().text_active);
    w::list_separator(ui);
    if item(ui, crate::icons::VIDEOCAM, "Open Live View").clicked() {
        view.select_source(&ctx, svc, &cam.id);
        close_ctx_menu(&ctx);
    }
    if item(ui, crate::icons::SNAPSHOT, "Save Snapshot").clicked() {
        save_snapshot_of(view, svc, &cam.id);
        close_ctx_menu(&ctx);
    }
    let admin = view.session.login.admin;
    if !cam.is_group() {
        if item(ui, crate::icons::ALERT1, "Trigger Now").clicked() {
            run_action(view, &ctx, "Triggered", {
                let (c, id) = (view.client.clone(), cam.id.clone());
                async move { c.trigger(&id).await }
            });
            close_ctx_menu(&ctx);
        }
        if admin {
            w::list_separator(ui);
            let rec = cam.is_man_rec;
            if item(ui, crate::icons::STOPLIGHT, if rec { "Stop Manual Recording" } else { "Start Manual Recording" }).clicked() {
                let (c, id) = (view.client.clone(), cam.id.clone());
                run_action(view, &ctx, if rec { "Manual recording stopped" } else { "Manual recording started" }, async move { c.camconfig(&id, CamConfig::ManualRecord(!rec)).await.map(|_| ()) });
                close_ctx_menu(&ctx);
            }
            if item(ui, crate::icons::PAUSE, "Pause…").clicked() {
                view.dialogs.open(Dialog::CameraPause(cam.id.clone()));
                close_ctx_menu(&ctx);
            }
            if svc.settings.bool("ui3_contextMenus_enableDisableCamera") {
                let en = cam.is_enabled;
                if item(ui, crate::icons::HOLD_PROFILE, if en { "Disable Camera" } else { "Enable Camera" }).clicked() {
                    let (c, id) = (view.client.clone(), cam.id.clone());
                    run_action(view, &ctx, if en { "Camera disabled" } else { "Camera enabled" }, async move { c.camconfig(&id, CamConfig::Enable(!en)).await.map(|_| ()) });
                    close_ctx_menu(&ctx);
                }
            }
            if item(ui, crate::icons::RESTART, "Restart Camera").clicked() {
                let (c, id) = (view.client.clone(), cam.id.clone());
                run_action(view, &ctx, "Camera restart requested", async move { c.camconfig(&id, CamConfig::Reset).await.map(|_| ()) });
                close_ctx_menu(&ctx);
            }
            if item(ui, crate::icons::LOGOUT, "Reboot Camera").clicked() {
                let (c, id) = (view.client.clone(), cam.id.clone());
                run_action(view, &ctx, "Camera reboot requested", async move { c.camconfig(&id, CamConfig::Reboot).await.map(|_| ()) });
                close_ctx_menu(&ctx);
            }
            if item(ui, crate::icons::SETTINGS, "Properties").clicked() {
                view.dialogs.open(Dialog::CameraProperties(cam.id.clone()));
                close_ctx_menu(&ctx);
            }
        }
        w::list_separator(ui);
        let ov = overlay_args(svc.settings, &cam.id);
        for (label, key, cur) in [("Motion overlay", "m", ov.motion), ("Text/graphic overlay", "t", ov.text)] {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(egui::RichText::new(label).size(14.0));
                ui.add_space(6.0);
                for (v, n) in [(0u8, "Default"), (1, "Off"), (2, "On")] {
                    if ui.selectable_label(cur == v, n).clicked() {
                        set_overlay(svc.settings, &cam.id, key, v);
                        view.reload_current(&ctx, svc);
                    }
                }
            });
            ui.add_space(4.0);
        }
    } else {
        w::list_separator(ui);
        group_options_menu(view, ui, svc, &cam.id);
    }
    w::list_separator(ui);
    if item(ui, crate::icons::INFO, if svc.settings.native.show_stats_overlay { "Hide Stats for Nerds" } else { "Stats for Nerds" }).clicked() {
        svc.settings.native.show_stats_overlay = !svc.settings.native.show_stats_overlay;
        svc.settings.mark_dirty();
        close_ctx_menu(&ctx);
    }
}

fn group_options_menu(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services, id: &str) {
    let ctx = ui.ctx().clone();
    ui.add_space(4.0);
    ui.label(egui::RichText::new("   Group layout options").size(14.0).color(style::t().text_active));
    ui.scope(|ui| {
        let mut cfg: Value = serde_json::from_str(&svc.settings.str("ui3_groupCfg")).unwrap_or_else(|_| serde_json::json!({}));
        if !cfg.is_object() {
            cfg = serde_json::json!({});
        }
        let key = id.to_ascii_lowercase();
        let mut changed = false;
        for (label, k) in [("Camera names", "scn"), ("Camera borders", "scb"), ("Show hidden cameras", "shc"), ("Hide disabled cameras", "hdc"), ("Hide inactive cameras", "hicwv")] {
            let cur = cfg[&key][k].as_u64().unwrap_or(0);
            ui.horizontal(|ui| {
                ui.label(label);
                for (v, n) in [(0u64, "Default"), (1, "Off"), (2, "On")] {
                    if ui.selectable_label(cur == v, n).clicked() {
                        let e = cfg.as_object_mut().unwrap().entry(key.clone()).or_insert_with(|| serde_json::json!({}));
                        e[k] = v.into();
                        changed = true;
                    }
                }
            });
        }
        if changed {
            svc.settings.set("ui3_groupCfg", serde_json::to_string(&cfg).unwrap_or_default());
            view.reload_current(&ctx, svc);
        }
    });
}

fn run_action(view: &mut MainView, ctx: &egui::Context, ok_msg: &'static str, fut: impl std::future::Future<Output = Result<()>> + Send + 'static) {
    view.dialogs.download_jobs.push(runtime::spawn(ctx, async move { fut.await.map(|_| ok_msg.to_string()) }));
}

pub fn save_snapshot(view: &mut MainView, svc: &mut Services) {
    if save_local_snapshot(view, svc) {
        return;
    }
    match view.content.clone() {
        Content::Live { id } | Content::Timeline { id } => save_snapshot_of(view, svc, &id),
        Content::Clip { .. } => {
            if let Some(c) = view.playback.clip.clone() {
                let q = svc.settings.f64("ui3_download_snapshot_server_quality") as i32;
                let path = bi_proto::urls::clip_snapshot(c.video_path(), view.playback.position_ms, q);
                let name = format!("{}_{}.jpg", c.camera, chrono::Local::now().format("%Y%m%d_%H%M%S"));
                start_download(view, path, name);
            }
        }
        Content::None => {}
    }
}

/// `ui3_download_snapshot_method` = "Local (JPEG)" / "Local (PNG)": saves the picture on screen instead of asking the
/// server for one (ui3.js:33925). Returns false when the server path should be used (setting is "Server", grid view,
/// or nothing to capture).
fn save_local_snapshot(view: &mut MainView, svc: &mut Services) -> bool {
    let png = match svc.settings.str("ui3_download_snapshot_method").as_str() {
        "Local (JPEG)" => false,
        "Local (PNG)" => true,
        _ => return false,
    };
    if view.grid.is_active() || !view.video.has_picture() {
        return false;
    }
    // Camera name and the server-local time of the frame: now for live video, the playback position otherwise.
    let tz = view.session.login.tz_offset_ms();
    let (cam_id, utc_ms) = match &view.content {
        Content::Live { id } => (id.clone(), chrono::Utc::now().timestamp_millis()),
        Content::Timeline { id } => (id.clone(), view.timeline.current_time()),
        Content::Clip { .. } => match &view.playback.clip {
            Some(c) => (c.camera.clone(), c.date * 1000 + view.playback.position_ms),
            None => return false,
        },
        Content::None => return false,
    };
    let Some(img) = view.video.capture(svc.gpu) else { return false };
    let cam = view.camera(&cam_id).map(|c| c.name.clone()).unwrap_or(cam_id);
    let stamp = chrono::DateTime::from_timestamp_millis(utc_ms + tz).map(|d| d.naive_utc().format("%Y-%m-%d %H.%M.%S").to_string()).unwrap_or_default();
    let ext = if png { "png" } else { "jpg" };
    let Some(dest) = rfd::FileDialog::new().set_file_name(format!("{cam} {stamp}.{ext}")).save_file() else { return true };
    let quality = (svc.settings.f64("ui3_download_snapshot_server_quality") as u8).clamp(1, 100);
    let result = encode_snapshot(&img, png, quality).and_then(|bytes| std::fs::write(&dest, bytes).map_err(Into::into));
    match result {
        Ok(()) => svc.toasts.success(format!("Saved {}", dest.display())),
        Err(e) => svc.toasts.error(format!("Could not save snapshot: {e:#}")),
    }
    true
}

fn encode_snapshot(img: &bi_media::jpeg::RgbaImage, png: bool, quality: u8) -> anyhow::Result<Vec<u8>> {
    let rgba = image::RgbaImage::from_raw(img.width, img.height, img.rgba.clone()).ok_or_else(|| anyhow::anyhow!("bad frame size"))?;
    let mut out = Vec::new();
    if png {
        image::DynamicImage::ImageRgba8(rgba).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
    } else {
        let rgb = image::DynamicImage::ImageRgba8(rgba).to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(&rgb)?;
    }
    Ok(out)
}

fn save_snapshot_of(view: &mut MainView, svc: &mut Services, id: &str) {
    let q = svc.settings.f64("ui3_download_snapshot_server_quality") as i32;
    let path = bi_proto::urls::live_snapshot(id, q, chrono::Utc::now().timestamp_millis());
    let name = format!("{}_{}.jpg", id, chrono::Local::now().format("%Y%m%d_%H%M%S"));
    start_download(view, path, name);
}

pub fn download_clips(view: &mut MainView, svc: &mut Services, items: Vec<ClipItem>) {
    let original = svc.settings.bool("ui3_clipDownloadOriginalName");
    let h24 = svc.settings.bool("ui3_time24hour");
    let tz = view.session.login.tz_offset_ms();
    for it in items {
        // UI3 `GetDownloadClipInfo` (ui3.js:14096): "<camera name> <date>" + everything from the first "." of the path.
        let path = it.video_path().to_string();
        let ext = path.find('.').map(|i| path[i..].to_string()).unwrap_or_else(|| ".bvr".to_string());
        let cam = view.camera(&it.camera).map(|c| c.name.clone()).unwrap_or_else(|| it.camera.clone());
        let stamp = chrono::DateTime::from_timestamp_millis(it.date * 1000 + tz)
            .map(|d| d.naive_utc().format(if h24 { "%Y-%m-%d %H.%M.%S" } else { "%Y-%m-%d %I.%M.%S %p" }).to_string())
            .unwrap_or_default();
        let (url, name) = (bi_proto::urls::clip_download(&path, original), format!("{cam} {stamp}{ext}"));
        if original {
            start_download_server_named(view, url, name);
        } else {
            start_download(view, url, name);
        }
    }
}

/// Downloads `path`, asking for the destination only after the response arrives so that the server's own
/// `Content-Disposition` file name (from `original_name=1`) can be offered as the default.
fn start_download_server_named(view: &mut MainView, path: String, fallback_name: String) {
    let c = view.client.clone();
    view.dialogs.download_jobs.push(runtime::spawn(&egui::Context::default(), async move {
        let r = c.get_bytes(&path).await?;
        anyhow::ensure!(r.status < 400, "HTTP {}", r.status);
        let name = r.header("content-disposition").and_then(filename_from_disposition).unwrap_or(fallback_name);
        let Some(dest) = rfd::AsyncFileDialog::new().set_file_name(&name).save_file().await else { return Ok("Download cancelled".to_string()) };
        tokio::fs::write(dest.path(), &r.body).await?;
        Ok(format!("Saved {}", dest.path().display()))
    }));
}

fn filename_from_disposition(v: &str) -> Option<String> {
    let rest = &v[v.to_ascii_lowercase().find("filename=")? + "filename=".len()..];
    let name = rest.split(';').next()?.trim().trim_matches('"');
    // Never let the server pick a directory.
    let name = name.rsplit(['\\', '/']).next()?;
    (!name.is_empty()).then(|| name.to_string())
}

fn start_download(view: &mut MainView, path: String, default_name: String) {
    let Some(dest) = rfd::FileDialog::new().set_file_name(&default_name).save_file() else { return };
    let c = view.client.clone();
    view.dialogs.download_jobs.push(runtime::spawn(&egui::Context::default(), async move {
        let r = c.get_bytes(&path).await?;
        anyhow::ensure!(r.status < 400, "HTTP {}", r.status);
        tokio::fs::write(&dest, &r.body).await?;
        Ok(format!("Saved {}", dest.display()))
    }));
}

pub fn show(view: &mut MainView, ctx: &egui::Context, svc: &mut Services) {
    // Background action results.
    view.dialogs.download_jobs.retain_mut(|j| match j.poll() {
        Some(Ok(m)) => {
            svc.toasts.success(m);
            false
        }
        Some(Err(e)) => {
            svc.toasts.error(format!("{e:#}"));
            false
        }
        None => true,
    });
    if !view.dialogs.download_jobs.is_empty() {
        ctx.request_repaint_after(Duration::from_millis(200));
    }
    // The right click that opens the menu must not also close it.
    let just_opened = view.dialogs.context_camera.is_some();
    if let Some(cam) = view.dialogs.context_camera.take() {
        let id = cam.or_else(|| view.current_live_id().map(str::to_string));
        if let Some(id) = id {
            let pos = ctx.input(|i| i.pointer.hover_pos()).unwrap_or_default();
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("ctxcam"), (id, pos)));
        }
    }
    if let Some((id, pos)) = ctx.data(|d| d.get_temp::<(String, egui::Pos2)>(egui::Id::new("ctxcam"))) {
        let mut close = false;
        let r = egui::Area::new(egui::Id::new("ctxcam_area")).order(egui::Order::Foreground).fixed_pos(pos).constrain(true).show(ctx, |ui| {
            w::list_frame().show(ui, |ui| {
                camera_menu(view, ui, svc, &id);
            });
        });
        if !just_opened && (r.response.clicked_elsewhere() || ctx.input(|i| i.key_pressed(egui::Key::Escape))) {
            close = true;
        }
        if !just_opened && ctx.input(|i| i.pointer.any_click()) && !r.response.contains_pointer() {
            close = true;
        }
        if close {
            ctx.data_mut(|d| d.remove::<(String, egui::Pos2)>(egui::Id::new("ctxcam")));
        }
    }

    let mut i = 0;
    while i < view.dialogs.open.len() {
        let mut open = true;
        let d = std::mem::replace(&mut view.dialogs.open[i], Dialog::About);
        match &d {
            Dialog::Settings => settings_window(view, ctx, svc, &mut open),
            Dialog::StreamingProfiles => profiles_window(view, ctx, svc, &mut open),
            Dialog::CameraList => camera_list_window(view, ctx, svc, &mut open),
            Dialog::CameraProperties(id) => camera_properties_window(view, ctx, svc, id, &mut open),
            Dialog::CameraPause(id) => camera_pause_window(view, ctx, id, &mut open),
            Dialog::ServerStatus => server_status_window(view, ctx, &mut open),
            Dialog::SystemLog => json_window(view, ctx, "System Log", "log", &mut open),
            Dialog::UserList => json_window(view, ctx, "User List", "users", &mut open),
            Dialog::DeviceList => json_window(view, ctx, "Device List", "devices", &mut open),
            Dialog::ExportList => export_list_window(view, ctx, svc, &mut open),
            Dialog::ServerControl => server_control_window(view, ctx, svc, &mut open),
            Dialog::UpdateCheck => json_window(view, ctx, "Blue Iris Updates", "updatecheck", &mut open),
            Dialog::About => about_window(view, ctx, svc, &mut open),
            Dialog::ClipProperties(c) => clip_properties_window(view, ctx, svc, c, &mut open),
            Dialog::Export(c) => export_window(view, ctx, svc, ExportTarget::Clip((**c).clone()), &mut open),
            Dialog::TimelineExport { camera, start, end } => export_window(view, ctx, svc, ExportTarget::Range { camera: camera.clone(), start: *start, end: *end }, &mut open),
        }
        if open {
            view.dialogs.open[i] = d;
            i += 1;
        } else {
            view.dialogs.open.remove(i);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// UI Settings (generic renderer over UI3's settings table)

fn settings_window(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, open: &mut bool) {
    let sounds = view.session.login.sounds().to_vec();
    let admin = view.session.login.admin;
    w::window("UI Settings").open(open).default_size([720.0, 560.0]).resizable(true).show(ctx, |ui| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            w::paint_icon(ui, crate::icons::SEARCH, r, style::t().text);
            ui.add(egui::TextEdit::singleline(&mut view.dialogs.settings_filter).hint_text("Search settings").desired_width(260.0));
        });
        ui.add_space(6.0);
        egui::Panel::left("settings_cats").resizable(false).exact_size(200.0).frame(egui::Frame::new()).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                let th = style::t();
                for c in CATEGORIES.iter().copied().chain(std::iter::once("Native")) {
                    let selected = view.dialogs.settings_category.unwrap_or("General Settings") == c && view.dialogs.settings_filter.is_empty();
                    let (rect, r) = ui.allocate_exact_size(egui::vec2(190.0, 28.0), egui::Sense::click());
                    let fill = if selected { th.dialog_title_bg } else if r.hovered() { th.collapsible_hover } else { egui::Color32::TRANSPARENT };
                    ui.painter().rect_filled(rect, 0.0, fill);
                    let label = if c == "Native" { "Native (UI3 Native only)" } else { c };
                    let color = if selected { egui::Color32::from_rgb(0xCD, 0xCD, 0xCD) } else if r.hovered() { th.text_hover } else { egui::Color32::from_gray(0x9A) };
                    ui.painter().text(rect.left_center() + egui::vec2(8.0, 0.0), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(14.0), color);
                    if r.clicked() {
                        view.dialogs.settings_category = Some(c);
                        view.dialogs.settings_filter.clear();
                    }
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let filter = view.dialogs.settings_filter.to_ascii_lowercase();
                let cat = view.dialogs.settings_category.unwrap_or("General Settings");
                if filter.is_empty() && cat == "Native" {
                    native_settings(ui, svc);
                    return;
                }
                egui::Grid::new("settings_grid").num_columns(2).spacing([16.0, 8.0]).striped(true).show(ui, |ui| {
                    for d in DEFS.iter() {
                        if matches!(d.input, Input::Hidden) || d.category.is_empty() {
                            continue;
                        }
                        if !filter.is_empty() {
                            let hay = format!("{} {} {} {}", d.label, d.desc, d.hint, d.category).to_ascii_lowercase();
                            if !hay.contains(&filter) {
                                continue;
                            }
                        } else if d.category != cat {
                            continue;
                        }
                        setting_row(ui, svc, d, &sounds, admin, &mut view.dialogs.capturing_hotkey);
                        ui.end_row();
                    }
                });
            });
        });
    });
}

fn setting_row(ui: &mut egui::Ui, svc: &mut Services, d: &'static crate::settings::SettingDef, sounds: &[String], admin: bool, capturing: &mut Option<&'static str>) {
    let s = &mut *svc.settings;
    let label = if d.label.is_empty() { d.key } else { d.label };
    if let Input::Comment(text) = d.input {
        ui.label(RichText::new(text).italics().color(style::t().text));
        ui.label("");
        return;
    }
    // UI3 shows the description as a smaller second line (.settingDesc, 0.85em).
    let l = ui
        .vertical(|ui| {
            ui.set_width(260.0);
            ui.spacing_mut().item_spacing.y = 1.0;
            let l = ui.label(label);
            if !d.desc.is_empty() {
                ui.label(RichText::new(d.desc).size(12.0).color(style::t().text));
            }
            l
        })
        .inner;
    if !d.hint.is_empty() {
        l.on_hover_text(d.hint);
    }
    let key = d.key;
    match d.input {
        Input::Checkbox => {
            let mut b = s.bool(key);
            if ui.checkbox(&mut b, "").changed() {
                s.set_bool(key, b);
                apply_setting_side_effects(svc, key);
            }
        }
        Input::Select(opts, dynamic) => {
            let cur = s.str(key);
            let options: Vec<String> = match dynamic {
                Some("getBISoundOptions") => std::iter::once("None".to_string()).chain(sounds.iter().cloned()).collect(),
                Some("getSystemNameButtonOptions") => crate::views::topbar::system_name_button_options(admin),
                Some("GetSpeechVoiceOptions") => crate::events::speech_voices().to_vec(),
                Some("GetStatusAreaBarOptions") => crate::views::sidebar::STATUS_BAR_NAMES.iter().map(|n| n.to_string()).collect(),
                _ if opts.is_empty() => vec![cur.clone()],
                _ => opts.iter().map(|o| o.to_string()).collect(),
            };
            egui::ComboBox::from_id_salt(key).selected_text(&cur).width(240.0).show_ui(ui, |ui| {
                for o in &options {
                    if ui.selectable_label(*o == cur, o).clicked() {
                        s.set(key, o.clone());
                    }
                }
            });
        }
        Input::Number { min, max, step, unit } | Input::Range { min, max, step, unit } => {
            let mut v = s.f64(key);
            let r = if matches!(d.input, Input::Range { .. }) {
                ui.add(egui::Slider::new(&mut v, min..=max).step_by(step).suffix(format!(" {unit}")))
            } else {
                ui.add(egui::DragValue::new(&mut v).range(min..=max).speed(step.max(0.01)).suffix(format!(" {unit}")))
            };
            if r.changed() {
                let keep_str = matches!(s.raw(key), Value::String(_));
                if keep_str { s.set(key, format_num(v)) } else { s.set(key, serde_json::json!(v)) }
            }
        }
        Input::Color => {
            let mut c = style::hex(&s.str(key));
            if ui.color_edit_button_srgba(&mut c).changed() {
                s.set(key, format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b()));
            }
        }
        Input::Text | Input::Password => {
            let mut t = s.str(key);
            if ui.add(egui::TextEdit::singleline(&mut t).password(matches!(d.input, Input::Password)).desired_width(260.0)).changed() {
                s.set(key, t);
            }
        }
        Input::Hotkey => {
            ui.horizontal(|ui| {
                let cur = s.hotkey(key).map(|h| h.label()).unwrap_or_else(|| "unset".into());
                if *capturing == Some(key) {
                    ui.label(RichText::new("Press a key… (Esc to cancel, Backspace to clear)").color(style::t().highlight));
                    let ev = ui.input(|i| i.events.iter().find_map(|e| if let egui::Event::Key { key, pressed: true, modifiers, .. } = e { Some((*key, *modifiers)) } else { None }));
                    if let Some((k, m)) = ev {
                        match k {
                            egui::Key::Escape => {}
                            egui::Key::Backspace => s.set(key, ""),
                            _ => s.set(key, Hotkey { ctrl: m.ctrl, alt: m.alt, shift: m.shift, key: k }.to_ui3_string()),
                        }
                        *capturing = None;
                    }
                } else if ui.button(cur).clicked() {
                    *capturing = Some(key);
                }
                if ui.small_button("↺").on_hover_text("Reset to default").clicked() {
                    s.reset(key);
                }
            });
        }
        Input::Hidden | Input::Comment(_) => {}
    }
    let _ = Dv::Num(0.0);
}

fn format_num(v: f64) -> String {
    if v.fract() == 0.0 { format!("{}", v as i64) } else { format!("{v}") }
}

fn apply_setting_side_effects(svc: &mut Services, key: &str) {
    if key == "ui3_audioMute" {
        svc.audio.set_muted(svc.settings.bool(key));
    }
}

fn native_settings(ui: &mut egui::Ui, svc: &mut Services) {
    let zc_supported = svc.gpu.shared_d3d11.is_some();
    let n = &mut svc.settings.native;
    let mut changed = false;
    egui::Grid::new("native").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
        ui.label("Hardware video decoding").on_hover_text("D3D11VA uses the GPU's video decoder (NVDEC / Intel Quick Sync / AMD VCN) for H.264 and H.265.");
        egui::ComboBox::from_id_salt("hw").selected_text(&n.hw_decode).show_ui(ui, |ui| {
            for o in ["Auto", "D3D11VA", "Software"] {
                changed |= ui.selectable_value(&mut n.hw_decode, o.to_string(), o).changed();
            }
        });
        ui.end_row();
        ui.label("Zero-copy GPU rendering").on_hover_text("Draw hardware-decoded frames straight from the video decoder's GPU textures instead of copying each frame through system memory. Falls back automatically if the GPU cannot do it. Applies to streams opened after the change.");
        ui.horizontal(|ui| {
            changed |= ui.checkbox(&mut n.zero_copy, "").changed();
            let status = match (zc_supported, crate::player::zero_copy_device().is_some()) {
                (false, _) => "not supported by this GPU/driver",
                (true, true) => "active",
                (true, false) => "off",
            };
            ui.label(RichText::new(status).small().color(style::t().text));
        });
        ui.end_row();
        ui.label("Client-side group grid").on_hover_text("Open one direct H.264/H.265 stream per camera and compose groups locally, instead of the server-composited group image. Uses more bandwidth, no server transcoding.");
        changed |= ui.checkbox(&mut n.client_side_grid, "").changed();
        ui.end_row();
        ui.label("Grid uses sub streams");
        changed |= ui.checkbox(&mut n.grid_use_substream, "").changed();
        ui.end_row();
        ui.label("Scrub frame cache (frames)");
        changed |= ui.add(egui::DragValue::new(&mut n.scrub_cache_frames).range(16..=4000)).changed();
        ui.end_row();
        ui.label("Timeline drag inertia").on_hover_text("Fling the timeline and it keeps coasting, slowing to a stop. Slow drags stop where you release. Click the timeline to catch it.");
        changed |= ui.checkbox(&mut n.timeline_inertia, "").changed();
        ui.end_row();
        ui.label("Stats overlay");
        changed |= ui.checkbox(&mut n.show_stats_overlay, "").changed();
        ui.end_row();
    });
    ui.add_space(8.0);
    ui.label(RichText::new(format!("Settings file: {}", Settings::path().display())).small().color(style::t().text));
    ui.label(RichText::new(format!("FFmpeg libavcodec {}", bi_media::ffmpeg_version())).small().color(style::t().text));
    if changed {
        svc.settings.mark_dirty();
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Streaming profiles editor (`StreamingProfileUI` / `StreamingProfileEditor`, ui3.js:25644-25789)

fn profiles_window(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, open: &mut bool) {
    let mut reload = false;
    w::window("Streaming Profiles").open(open).default_size([680.0, 520.0]).show(ctx, |ui| {
        let profiles = &mut svc.settings.streaming_profiles;
        let sel = &mut view.dialogs.profile_sel;
        if *sel >= profiles.len() {
            *sel = 0;
        }
        let mut action: Option<&str> = None;
        ui.horizontal(|ui| {
            for a in ["Add", "Clone", "Delete", "Move up", "Move down", "Restore defaults"] {
                if ui.button(a).clicked() {
                    action = Some(a);
                }
            }
        });
        match action {
            Some("Add") => {
                profiles.push(StreamingProfile::default());
                *sel = profiles.len() - 1;
            }
            Some("Clone") if !profiles.is_empty() => {
                let mut p = profiles[*sel].clone();
                p.name = format!("{} (copy)", p.display_name());
                profiles.insert(*sel + 1, p);
                *sel += 1;
            }
            Some("Delete") if profiles.len() > 1 => {
                profiles.remove(*sel);
                *sel = sel.saturating_sub(1);
            }
            Some("Move up") if *sel > 0 => {
                profiles.swap(*sel, *sel - 1);
                *sel -= 1;
            }
            Some("Move down") if *sel + 1 < profiles.len() => {
                profiles.swap(*sel, *sel + 1);
                *sel += 1;
            }
            Some("Restore defaults") => {
                *profiles = crate::settings::default_profiles();
                *sel = 0;
            }
            _ => {}
        }
        ui.separator();
        ui.columns(2, |cols| {
            egui::ScrollArea::vertical().id_salt("plist").show(&mut cols[0], |ui| {
                for (i, p) in profiles.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&p.abbr).small().color(Color32::WHITE).background_color(style::hex(&p.abbr_color)));
                        if ui.selectable_label(i == *sel, p.display_name()).on_hover_text(p.tooltip()).clicked() {
                            *sel = i;
                        }
                    });
                }
            });
            let ui = &mut cols[1];
            if let Some(p) = profiles.get_mut(*sel) {
                let before = p.clone();
                egui::Grid::new("pedit").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut p.name);
                    ui.end_row();
                    ui.label("Abbreviation");
                    ui.text_edit_singleline(&mut p.abbr);
                    ui.end_row();
                    ui.label("Abbr. color");
                    let mut c = style::hex(&p.abbr_color);
                    if ui.color_edit_button_srgba(&mut c).changed() {
                        p.abbr_color = format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b());
                    }
                    ui.end_row();
                    ui.label("Video codec family");
                    egui::ComboBox::from_id_salt("vc").selected_text(if p.vcodec == VCodec::H264 { "H.264 or H.265" } else { "JPEG" }).show_ui(ui, |ui| {
                        ui.selectable_value(&mut p.vcodec, VCodec::H264, "H.264 or H.265");
                        ui.selectable_value(&mut p.vcodec, VCodec::Jpeg, "JPEG");
                    });
                    ui.end_row();
                    ui.label("Base stream");
                    egui::ComboBox::from_id_salt("bs").selected_text(format!("Streaming {}", p.stream)).show_ui(ui, |ui| {
                        for (i, st) in view.session.login.server_streams().iter().enumerate() {
                            ui.selectable_value(&mut p.stream, i as i32, format!("Streaming {i}: {}", st.text));
                        }
                    });
                    ui.end_row();
                    ui.label("Max width / height");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut p.w).range(-1..=99999));
                        ui.add(egui::DragValue::new(&mut p.h).range(-1..=99999));
                    });
                    ui.end_row();
                    ui.label("Quality % (BI6)").on_hover_text(format!("H.264 QP {} / H.265 QP {}", bi_proto::profile::quality::bi6_q_to_h264_qp(p.q6.max(0)), bi_proto::profile::quality::bi6_q_to_h265_qp(p.q6.max(0))));
                    ui.add(egui::Slider::new(&mut p.q6, -1..=100));
                    ui.end_row();
                    if p.vcodec == VCodec::H264 {
                        ui.label("Preferred codec").on_hover_text("Used when transcoding is required; Direct-to-wire sends the camera's own codec.");
                        egui::ComboBox::from_id_salt("pc").selected_text(format!("{:?}", p.codec_pref)).show_ui(ui, |ui| {
                            ui.selectable_value(&mut p.codec_pref, CodecPref::Inherit, "Inherit");
                            ui.selectable_value(&mut p.codec_pref, CodecPref::H264, "H.264");
                            ui.selectable_value(&mut p.codec_pref, CodecPref::H265, "H.265");
                        });
                        ui.end_row();
                        ui.label("Rate control");
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut p.rc, RateControl::Vbr, "VBR");
                            ui.selectable_value(&mut p.rc, RateControl::Cbr, "CBR");
                        });
                        ui.end_row();
                        ui.label("Kbps limit");
                        ui.add(egui::DragValue::new(&mut p.kbps).range(-1..=100000));
                        ui.end_row();
                        ui.label("FPS (-1 inherit)");
                        ui.add(egui::DragValue::new(&mut p.fps).range(-1..=120));
                        ui.end_row();
                        ui.label("Keyframe interval (GOP)");
                        ui.add(egui::DragValue::new(&mut p.gop).range(-1..=99999));
                        ui.end_row();
                        tri(ui, "Zero frame latency", &mut p.zfl);
                        tri(ui, "Direct to wire", &mut p.direct_to_wire);
                        tri(ui, "Full range color", &mut p.full_range_color);
                        ui.label("Encoder preset");
                        egui::ComboBox::from_id_salt("pre").selected_text(["inherit", "ultrafast", "superfast", "veryfast"][p.preset.clamp(0, 3) as usize]).show_ui(ui, |ui| {
                            for (i, n) in ["inherit", "ultrafast", "superfast", "veryfast"].iter().enumerate() {
                                ui.selectable_value(&mut p.preset, i as i32, *n);
                            }
                        });
                        ui.end_row();
                        ui.label("Encoder profile");
                        let names = ["inherit", "default", "baseline", "main", "extended", "high", "high 10"];
                        egui::ComboBox::from_id_salt("pro").selected_text(names[p.profile.clamp(0, 6) as usize]).show_ui(ui, |ui| {
                            for (i, n) in names.iter().enumerate() {
                                ui.selectable_value(&mut p.profile, i as i32, *n);
                            }
                        });
                        ui.end_row();
                    }
                });
                if *p != before {
                    reload = true;
                }
            }
        });
    });
    if reload {
        svc.settings.mark_dirty();
    }
}

fn tri(ui: &mut egui::Ui, label: &str, v: &mut i32) {
    ui.label(label);
    ui.horizontal(|ui| {
        ui.selectable_value(v, 0, "Inherit");
        ui.selectable_value(v, 1, "Off");
        ui.selectable_value(v, 2, "On");
    });
    ui.end_row();
}

// ---------------------------------------------------------------------------------------------------------------

fn camera_list_window(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, open: &mut bool) {
    let mut load = None;
    w::window("Camera List").open(open).default_size([820.0, 480.0]).show(ctx, |ui| {
        egui::ScrollArea::both().show(ui, |ui| {
            egui::Grid::new("camlist").striped(true).num_columns(9).spacing([12.0, 4.0]).show(ui, |ui| {
                for h in ["", "Name", "Short name", "Status", "Resolution", "FPS", "Kbps", "Alerts / Clips", ""] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for c in view.cameras.iter().filter(|c| !c.is_group() && !c.is_cycle()) {
                    ui.label(RichText::new("■").color(style::rgb3(c.color_rgb())));
                    ui.label(&c.name);
                    ui.label(&c.id);
                    let st = if !c.is_enabled {
                        "Disabled"
                    } else if c.is_no_signal {
                        "No signal"
                    } else if c.is_paused {
                        "Paused"
                    } else if c.is_triggered {
                        "Triggered"
                    } else if c.is_recording {
                        "Recording"
                    } else {
                        "OK"
                    };
                    ui.label(st);
                    ui.label(format!("{}x{}", c.width, c.height));
                    ui.label(format!("{:.1}", c.fps));
                    ui.label(format!("{:.0}", c.bps / 1000.0));
                    ui.label(format!("{} / {}", c.n_alerts, c.n_clips));
                    if ui.small_button("View").clicked() {
                        load = Some(c.id.clone());
                    }
                    ui.end_row();
                }
            });
        });
    });
    if let Some(id) = load {
        view.select_source(ctx, svc, &id);
    }
}

fn camera_properties_window(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, id: &str, open: &mut bool) {
    // `CameraProperties` (ui3.js:29109) — camconfig get + toggles.
    if view.dialogs.camcfg.as_ref().is_none_or(|(c, _)| c != id) && view.dialogs.camcfg_job.is_none() {
        let (c, cam) = (view.client.clone(), id.to_string());
        view.dialogs.camcfg_job = Some(runtime::spawn(ctx, async move { c.camconfig_get(&cam).await }));
    }
    if let Some(j) = &mut view.dialogs.camcfg_job
        && let Some(r) = j.poll()
    {
        view.dialogs.camcfg_job = None;
        match r {
            Ok(v) => view.dialogs.camcfg = Some((id.to_string(), v)),
            Err(e) => svc.toasts.error(format!("camconfig: {e:#}")),
        }
    }
    let name = view.camera(id).map(|c| c.name.clone()).unwrap_or_else(|| id.to_string());
    let mut change: Option<CamConfig> = None;
    w::window(format!("Camera Properties — {name}")).open(open).default_size([460.0, 520.0]).show(ctx, |ui| {
        let Some((_, cfg)) = &view.dialogs.camcfg else {
            ui.spinner();
            return;
        };
        let b = |k: &str| bi_proto::lenient::value_to_bool(&cfg[k]);
        egui::Grid::new("camprops").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            let toggles: [(&str, &str, fn(bool) -> CamConfig); 7] = [
                ("Motion detector", "motion", CamConfig::Motion),
                ("Override global schedule", "schedule", CamConfig::Schedule),
                ("PTZ preset cycle", "ptzcycle", CamConfig::PtzCycle),
                ("PTZ event schedule", "ptzevents", CamConfig::PtzEvents),
                ("Push notifications", "push", CamConfig::Push),
                ("DIO output", "output", CamConfig::Output),
                ("Manual recording", "manrec", CamConfig::ManualRecord),
            ];
            for (label, key, ctor) in toggles {
                if cfg.get(key).is_none() {
                    continue;
                }
                ui.label(label);
                let mut v = b(key);
                if ui.checkbox(&mut v, "").changed() {
                    change = Some(ctor(v));
                }
                ui.end_row();
            }
            for (label, key) in [("Record", "record"), ("Alerts", "alerts")] {
                if let Some(v) = cfg.get(key) {
                    ui.label(label);
                    let cur = bi_proto::lenient::value_to_f64(v).unwrap_or(0.0) as i64;
                    let opts: &[(i64, &str)] = if key == "record" {
                        &[(-1, "Default"), (0, "Never"), (1, "When triggered"), (2, "Continuous + triggered"), (3, "Continuous")]
                    } else {
                        &[(-1, "Default"), (0, "Never"), (1, "On motion"), (2, "On trigger")]
                    };
                    egui::ComboBox::from_id_salt(key).selected_text(opts.iter().find(|o| o.0 == cur).map(|o| o.1).unwrap_or("?")).show_ui(ui, |ui| {
                        for (v, n) in opts {
                            if ui.selectable_label(*v == cur, *n).clicked() {
                                change = Some(if key == "record" { CamConfig::Record(*v) } else { CamConfig::Alerts(*v) });
                            }
                        }
                    });
                    ui.end_row();
                }
            }
        });
        if let Some(m) = cfg.get("setmotion").and_then(|v| v.as_object()) {
            ui.separator();
            ui.label(RichText::new("Motion sensor").strong());
            egui::Grid::new("motion").num_columns(2).show(ui, |ui| {
                for (k, v) in m {
                    ui.label(k);
                    ui.label(v.to_string());
                    ui.end_row();
                }
            });
        }
        ui.separator();
        ui.collapsing("Raw configuration", |ui| {
            ui.monospace(serde_json::to_string_pretty(cfg).unwrap_or_default());
        });
    });
    if let Some(ch) = change {
        let (c, cam) = (view.client.clone(), id.to_string());
        view.dialogs.camcfg = None;
        view.dialogs.download_jobs.push(runtime::spawn(ctx, async move {
            c.camconfig(&cam, ch).await?;
            Ok("Camera setting updated".to_string())
        }));
    }
}

fn camera_pause_window(view: &mut MainView, ctx: &egui::Context, id: &str, open: &mut bool) {
    // `CameraPauseDialog` (ui3.js:31316): pause indefinitely, add time, or resume.
    let mut act: Option<i64> = None;
    w::window(format!("Pause {}", view.camera(id).map(|c| c.name.clone()).unwrap_or_default())).open(open).collapsible(false).show(ctx, |ui| {
        if let Some(c) = view.camera(id) {
            ui.label(if c.is_paused { format!("Paused ({} s remaining)", c.pause.max(0)) } else { "Not paused".into() });
        }
        ui.horizontal(|ui| {
            for (s, l) in [(30, "+30 s"), (300, "+5 min"), (1800, "+30 min"), (3600, "+1 hr"), (10800, "+3 hr")] {
                if ui.button(l).clicked() {
                    act = Some(s);
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Pause indefinitely").clicked() {
                act = Some(-1);
            }
            if ui.button("Resume").clicked() {
                act = Some(0);
            }
        });
    });
    if let Some(v) = act {
        view.dialogs.pause_secs = v;
        let (c, cam) = (view.client.clone(), id.to_string());
        view.dialogs.download_jobs.push(runtime::spawn(ctx, async move {
            c.camconfig(&cam, CamConfig::Pause(v)).await?;
            Ok(if v == 0 { "Camera resumed".to_string() } else { "Camera paused".to_string() })
        }));
    }
}

fn server_status_window(view: &mut MainView, ctx: &egui::Context, open: &mut bool) {
    w::window("Server Status / Disk Usage").open(open).default_size([520.0, 400.0]).show(ctx, |ui| {
        let Some(st) = &view.status else {
            ui.spinner();
            return;
        };
        let max_h = ui.ctx().content_rect().height() * 0.75;
        egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
        egui::Grid::new("st").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            for (k, v) in [("CPU", st.cpu.clone()), ("Memory", format!("{} ({})", st.mem, st.memload)), ("Physical memory", st.memphys.clone()), ("Uptime", st.uptime.clone()), ("Clips", st.clips.clone()), ("Warnings", st.warnings.to_string()), ("Alerts", st.alerts.to_string())] {
                ui.label(k);
                ui.label(v);
                ui.end_row();
            }
        });
        ui.separator();
        for d in &st.disks {
            ui.label(RichText::new(&d.disk).strong());
            let frac = if d.total > 0.0 { (d.total - d.free) / d.total } else { 0.0 };
            w::flat_bar(ui, frac as f32, &format!("Disk: {:.1} GB used of {:.1} GB ({:.1} GB free)", (d.total - d.free) / 1024.0, d.total / 1024.0, d.free / 1024.0), 460.0);
            w::flat_bar(ui, if d.allocated > 0.0 { (d.used / d.allocated) as f32 } else { 0.0 }, &format!("Blue Iris: {:.1} GB used of {:.1} GB allocated", d.used / 1024.0, d.allocated / 1024.0), 460.0);
            ui.add_space(4.0);
        }
        if let Some(f) = st.extra.get("folders").and_then(|f| f.as_array()) {
            ui.separator();
            ui.label(RichText::new("Folders").strong());
            for f in f {
                ui.label(format!("{}: {} clips, {} / {} MB", f["name"].as_str().unwrap_or("?"), f["clips"], f["used"], f["allocated"]));
            }
        }
        });
    });
}

/// Generic window that fetches a JSON command and shows the result as a table (log, users, devices, updatecheck).
fn json_window(view: &mut MainView, ctx: &egui::Context, title: &str, cmd: &str, open: &mut bool) {
    let fresh = view.dialogs.json_cache.get(cmd).is_some_and(|(_, t)| t.elapsed() < Duration::from_secs(10));
    if !fresh && view.dialogs.json_job.is_none() {
        let c = view.client.clone();
        let cmd2 = cmd.to_string();
        view.dialogs.json_job = Some((cmd.to_string(), runtime::spawn(ctx, async move { c.json_ok(serde_json::json!({ "cmd": cmd2 })).await })));
    }
    if let Some((k, j)) = &mut view.dialogs.json_job
        && let Some(r) = j.poll()
    {
        let k = k.clone();
        view.dialogs.json_job = None;
        let v = r.unwrap_or_else(|e| serde_json::json!({ "error": format!("{e:#}") }));
        view.dialogs.json_cache.insert(k, (v, Instant::now()));
    }
    w::window(title).open(open).default_size([760.0, 460.0]).show(ctx, |ui| {
        let Some((v, _)) = view.dialogs.json_cache.get(cmd) else {
            ui.spinner();
            return;
        };
        json_table(ui, v, cmd);
    });
}

fn json_table(ui: &mut egui::Ui, v: &Value, id: &str) {
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| match v {
        Value::Array(rows) if rows.iter().all(|r| r.is_object()) && !rows.is_empty() => {
            let mut cols: Vec<String> = Vec::new();
            for r in rows {
                for k in r.as_object().unwrap().keys() {
                    if !cols.contains(k) {
                        cols.push(k.clone());
                    }
                }
            }
            egui::Grid::new(id).striped(true).num_columns(cols.len()).spacing([12.0, 3.0]).show(ui, |ui| {
                for c in &cols {
                    ui.label(RichText::new(c).strong());
                }
                ui.end_row();
                for r in rows {
                    for c in &cols {
                        let cell = match &r[c] {
                            Value::String(s) => s.clone(),
                            Value::Null => String::new(),
                            x => x.to_string(),
                        };
                        ui.label(cell);
                    }
                    ui.end_row();
                }
            });
        }
        other => {
            ui.monospace(serde_json::to_string_pretty(other).unwrap_or_default());
        }
    });
}

fn export_list_window(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, open: &mut bool) {
    let fresh = view.dialogs.json_cache.get("export").is_some_and(|(_, t)| t.elapsed() < Duration::from_secs(4));
    if !fresh && view.dialogs.json_job.is_none() {
        let c = view.client.clone();
        view.dialogs.json_job = Some(("export".into(), runtime::spawn(ctx, async move { c.export_list().await })));
    }
    if let Some((k, j)) = &mut view.dialogs.json_job
        && let Some(r) = j.poll()
    {
        let k = k.clone();
        view.dialogs.json_job = None;
        if let Ok(v) = r {
            view.dialogs.json_cache.insert(k, (v, Instant::now()));
        }
    }
    let mut delete = None;
    let mut download = None;
    w::window("Convert/Export Queue").open(open).default_size([620.0, 420.0]).show(ctx, |ui| {
        ui.label("Finished exports are accessible in Clips > Clipboard. Click a queued item's ✖ to cancel it.");
        ui.separator();
        let Some((Value::Array(items), _)) = view.dialogs.json_cache.get("export") else {
            ui.spinner();
            return;
        };
        if items.is_empty() {
            ui.label("The queue is empty.");
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for it in items.iter().rev() {
                let path = it["path"].as_str().unwrap_or("").to_string();
                let status = it["status"].as_str().unwrap_or("");
                ui.horizontal(|ui| {
                    let tz = view.session.login.tz_offset_ms();
                    let when = it["utc"].as_i64().or_else(|| it["utc"].as_str().and_then(|s| s.parse().ok())).map(|u| crate::views::video::fmt_utc(u, tz)).unwrap_or_default();
                    ui.label(format!("{when}  {}", it["uri"].as_str().unwrap_or(&path)));
                    match status {
                        "active" => {
                            {
                                let pr = it["progress"].as_f64().unwrap_or(0.0);
                                w::flat_bar(ui, (pr / 100.0) as f32, &format!("{pr:.0}%"), 120.0);
                            }
                        }
                        "error" => {
                            ui.colored_label(Color32::LIGHT_RED, format!("Error: {}", it["error"].as_str().unwrap_or("")));
                        }
                        s => {
                            ui.label(s);
                        }
                    }
                    if let Some(sz) = it["filesize"].as_str() {
                        ui.label(RichText::new(sz).color(style::t().text));
                    }
                    if status == "done" && ui.small_button("⬇ Download").clicked() {
                        download = Some((path.clone(), it["uri"].as_str().unwrap_or("export.mp4").rsplit(['\\', '/']).next().unwrap_or("export.mp4").to_string()));
                    }
                    if ui.small_button("✖").on_hover_text("Remove from queue").clicked() {
                        delete = Some(path.clone());
                    }
                });
            }
        });
    });
    if let Some(p) = delete {
        let c = view.client.clone();
        view.dialogs.json_cache.remove("export");
        view.dialogs.download_jobs.push(runtime::spawn(ctx, async move {
            c.export_delete(&p).await?;
            Ok("Removed from the export queue".to_string())
        }));
    }
    if let Some((p, name)) = download {
        start_download(view, bi_proto::urls::clip_download(&p, false), name);
    }
    ctx.request_repaint_after(Duration::from_secs(1));
    let _ = svc;
}

fn server_control_window(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, open: &mut bool) {
    // `ServerControl` (ui3.js:31720): sysconfig archive/schedule/manrecsec and reboot.
    let fresh = view.dialogs.json_cache.get("sysconfig").is_some_and(|(_, t)| t.elapsed() < Duration::from_secs(30));
    if !fresh && view.dialogs.json_job.is_none() {
        let c = view.client.clone();
        view.dialogs.json_job = Some(("sysconfig".into(), runtime::spawn(ctx, async move { c.sysconfig(None).await })));
    }
    if let Some((k, j)) = &mut view.dialogs.json_job
        && let Some(r) = j.poll()
    {
        let k = k.clone();
        view.dialogs.json_job = None;
        if let Ok(v) = r {
            view.dialogs.json_cache.insert(k, (v, Instant::now()));
        }
    }
    let mut set: Option<(String, Value)> = None;
    let mut reboot = false;
    w::window("Server Control").open(open).default_size([420.0, 260.0]).show(ctx, |ui| {
        let Some((cfg, _)) = view.dialogs.json_cache.get("sysconfig") else {
            ui.spinner();
            return;
        };
        for (label, key) in [("Web archival (FTP backup)", "archive"), ("Global schedule enabled", "schedule")] {
            if cfg.get(key).is_some() {
                let mut b = bi_proto::lenient::value_to_bool(&cfg[key]);
                if ui.checkbox(&mut b, label).changed() {
                    set = Some((key.to_string(), b.into()));
                }
            }
        }
        if let Some(v) = cfg.get("manrecsec") {
            let mut n = bi_proto::lenient::value_to_f64(v).unwrap_or(0.0) as i64;
            ui.horizontal(|ui| {
                ui.label("Manual recording limit (seconds)");
                if ui.add(egui::DragValue::new(&mut n).range(0..=86400)).lost_focus() {
                    set = Some(("manrecsec".into(), n.into()));
                }
            });
        }
        ui.separator();
        if ui.button(RichText::new("⏻ Restart Blue Iris server").color(Color32::LIGHT_RED)).clicked() {
            reboot = true;
        }
    });
    if let Some((k, v)) = set {
        let c = view.client.clone();
        view.dialogs.json_cache.remove("sysconfig");
        view.dialogs.download_jobs.push(runtime::spawn(ctx, async move {
            let mut m = serde_json::Map::new();
            m.insert(k, v);
            c.sysconfig(Some(m)).await?;
            Ok("Server setting updated".to_string())
        }));
    }
    if reboot {
        let c = view.client.clone();
        view.dialogs.download_jobs.push(runtime::spawn(ctx, async move {
            c.reboot_server().await?;
            Ok("Blue Iris restart requested".to_string())
        }));
    }
    let _ = svc;
}

fn about_window(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, open: &mut bool) {
    w::window("About UI3 Native").open(open).collapsible(false).resizable(false).show(ctx, |ui| {
        ui.heading(format!("UI3 Native {}", env!("CARGO_PKG_VERSION")));
        ui.label("A native Windows port of Blue Iris UI3 (bp2008/ui3) in Rust.");
        ui.label("Direct H.264 / H.265 playback with hardware decoding (FFmpeg D3D11VA).");
        ui.separator();
        let l = &view.session.login;
        ui.label(format!("Server: {} — Blue Iris {}", l.system_name, l.version));
        ui.label(format!("User: {} ({})", if l.user.is_empty() { "-" } else { &l.user }, if l.admin { "Administrator" } else { "Limited user" }));
        ui.label(format!("GPU: {} ({})", svc.gpu.adapter_name, svc.gpu.backend));
        ui.label(format!("FFmpeg libavcodec {} (LGPL, dynamically linked)", bi_media::ffmpeg_version()));
        ui.label(format!("Audio output: {} Hz", svc.audio.device_rate()));
    });
}

fn clip_properties_window(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, c: &ClipItem, open: &mut bool) {
    let tz = view.session.login.tz_offset_ms();
    w::window("Clip Properties").open(open).default_size([440.0, 360.0]).show(ctx, |ui| {
        egui::Grid::new("clipprops").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            let names: Vec<&str> = [
                ("AUDIO", flags::AUDIO),
                ("FLAGGED", flags::FLAGGED),
                ("PROTECTED", flags::PROTECTED),
                ("ARCHIVED", flags::ARCHIVED),
                ("RECORDING", flags::RECORDING),
                ("AI_PERSON", flags::AI_PERSON),
                ("AI_VEHICLE", flags::AI_VEHICLE),
                ("AI_WILDLIFE", flags::AI_WILDLIFE),
                ("AI_CONFIRMED", flags::AI_CONFIRMED),
                ("ALERT_MOTION", flags::ALERT_MOTION),
                ("ALERT_EXTERNAL", flags::ALERT_EXTERNAL),
                ("ALERT_ONVIF", flags::ALERT_ONVIF),
                ("ALERT_AUDIO", flags::ALERT_AUDIO),
                ("ALERT_CANCELLED", flags::ALERT_CANCELLED),
                ("ALERT_OFFSETTIME", flags::ALERT_OFFSETTIME),
            ]
            .iter()
            .filter(|(_, f)| c.has_flag(*f))
            .map(|(n, _)| *n)
            .collect();
            for (k, v) in [
                ("Camera", view.camera(&c.camera).map(|x| x.name.clone()).unwrap_or(c.camera.clone())),
                ("Date", crate::views::video::fmt_utc(c.date * 1000, tz)),
                ("Type", if c.is_clip() { "Clip".into() } else { "Alert".into() }),
                ("Path", c.path.clone()),
                ("Clip", c.clip.clone().unwrap_or_default()),
                ("Offset", format!("{} ms", c.offset)),
                ("Duration", format!("{} ms", c.duration_ms())),
                ("Size", c.filesize.clone()),
                ("File type", c.filetype.clone()),
                ("Resolution", c.res.clone()),
                ("Memo", c.memo.clone()),
                ("Plate", c.plate.clone()),
                ("Flags", format!("{} ({})", c.flags, names.join(", "))),
            ] {
                ui.label(RichText::new(k).strong());
                ui.label(v);
                ui.end_row();
            }
        });
        if ui.button("⬇ Download").clicked() {
            download_clips(view, svc, vec![c.clone()]);
        }
    });
}

// ---------------------------------------------------------------------------------------------------------------
// Export (`ClipExportPanel`, ui3.js:30042) + native client-side MP4 export (supports H.265, unlike UI3's AVI exporter)

enum ExportTarget {
    Clip(ClipItem),
    Range { camera: String, start: i64, end: i64 },
}

#[derive(Clone)]
struct ExportOpts {
    format: i64,
    profile: i64,
    substream: i64,
    audio: bool,
    gain: f64,
    reencode: bool,
    overlay: bool,
    motion: bool,
    timelapse: bool,
    tl_mult: f64,
    tl_fps: f64,
    client_side: bool,
}

struct ClientExport {
    job: Pending<Result<String>>,
    progress: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

fn export_window(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, target: ExportTarget, open: &mut bool) {
    let s = &*svc.settings;
    let is_range = matches!(target, ExportTarget::Range { .. });
    let opts = view.dialogs.export_opts.get_or_insert_with(|| ExportOpts {
        format: if is_range { 1 } else { s.f64("ui3_clip_export_format2") as i64 },
        profile: s.f64("ui3_clip_export_profile") as i64,
        substream: s.f64("ui3_clip_export_substream").clamp(0.0, 2.0) as i64,
        audio: s.bool("ui3_clip_export_withAudio"),
        gain: s.f64("ui3_clip_export_gain").clamp(-12.8, 12.7),
        reencode: is_range || s.bool("ui3_clip_export_reencode"),
        overlay: s.bool("ui3_clip_export_overlay"),
        motion: s.bool("ui3_clip_export_motion"),
        timelapse: s.bool("ui3_clip_export_timelapse"),
        tl_mult: s.f64("ui3_clip_export_timelapseMultiplier").max(1.0),
        tl_fps: s.f64("ui3_clip_export_timelapseFps").max(1.0),
        client_side: false,
    });
    let mut go = false;
    let title = if is_range { "Export timeline segment" } else { "Convert / Export clip" };
    w::window(title).open(open).collapsible(false).default_width(420.0).show(ctx, |ui| {
        ui.checkbox(&mut opts.client_side, "Save the original stream directly (client-side MP4, keeps H.265, no re-encode)");
        ui.separator();
        ui.add_enabled_ui(!opts.client_side, |ui| {
            egui::Grid::new("exp").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Format");
                let names = ["AVI", "MP4 (H.264, H.265, MPEG4 only)", "Windows Media", "BVR (clipboard reference only)"];
                ui.add_enabled_ui(!is_range, |ui| {
                    egui::ComboBox::from_id_salt("fmt").selected_text(names[opts.format.clamp(0, 3) as usize]).show_ui(ui, |ui| {
                        for (i, n) in names.iter().enumerate() {
                            ui.selectable_value(&mut opts.format, i as i64, *n);
                        }
                    });
                });
                ui.end_row();
                if opts.format != 3 {
                    ui.label("Encoding profile");
                    ui.add(egui::DragValue::new(&mut opts.profile).range(0..=7));
                    ui.end_row();
                    ui.label("Stream");
                    egui::ComboBox::from_id_salt("sub").selected_text(["Auto main/sub stream", "Sub stream", "Main stream"][opts.substream as usize]).show_ui(ui, |ui| {
                        for (i, n) in ["Auto main/sub stream", "Sub stream", "Main stream"].iter().enumerate() {
                            ui.selectable_value(&mut opts.substream, i as i64, *n);
                        }
                    });
                    ui.end_row();
                    ui.label("Include audio");
                    ui.checkbox(&mut opts.audio, "");
                    ui.end_row();
                    if opts.audio && !is_range {
                        ui.label("Audio gain (dB)");
                        ui.add(egui::Slider::new(&mut opts.gain, -12.8..=12.7));
                        ui.end_row();
                    }
                    ui.label("Re-encode");
                    ui.add_enabled(!is_range, egui::Checkbox::new(&mut opts.reencode, ""));
                    ui.end_row();
                    if opts.reencode {
                        ui.label("Text/graphic overlays");
                        ui.checkbox(&mut opts.overlay, "");
                        ui.end_row();
                        ui.label("Motion overlay");
                        ui.checkbox(&mut opts.motion, "");
                        ui.end_row();
                        ui.label("Time-lapse");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut opts.timelapse, "");
                            ui.add_enabled(opts.timelapse, egui::DragValue::new(&mut opts.tl_mult).prefix("×").range(1.0..=10000.0));
                            ui.add_enabled(opts.timelapse, egui::DragValue::new(&mut opts.tl_fps).suffix(" fps").range(1.0..=60.0));
                        });
                        ui.end_row();
                    }
                }
            });
        });
        if let Some(ce) = &view.dialogs.client_export {
            let p = ce.progress.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0;
            w::flat_bar(ui, p, &format!("{:.0}%", p * 100.0), 360.0);
        }
        ui.horizontal(|ui| {
            let busy = view.dialogs.export_job.is_some() || view.dialogs.client_export.is_some();
            if ui.add_enabled(!busy, egui::Button::new("Export")).clicked() {
                go = true;
            }
            if busy {
                ui.spinner();
            }
        });
    });
    let opts = view.dialogs.export_opts.clone().unwrap();
    if go {
        // Persist choices like UI3.
        let st = &mut *svc.settings;
        if !is_range {
            st.set("ui3_clip_export_format2", opts.format.to_string());
        }
        st.set("ui3_clip_export_profile", opts.profile.to_string());
        st.set("ui3_clip_export_substream", opts.substream.to_string());
        st.set_bool("ui3_clip_export_withAudio", opts.audio);
        st.set_bool("ui3_clip_export_overlay", opts.overlay);
        st.set_bool("ui3_clip_export_motion", opts.motion);
        st.set_bool("ui3_clip_export_timelapse", opts.timelapse);
        let (path, start, msec, camera) = match &target {
            ExportTarget::Clip(c) => {
                let (s, m) = if let Some((a, b)) = (view.playback.export_start, view.playback.export_end).into_option() {
                    (a.min(b), (a - b).abs())
                } else if c.is_clip() {
                    (0, c.duration_ms())
                } else if c.has_flag(flags::ALERT_OFFSETTIME) {
                    (c.offset, c.rough_length_ms())
                } else {
                    (0, c.duration_ms())
                };
                (c.video_path().to_string(), s, m, c.camera.clone())
            }
            ExportTarget::Range { camera, start, end } => (camera.clone(), *start, end - start, camera.clone()),
        };
        if opts.client_side {
            start_client_export(view, ctx, &target, start, msec, &camera);
        } else {
            let req = ExportRequest {
                path,
                start_ms: Some(start),
                msec: Some(msec),
                format: Some(opts.format),
                profile: (opts.format != 3).then_some(opts.profile),
                substream: (opts.format != 3).then_some(opts.substream),
                audio: (opts.format != 3).then_some(opts.audio),
                gain: (opts.format != 3 && opts.audio && opts.gain != 0.0 && !is_range).then(|| (opts.gain * 10.0).round() as i64),
                reencode: (opts.format != 3).then_some(opts.reencode),
                overlay: (opts.format != 3).then_some(opts.overlay),
                motion: (opts.format != 3).then_some(opts.motion),
                timelapse: (opts.timelapse && opts.format != 3).then(|| format!("{}@{}", opts.tl_mult, opts.tl_fps)),
            };
            let c = view.client.clone();
            view.dialogs.export_job = Some(runtime::spawn(ctx, async move {
                c.export(&req).await?;
                Ok("Export queued. See the Convert/Export queue.".to_string())
            }));
        }
    }
    if let Some(j) = &mut view.dialogs.export_job
        && let Some(r) = j.poll()
    {
        view.dialogs.export_job = None;
        match r {
            Ok(m) => {
                svc.toasts.success(m);
                view.dialogs.open(Dialog::ExportList);
            }
            Err(e) => svc.toasts.error(format!("Export command failed: {e:#}")),
        }
    }
    if let Some(ce) = &mut view.dialogs.client_export {
        ctx.request_repaint_after(Duration::from_millis(200));
        if let Some(r) = ce.job.poll() {
            view.dialogs.client_export = None;
            match r {
                Ok(m) => svc.toasts.success(m),
                Err(e) => svc.toasts.error(format!("Export failed: {e:#}")),
            }
        }
    }
}

trait PairOpt<T> {
    fn into_option(self) -> Option<(T, T)>;
}
impl<T> PairOpt<T> for (Option<T>, Option<T>) {
    fn into_option(self) -> Option<(T, T)> {
        match self {
            (Some(a), Some(b)) => Some((a, b)),
            _ => None,
        }
    }
}

fn start_client_export(view: &mut MainView, ctx: &egui::Context, target: &ExportTarget, start: i64, msec: i64, camera: &str) {
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let Some(dest) = rfd::FileDialog::new().set_file_name(format!("{camera}_{stamp}.mp4")).add_filter("MP4", &["mp4"]).save_file() else { return };
    let path = match target {
        ExportTarget::Clip(c) => bi_proto::urls::clip_video(c.video_path(), bi_proto::urls::ClipStart::Time(start), 100, bi_proto::urls::AudioRequest::None, "&stream=0", "", ""),
        ExportTarget::Range { camera, start, .. } => {
            bi_proto::urls::timeline_video(camera, view.client.opaque(), *start, 0, bi_proto::urls::AudioRequest::None, "&stream=0", "", 100, Some(false), "", "")
        }
    };
    // Request the original stream: direct-to-wire with both codecs accepted.
    let path = format!("{path}&d2w=1&vcs=3");
    let progress = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let p2 = progress.clone();
    let c = view.client.clone();
    let job = runtime::spawn(ctx, async move { crate::views::export::export_stream_to_mp4(c, path, dest, msec, p2).await });
    view.dialogs.client_export = Some(ClientExport { job, progress });
}

/// Dismisses the live-camera context menu (an `egui::Area`, so `ui.close()` has no effect on it).
fn close_ctx_menu(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<(String, egui::Pos2)>(egui::Id::new("ctxcam")));
}

#[cfg(test)]
mod snapshot_tests {
    use super::encode_snapshot;

    #[test]
    fn snapshots_encode_as_png_and_jpeg() {
        let img = bi_media::jpeg::RgbaImage { width: 4, height: 2, rgba: vec![200; 4 * 2 * 4] };
        let png = encode_snapshot(&img, true, 90).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let jpg = encode_snapshot(&img, false, 90).unwrap();
        assert_eq!(&jpg[..2], &[0xFF, 0xD8]);
    }
}
