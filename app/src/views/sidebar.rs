//! Left bar (ui3.htm #layoutleft, 256px): on Live / Timeline the PTZ, Profile, Schedule, Current Group and
//! Streaming Quality boxes with the status area pinned to the bottom; on Clips the clip list.

use crate::app::Services;
use crate::icons;
use crate::runtime;
use crate::views::main_view::{MainView, Tab};
use crate::views::style::t;
use crate::views::w;
use egui::{Align2, Color32, FontId, Rect, Sense, pos2, vec2};
use std::time::Duration;

pub const WIDTH: f32 = 256.0;
/// Content width inside a `.leftBarBox` (190px controls).
const BOX_W: f32 = 190.0;

pub fn show(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services) {
    if !crate::views::topbar::sidebar_visible(view, svc) {
        return;
    }
    let id = if view.tab == Tab::Clips { "sidebar_clips" } else { "sidebar_live" };
    let panel = if svc.settings.str("ui3_sideBarPosition") == "Right" { egui::Panel::right(id) } else { egui::Panel::left(id) };
    panel.resizable(false).exact_size(WIDTH).frame(egui::Frame::new().fill(t().panel)).show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let full = ui.max_rect();
        let status_on = svc.settings.bool("ui3_status_area_show")
            && match view.tab {
                Tab::Clips => svc.settings.bool("ui3_status_area_show_clips"),
                Tab::Timeline => svc.settings.bool("ui3_status_area_show_timeline"),
                Tab::Live => true,
            };
        let status_h = if status_on { status_area_height(svc) } else { 0.0 };
        let top = Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - status_h));
        let mut top_ui = ui.new_child(egui::UiBuilder::new().max_rect(top).layout(egui::Layout::top_down(egui::Align::Min)));
        if view.tab == Tab::Clips {
            crate::views::clips::show_list(view, &mut top_ui, svc);
        } else {
            egui::ScrollArea::vertical().id_salt("leftlive").auto_shrink([false, false]).show(&mut top_ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                ui.add_space(22.0);
                live_boxes(view, ui, svc);
            });
        }
        if status_on {
            let rect = Rect::from_min_max(pos2(full.left(), full.bottom() - status_h), full.max);
            let mut sui = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
            status_area(view, &mut sui, svc, rect);
        }
    });
}

fn live_boxes(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services) {
    let ctx = ui.ctx().clone();
    if svc.settings.bool("ui3_feature_enabled_ptzControls") && view.session.login.permissions().ptz {
        section(ui, "PTZ", "ui3_collapsible_ptz", svc, |ui, svc| {
            ui.add_space(16.0);
            crate::views::ptz::show(view, ui, svc);
        });
    }
    // Profile and schedule boxes need the change-profile permission (ui3.js:17254).
    let can_change_profile = view.session.login.permissions().change_profile;
    if svc.settings.bool("ui3_feature_enabled_profileStatus")
        && can_change_profile
        && let Some(st) = view.status.clone()
    {
        section(ui, "Profile", "ui3_collapsible_profileStatus", svc, |ui, svc| {
            ui.add_space(15.0);
            profile_box(view, ui, svc, &st);
        });
    }
    if svc.settings.bool("ui3_feature_enabled_globalSchedule")
        && can_change_profile
        && !view.session.login.schedules.is_empty()
        && let Some(st) = view.status.clone()
    {
        section(ui, "Schedule", "ui3_collapsible_schedule", svc, |ui, _svc| {
            ui.add_space(16.0);
            let cur = st.schedule.clone();
            let can = can_change_profile;
            let mut chosen = None;
            w::dropdown_box(ui, "schedule", &cur, BOX_W, 39.0, can, |ui| {
                for s in &view.session.login.schedules {
                    if w::list_item(ui, None, s, *s == cur, w::ItemKind::Normal).clicked() {
                        chosen = Some(s.clone());
                        ui.close();
                    }
                }
            });
            if let Some(s) = chosen {
                let c = view.client.clone();
                runtime::spawn_detached(async move {
                    let _ = c.set_schedule(&s).await;
                });
                view.force_status_refresh();
            }
        });
    }
    section(ui, "Current Group", "ui3_collapsible_currentGroup", svc, |ui, svc| {
        ui.add_space(16.0);
        current_group_dropdown(view, ui, svc, &ctx, "group");
    });
    section(ui, "Streaming Quality", "ui3_collapsible_streamingQuality", svc, |ui, svc| {
        ui.add_space(16.0);
        quality_dropdown(view, ui, svc, &ctx);
    });
}

