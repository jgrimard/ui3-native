//! Top bar (ui3.htm #layouttop): system name button, Live View / Clips / Timeline tabs, clip-list shortcuts,
//! save snapshot and the main menu — same order, metrics and icons as UI3 (ui3.css "sizeLarge").

use crate::app::Services;
use crate::icons::{self, Icon};
use crate::views::dialogs::Dialog;
use crate::views::main_view::{MainView, Tab};
use crate::views::style::t;
use crate::views::w::{self, ItemKind};
use egui::{Align2, FontId, Rect, Sense, pos2, vec2};

pub const HEIGHT: f32 = 64.0;
/// `#layoutleft` width (the system name block matches it).
pub const SIDEBAR_W: f32 = 256.0;

/// Counterpart of `icons::SIDEBAR_HIDDEN` with a solid side bar (not in UI3's icon set, so not in generated `icons.rs`).
const SIDEBAR_SHOWN: icons::Icon = icons::Icon { name: "sidebar_shown", svg: include_bytes!("../../assets/icons/sidebar_shown.svg"), aspect: 0.8333 };

/// UI3 main menu items (`mainMenuItems`, ui3.js:6200): command, label, icon, style, admin only.
pub const MAIN_MENU: &[(&str, &str, Icon, ItemKind, bool)] = &[
    ("ui_settings", "UI Settings", icons::SETTINGS, ItemKind::Golden, false),
    ("about_this_ui", "About This UI", icons::ABOUT, ItemKind::Golden, false),
    ("streaming_profiles", "Streaming Profiles", icons::VIDEO_FILTER, ItemKind::Golden, false),
    ("copy_current_url", "Copy Current URL", icons::COPY, ItemKind::Golden, false),
    ("system_log", "System Log", icons::SYSTEM_LOG, ItemKind::Blue, false),
    ("user_list", "User List", icons::USER, ItemKind::Blue, true),
    ("device_list", "Device List", icons::DEVICE_INFO, ItemKind::Blue, true),
    ("full_camera_list", "Full Camera List", icons::FULL_CAMERA_LIST, ItemKind::Blue, false),
    ("export_list", "Convert/Export Queue", icons::LAUNCH, ItemKind::Blue, false),
    ("disk_usage", "Disk Usage", icons::INFORMATION, ItemKind::Blue, false),
    ("server_control", "Server Control", icons::SYSTEM_CONFIGURATION, ItemKind::Blue, true),
    ("bi_updates", "Update Blue Iris", icons::UPDATE, ItemKind::Blue, true),
    ("help", "Help", icons::HELP, ItemKind::Golden, false),
    ("logout", "Log Out", icons::LOGOUT, ItemKind::Golden, false),
];

/// Options for the "System Name Button Action" setting (`getSystemNameButtonOptions`).
pub fn system_name_button_options(admin: bool) -> Vec<String> {
    MAIN_MENU.iter().filter(|m| !m.4 || admin).map(|m| m.1.to_string()).chain(["Toggle Side Bar", "Follow a Link", "Do Nothing"].map(String::from)).collect()
}