/// `.leftBarBox` with a collapsible `.leftBarHeading`; the open state persists in UI3's setting keys.
fn section(ui: &mut egui::Ui, title: &str, key: &str, svc: &mut Services, body: impl FnOnce(&mut egui::Ui, &mut Services)) {
    let mut open = svc.settings.bool(key);
    w::left_box(ui, |ui| {
        ui.set_width(WIDTH - 61.0);
        w::left_heading(ui, title, &mut open);
        if open {
            body(ui, svc);
        }
    });
    if open != svc.settings.bool(key) {
        svc.settings.set(key, if open { "1" } else { "0" });
    }
}

/// UI3 `getProfileDropdownHtml`: bold profile number in the profile's color, then the name.
pub fn profile_job(num: i64, name: &str, size: f32) -> egui::text::LayoutJob {
    let (color, bg) = match num {
        0 => (Color32::from_rgb(0xEE, 0xEE, 0xEE), Some(Color32::from_rgb(0xEE, 0x23, 0x2C))),
        1 => (Color32::from_rgb(0x00, 0xFF, 0x00), None),
        2 => (Color32::from_rgb(0x00, 0x97, 0xF0), None),
        3 => (Color32::from_rgb(0xFF, 0x00, 0x00), None),
        4 => (Color32::from_rgb(0xFF, 0xFF, 0x00), None),
        5 => (Color32::from_rgb(0xFF, 0x88, 0x00), None),
        6 => (Color32::from_rgb(0xFF, 0x00, 0xFF), None),
        7 => (Color32::from_rgb(0x00, 0xFF, 0xFF), None),
        _ => (Color32::from_rgb(0xFF, 0x00, 0x00), None),
    };
    let num_s = if bg.is_some() { format!(" {num} ") } else { num.to_string() };
    let name = if num == 0 && name.is_empty() {
        "Inactive".to_string()
    } else if name.is_empty() {
        format!("Profile {num}")
    } else {
        name.to_string()
    };
    w::job(&[(&num_s, color, true, bg), (&format!("   {name}"), t().text, false, None)], size)
}

fn profile_box(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services, st: &bi_proto::api::StatusData) {
    let names = view.session.login.profiles.clone();
    let cur = st.profile.max(0);
    let can = view.session.login.permissions().change_profile;
    let mut chosen = None;
    let mut lock_click = false;
    let mut hint = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let label = names.get(cur as usize).cloned().unwrap_or_default();
        w::dropdown_box_job(ui, "profile", Some(profile_job(cur, &label, 16.0)), &label, BOX_W - 39.0, 39.0, can, |ui| {
            for (i, n) in names.iter().enumerate() {
                if w::list_item_job(ui, profile_job(i as i64, n, 16.0), i as i64 == cur).clicked() {
                    chosen = Some(i as i64);
                    ui.close();
                }
            }
        });
        // #schedule_lock_button: green play (schedule active), red square (hold), yellow pause (temporary).
        let sched = if st.schedule.is_empty() { "N/A".to_string() } else { st.schedule.clone() };
        let (icon, color, tip) = match st.lock {
            1 => (icons::SQUARE, t().profile_stop, format!("Schedule \"{sched}\" is currently disabled. Click to re-enable.")),
            2 => (icons::PAUSE, t().profile_pause, format!("Schedule \"{sched}\" is temporarily overridden. Click to resume schedule, or wait some hours and it should return to normal.")),
            _ => (icons::PLAY, t().profile_active, format!("Schedule \"{sched}\" is active. Long press to disable automatic scheduling.")),
        };
        let color = if can { color } else { Color32::from_gray(0x6A) };
        let (rect, r) = ui.allocate_exact_size(vec2(39.0, 39.0), if can { Sense::click() } else { Sense::hover() });
        if can && (r.hovered() || r.is_pointer_button_down_on()) {
            ui.painter().rect_filled(rect, 0.0, Color32::from_white_alpha(if r.is_pointer_button_down_on() { 51 } else { 26 }));
        }
        w::paint_icon(ui, icon, rect.shrink(8.0), color);
        let r = r.on_hover_text(tip);
        // Long press (600 ms) toggles the lock even while the schedule is running.
        let long_id = ui.id().with("lock_long");
        let fired: bool = ui.data(|d| d.get_temp(long_id).unwrap_or(false));
        if r.is_pointer_button_down_on() {
            let held = ui.input(|i| i.pointer.press_start_time().map(|t0| i.time - t0)).unwrap_or(0.0);
            if held > 0.6 && !fired {
                ui.data_mut(|d| d.insert_temp(long_id, true));
                lock_click = true;
            }
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        } else {
            if r.clicked() && !fired {
                if st.lock != 0 {
                    lock_click = true;
                } else {
                    hint = true;
                }
            }
            ui.data_mut(|d| d.remove::<bool>(long_id));
        }
    });
    if hint {
        svc.toasts.info("Long press to disable automatic scheduling.");
    }
    if let Some(i) = chosen {
        let c = view.client.clone();
        runtime::spawn_detached(async move {
            let _ = c.set_profile(i, None).await;
        });
        view.force_status_refresh();
    }
    if lock_click {
        let c = view.client.clone();
        runtime::spawn_detached(async move {
            let _ = c.set_profile(-1, None).await;
        });
        view.force_status_refresh();
    }
}

/// The "currentGroup" dropdown (also used by the Clips "Filter by" box).
pub fn current_group_dropdown(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services, ctx: &egui::Context, salt: &str) {
    let groups: Vec<(String, String)> = view.groups().map(|g| (g.id.clone(), g.name.clone())).collect();
    // The Clips "Filter by" list always offers individual cameras so clips can be filtered per camera.
    let cams: Vec<(String, String)> = if view.tab == Tab::Clips || svc.settings.bool("ui3_show_cameras_in_group_dropdowns") {
        view.cameras.iter().filter(|c| !c.is_group() && !c.is_cycle() && c.is_enabled && !c.hidden && (view.tab == Tab::Clips || c.webcast())).map(|c| (c.id.clone(), c.name.clone())).collect()
    } else {
        Vec::new()
    };
    let current = if view.tab == Tab::Clips {
        if view.clips.camera_filter.is_empty() { view.home_group.clone() } else { view.clips.camera_filter.clone() }
    } else {
        // UI3 keeps showing the group while one of its cameras is maximized.
        match &view.content {
            crate::views::main_view::Content::Live { id } | crate::views::main_view::Content::Timeline { id } if view.camera(id).is_some_and(|c| c.is_group() || c.is_cycle()) || !cams.is_empty() => id.clone(),
            _ => view.home_group.clone(),
        }
    };
    let cur_name = view.camera(&current).map(|c| c.name.clone()).unwrap_or_else(|| current.clone());
    let mut chosen = None;
    w::dropdown_box(ui, salt, &cur_name, BOX_W, 39.0, true, |ui| {
        for (id, name) in groups.iter().chain(cams.iter()) {
            if w::list_item(ui, None, name, id.eq_ignore_ascii_case(&current), w::ItemKind::Normal).clicked() {
                chosen = Some(id.clone());
                ui.close();
            }
        }
    });
    if let Some(id) = chosen {
        if view.camera(&id).is_some_and(|c| c.is_group() || c.is_cycle()) {
            view.home_group = id.clone();
            svc.settings.set("ui3_defaultCameraGroupId", id.clone());
        }
        match view.tab {
            Tab::Timeline => view.select_source(ctx, svc, &id),
            Tab::Clips => {
                view.clips.camera_filter = if view.camera(&id).is_some_and(|c| c.is_group()) && id.eq_ignore_ascii_case(&view.home_group) { String::new() } else { id.clone() };
                let home = view.home_group.clone();
                view.clips.ensure_loaded(ctx, &view.client, svc.settings, &home);
            }
            Tab::Live => view.load_live(ctx, svc.settings, Some(svc.audio), &id),
        }
    }
}