pub fn run_menu_command(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, cmd: &str) {
    match cmd {
        "ui_settings" => view.dialogs.open(Dialog::Settings),
        "about_this_ui" => view.dialogs.open(Dialog::About),
        "streaming_profiles" => view.dialogs.open(Dialog::StreamingProfiles),
        "copy_current_url" => {
            // Mirrors `UpdateCurrentURL` (ui3.js:34727). `base_url` has no session argument, so nothing secret is copied.
            let base = view.client.base_url().to_string();
            let query = match &view.content {
                crate::views::main_view::Content::Clip { key } => {
                    let mut q = format!("rec={}", bi_proto::api::rec_id_from_path(key));
                    if view.playback.position_ms > 0 {
                        q.push_str(&format!("-{}", view.playback.position_ms));
                    }
                    if view.playback.paused {
                        q.push_str("&pause=1");
                    }
                    q
                }
                crate::views::main_view::Content::Timeline { id } => {
                    format!("timeline={}&cam={id}{}", view.timeline.current_time(), if view.timeline.paused { "&pause=1" } else { "" })
                }
                crate::views::main_view::Content::Live { id } => {
                    let arg = if view.camera(id).is_some_and(|c| c.is_group()) { "group" } else { "cam" };
                    format!("t={}&{arg}={id}", match view.tab { Tab::Timeline => "timeline", Tab::Clips => "clips", Tab::Live => "live" })
                }
                _ => format!("t=live&group={}", view.home_group),
            };
            ctx.copy_text(format!("{base}ui3.htm?{query}"));
            svc.toasts.info("URL copied to the clipboard");
        }
        "system_log" => view.dialogs.open(Dialog::SystemLog),
        "user_list" => view.dialogs.open(Dialog::UserList),
        "device_list" => view.dialogs.open(Dialog::DeviceList),
        "full_camera_list" => view.dialogs.open(Dialog::CameraList),
        "export_list" => view.dialogs.open(Dialog::ExportList),
        "disk_usage" => view.dialogs.open(Dialog::ServerStatus),
        "server_control" => view.dialogs.open(Dialog::ServerControl),
        "bi_updates" => view.dialogs.open(Dialog::UpdateCheck),
        "help" => ctx.open_url(egui::OpenUrl::new_tab(view.client.url("ui3/help/help.html"))),
        "logout" => view.logout_requested = true,
        _ => {}
    }
}

pub fn sidebar_visible(view: &MainView, svc: &Services) -> bool {
    match view.tab {
        Tab::Clips => svc.settings.bool("ui3_sidebar_visible_on_clips"),
        _ => svc.settings.bool("ui3_sidebar_visible_on_live"),
    }
}

pub fn toggle_sidebar(view: &MainView, svc: &mut Services) {
    let key = if view.tab == Tab::Clips { "ui3_sidebar_visible_on_clips" } else { "ui3_sidebar_visible_on_live" };
    let v = svc.settings.bool(key);
    svc.settings.set(key, if v { "0" } else { "1" });
}