fn quality_dropdown(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services, ctx: &egui::Context) {
    let cur = svc.settings.current_profile();
    let mut chosen = None;
    let profiles = svc.settings.streaming_profiles.clone();
    w::dropdown_box(ui, "quality", &cur.display_name(), BOX_W, 39.0, true, |ui| {
        for p in &profiles {
            let job = w::job(&[(&p.display_name(), t().dd_text, false, None), (&format!("   {}", p.abbr), t().dd_blue, true, None)], 16.0);
            if w::list_item_job(ui, job, p.name == cur.name).on_hover_text(p.tooltip()).clicked() {
                chosen = Some(p.name.clone());
                ui.close();
            }
        }
    });
    if let Some(n) = chosen {
        svc.settings.set("ui3_streamingQuality", n);
        view.reload_current(ctx, svc);
    }
}

// ---- Status area (`status-area` component) ----

/// Status bars UI3 registers (`statusAreaApi.setStatusBarRegistration`, ui3.js:5418).
pub const STATUS_BAR_NAMES: [&str; 8] = ["Server CPU", "Server Memory", "Server Disk", "Server Uptime", "Stream FPS", "Stream Bit Rate", "Stream Delay", "Audio Buffer"];

fn bar_keys(svc: &Services) -> Vec<String> {
    (1..=4).map(|i| svc.settings.str(&format!("ui3_status_area_bar_key_{i}"))).filter(|k| !k.is_empty() && k != "None").collect()
}

fn status_area_height(svc: &Services) -> f32 {
    let collapsed = !svc.settings.bool("ui3_collapsible_serverStatus");
    let header = 1.0 + 5.0 + 18.0 + 12.0;
    if collapsed { header } else { header + bar_keys(svc).len() as f32 * 31.0 }
}

fn status_area(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services, rect: Rect) {
    ui.painter().rect_filled(rect, 0.0, t().panel);
    ui.painter().hline(rect.x_range(), rect.top() + 0.5, egui::Stroke::new(1.0, t().separator));
    let inner = Rect::from_min_max(pos2(rect.left() + 31.0, rect.top() + 6.0), pos2(rect.right() - 30.0, rect.bottom()));
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
    let ui = &mut ui;
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let mut open = svc.settings.bool("ui3_collapsible_serverStatus");
    // Header: "Server Status" (click to collapse) and the traffic signal button.
    let header = Rect::from_min_size(inner.min, vec2(inner.width(), 18.0));
    let name = svc.settings.str("ui3_status_area_name");
    let name = if name.is_empty() { "Server Status".to_string() } else { name };
    let g = ui.painter().layout_no_wrap(name, FontId::proportional(15.0), t().text);
    let label_rect = Rect::from_min_size(header.min, vec2(g.size().x + 4.0, 18.0));
    let lr = ui.interact(label_rect, ui.id().with("ss_label"), Sense::click());
    if lr.hovered() {
        ui.painter().rect_filled(label_rect, 0.0, Color32::from_rgb(0x1F, 0x20, 0x26));
    }
    ui.painter().galley(pos2(label_rect.left(), label_rect.top()), g, t().text);
    if lr.clicked() {
        open = !open;
        svc.settings.set("ui3_collapsible_serverStatus", if open { "1" } else { "0" });
    }
    if svc.settings.bool("ui3_feature_enabled_stopLight")
        && let Some(st) = &view.status
    {
        let admin = view.session.login.admin;
        let base = pos2(header.right() - 58.0, header.top());
        let lights = [(0, Color32::from_rgb(255, 0, 0)), (2, Color32::from_rgb(255, 255, 0)), (1, Color32::from_rgb(99, 195, 91))];
        let br = Rect::from_min_size(base, vec2(58.0, 16.0));
        let r = ui.interact(br, ui.id().with("stoplight"), if admin { Sense::click() } else { Sense::hover() });
        if admin && r.hovered() {
            ui.painter().rect_filled(br, 0.0, t().ui_hover);
        }
        for (i, (sig, c)) in lights.iter().enumerate() {
            let on = st.signal == *sig;
            let cr = Rect::from_min_size(base + vec2(i as f32 * 21.0, 0.0), vec2(16.0, 16.0));
            w::paint_icon(ui, icons::STOPLIGHT, cr, c.gamma_multiply(if on { 1.0 } else { 0.15 }));
        }
        let tip = match st.signal {
            0 => "Red: Blue Iris is not recording or alerting",
            2 => "Yellow: recording but not alerting",
            _ => "Green: normal operation",
        };
        let sig = st.signal;
        if r.on_hover_text(format!("Traffic signal — {tip}{}", if admin { "\nClick to change" } else { "" })).clicked() && admin {
            // UI3 only toggles red <-> yellow (ui3.js:6046).
            let next = if sig == 0 { 2 } else { 0 };
            let c = view.client.clone();
            runtime::spawn_detached(async move {
                let _ = c.set_signal(next).await;
            });
            view.force_status_refresh();
        }
    }
    ui.add_space(30.0);
    if !open {
        return;
    }
    let Some(st) = view.status.clone() else { return };
    for key in bar_keys(svc) {
        match key.as_str() {
            "Server CPU" => {
                let cpu: f32 = st.cpu.trim_end_matches('%').parse().unwrap_or(0.0);
                w::status_bar(ui, "CPU", cpu / 100.0, &format!("{}%", cpu.round()), &format!("Total CPU usage on server: {}%", cpu.round()));
            }
            "Server Memory" => {
                let load: f32 = st.memload.trim_end_matches('%').parse().unwrap_or(0.0);
                let tip = if st.memphys.is_empty() { format!("Server's Memory Usage: {} ({}%)", st.mem, load as i32) } else { format!("Server's Memory Usage: {}/{} ({}%)", st.mem, st.memphys, load as i32) };
                w::status_bar(ui, "MEM", load / 100.0, &format!("{}%", load as i32), &tip);
            }
            "Server Disk" => {
                let (used, alloc) = st.disks.iter().fold((0.0, 0.0), |(u, a), d| (u + d.used, a + d.allocated));
                let frac = if alloc > 0.0 { (used / alloc) as f32 } else { 0.0 };
                let tip = format!("Click to visualize disk usage.{}", if st.clips.is_empty() { String::new() } else { format!("\n\n{}", st.clips) });
                if w::status_bar(ui, "DISK", frac, &format!("{}%", (frac * 100.0) as i32), &tip).clicked() {
                    view.dialogs.open(crate::views::dialogs::Dialog::ServerStatus);
                }
            }
            "Server Uptime" => {
                let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
                ui.painter().text(rect.left_center(), Align2::LEFT_CENTER, "UPTIME", FontId::proportional(15.0), t().text);
                ui.painter().text(rect.right_center(), Align2::RIGHT_CENTER, &st.uptime, FontId::proportional(15.0), t().text);
                r.on_hover_text("Server Uptime");
            }
            "Stream FPS" => {
                let s = view.player.stats();
                let max = view.current_single_camera().map(|c| c.fps).filter(|f| *f > 0.0).unwrap_or(10.0);
                w::status_bar(ui, "FPS", (s.fps / max) as f32, &format!("{}", s.fps.round()), "Frame rate of video being played");
            }
            "Stream Bit Rate" => {
                let s = view.player.stats();
                let mbps = (s.video_kbps + s.audio_kbps) / 1000.0;
                let k = svc.settings.current_profile().kbps;
                let expect = if k > 0 { k as f64 / 1000.0 } else { 24.0 };
                w::status_bar(ui, "MBPS", (mbps / expect) as f32, &format!("{mbps:.1}"), "Total video + audio bit rate");
            }
            "Stream Delay" => {
                let ms = view.player.stats().late_ms.max(0.0);
                w::status_bar(ui, "DLY", (ms / 5000.0) as f32, &format!("{}", ms as i64), "Total video streaming delay in milliseconds");
            }
            "Audio Buffer" => {
                let ms = view.player.stats().audio_buffer_ms;
                w::status_bar(ui, "ABUF", (ms / 1000.0) as f32, &format!("{}", ms as i64), "Audio buffer contains this many milliseconds");
            }
            _ => continue,
        }
        ui.add_space(15.0);
    }
}