pub fn show(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services) {
    let ctx = ui.ctx().clone();
    egui::Panel::top("topbar").exact_size(HEIGHT).frame(egui::Frame::new().fill(t().panel)).show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let full = ui.max_rect();
        let admin = view.session.login.admin;
        let has_clips = view.session.login.permissions().clips;
        if !has_clips && view.tab != Tab::Live {
            view.set_tab(&ctx, svc, Tab::Live);
        }

        // ---- Right-hand icons (measured first so the tabs can shrink to fit) ----
        let mut right: Vec<(&str, f32)> = Vec::new();
        for (key, what, width) in [
            ("ui3_topbar_allclips_shortcut_show", "all", 64.0),
            ("ui3_topbar_alerts_shortcut_show", "alerts", 72.0),
            ("ui3_topbar_alerts_canceled_shortcut_show", "cancelled", 64.0),
            ("ui3_topbar_alerts_confirmed_shortcut_show", "confirmed", 64.0),
            ("ui3_topbar_save_snapshot_btn_show", "snapshot", 56.0),
        ] {
            // The list shortcuts need the clips permission (ui3.js:39664); the snapshot button does not.
            if svc.settings.bool(key) && (has_clips || what == "snapshot") {
                right.push((what, width));
            }
        }
        right.push(("menu", 74.0));
        let right_w: f32 = right.iter().map(|r| r.1).sum();

        // ---- System name ----
        let name = {
            let o = svc.settings.str("ui3_system_name_button_text_override");
            if o.trim().is_empty() { view.session.login.system_name.clone() } else { o }
        };
        let sys_w = SIDEBAR_W.min((full.width() - right_w) * 0.4).max(120.0);
        let sys_rect = Rect::from_min_size(full.min, vec2(sys_w, HEIGHT));
        let action = svc.settings.str("ui3_system_name_button");
        let hot = action != "Do Nothing";
        let sys_resp = ui.interact(sys_rect, ui.id().with("sysname"), if hot { Sense::click() } else { Sense::hover() });
        let p = ui.painter();
        p.rect_filled(sys_rect, 0.0, if hot && sys_resp.hovered() { t().highlight } else { t().sysname_bg });
        p.with_clip_rect(sys_rect.shrink2(vec2(34.0, 0.0))).text(pos2(sys_rect.center().x, sys_rect.top() + 28.0), Align2::CENTER_CENTER, &name, FontId::proportional(20.0), t().text_active);
        let mut sidebar_btn_clicked = false;
        // Side bar toggle (per tab: Live/Timeline share one setting, Clips has its own). UI3 only shows it while
        // the side bar is hidden; here it is always shown so the side bar can be hidden again without Settings.
        if svc.settings.bool("ui3_show_sidebar_hidden_button") {
            let visible = sidebar_visible(view, svc);
            let br = Rect::from_min_size(sys_rect.min + vec2(0.0, 2.0), vec2(32.0, 32.0));
            let tip = if visible { "Hide Side Bar" } else { "Show Side Bar" };
            let r = ui.interact(br, ui.id().with("sidebar_toggle"), Sense::click()).on_hover_text(tip);
            let a = if r.hovered() { 0.8 } else { 0.4 };
            w::paint_icon(ui, if visible { SIDEBAR_SHOWN } else { icons::SIDEBAR_HIDDEN }, br, t().text_active.gamma_multiply(a));
            if r.clicked() {
                sidebar_btn_clicked = true;
                toggle_sidebar(view, svc);
            }
        }
        if sys_resp.clicked() && !sidebar_btn_clicked {
            match action.as_str() {
                "Toggle Side Bar" => toggle_sidebar(view, svc),
                "Follow a Link" => {
                    let link = svc.settings.str("ui3_system_name_button_link");
                    if !link.is_empty() {
                        ctx.open_url(egui::OpenUrl::new_tab(link));
                    }
                }
                a => {
                    if let Some(m) = MAIN_MENU.iter().find(|m| m.1 == a && (!m.4 || admin)) {
                        run_menu_command(view, &ctx, svc, m.0);
                    }
                }
            }
        }

        // ---- Tabs ----
        let tabs_avail = (full.width() - sys_w - right_w).max(150.0);
        let tab_w = (tabs_avail / 3.0).min(SIDEBAR_W);
        let mut x = full.left() + sys_w;
        for (tab, label) in [(Tab::Live, "Live View"), (Tab::Clips, "Clips"), (Tab::Timeline, "Timeline")] {
            if tab != Tab::Live && !has_clips {
                continue;
            }
            let r = Rect::from_min_size(pos2(x, full.top()), vec2(tab_w, HEIGHT));
            let resp = ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| w::topbar_tab(ui, label, view.tab == tab, vec2(tab_w, HEIGHT))).inner;
            if resp.clicked() {
                view.set_tab(&ctx, svc, tab);
            }
            x += tab_w;
        }

        // ---- Icons ----
        let mut x = full.right() - right_w;
        // UI3 shows the status `alerts` value (ui3.js:16389).
        let new_alerts: i64 = view.status.as_ref().map(|s| s.alerts).unwrap_or(0);
        for (what, width) in right {
            let cell = Rect::from_min_size(pos2(x, full.top()), vec2(width, HEIGHT));
            x += width;
            let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(cell).layout(egui::Layout::left_to_right(egui::Align::Center)));
            let selected = view.tab == Tab::Clips && view.clips.db_view == what;
            let base = if selected { t().dd_blue } else { t().button };
            let mut open_view = None;
            match what {
                "all" => {
                    if w::icon_button_colored(&mut cui, icons::STORAGE, vec2(24.0, 24.0), vec2(20.0, 20.0), "Open All Clips", base, false).clicked() {
                        open_view = Some("all");
                    }
                }
                "alerts" => {
                    let r = w::icon_button_colored(&mut cui, icons::ALERT1, vec2(16.0, 36.0), vec2(28.0, 14.0), "Open Alerts", base, false);
                    if new_alerts > 0 && svc.settings.bool("ui3_topbar_alerts_shortcut_counter") {
                        w::counter(ui.painter(), pos2(cell.right() - 5.0, cell.top() + 6.0), &new_alerts.min(99999).to_string());
                    }
                    if r.clicked() {
                        // Only the context menu's "Clear Counters" resets the counters (ui3.js:28094).
                        open_view = Some("alerts");
                    }
                    // UI3 OpenAlertListButtonContextMenu.
                    let mut action = None;
                    egui::Popup::context_menu(&r).frame(w::list_frame()).width(160.0).show(|ui| {
                        ui.set_min_width(160.0);
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        if w::list_item(ui, Some(icons::ALERT1), "Open Alert List", false, w::ItemKind::Normal).clicked() {
                            action = Some(true);
                            ui.close();
                        }
                        if w::list_item(ui, Some(icons::TRASH), "Clear Counters", false, w::ItemKind::Normal).clicked() {
                            action = Some(false);
                            ui.close();
                        }
                    });
                    match action {
                        Some(true) => open_view = Some("alerts"),
                        Some(false) => view.reset_new_alert_counters(&ctx),
                        None => {}
                    }
                }
                "cancelled" => {
                    if w::icon_button_colored(&mut cui, icons::HOLD_PROFILE, vec2(24.0, 24.0), vec2(20.0, 20.0), "Open Cancelled Alerts", base, false).clicked() {
                        open_view = Some("cancelled");
                    }
                }
                "confirmed" => {
                    if w::icon_button_colored(&mut cui, icons::CB_CHECKED, vec2(24.0, 24.0), vec2(20.0, 20.0), "Open Confirmed Alerts", base, false).clicked() {
                        open_view = Some("confirmed");
                    }
                }
                "snapshot" => {
                    if w::icon_button(&mut cui, icons::SNAPSHOT, vec2(30.0, 24.0), vec2(13.0, 20.0), "Save image to disk").clicked() {
                        crate::views::dialogs::save_snapshot(view, svc);
                    }
                }
                _ => {
                    let r = w::icon_button(&mut cui, icons::MENU, vec2(10.0, 40.0), vec2(32.0, 12.0), "Main Menu");
                    if svc.settings.bool("ui3_topbar_warnings_counter")
                        && let Some(st) = &view.status
                        && st.warnings > 0
                    {
                        w::counter(ui.painter(), pos2(cell.right() - 5.0, cell.top() + 6.0), &st.warnings.min(99).to_string());
                    }
                    main_menu_popup(view, &ctx, svc, &r);
                }
            }
            if let Some(v) = open_view {
                view.clips.set_view(&ctx, &view.client, svc.settings, v, &view.home_group);
                view.set_tab(&ctx, svc, Tab::Clips);
            }
        }
    });
}

fn main_menu_popup(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, r: &egui::Response) {
    let admin = view.session.login.admin;
    let warnings = view.status.as_ref().map(|s| s.warnings).unwrap_or(0);
    let mut chosen = None;
    egui::Popup::menu(r).frame(w::list_frame()).width(290.0).align(egui::RectAlign::BOTTOM_END).show(|ui| {
        ui.set_min_width(260.0);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        for (cmd, label, icon, kind, admin_only) in MAIN_MENU {
            if *admin_only && !admin {
                continue;
            }
            let r = w::list_item(ui, Some(*icon), label, false, *kind);
            if *cmd == "system_log" && warnings > 0 {
                w::counter(ui.painter(), pos2(r.rect.right() - 8.0, r.rect.top() + 6.0), &warnings.min(99999).to_string());
            }
            if r.clicked() {
                chosen = Some(*cmd);
                ui.close();
            }
        }
    });
    if let Some(c) = chosen {
        run_menu_command(view, ctx, svc, c);
    }
}
