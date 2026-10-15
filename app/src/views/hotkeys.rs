//! Keyboard shortcuts. Port of `BI_Hotkeys` / `BI_Hotkey_*` (ui3.js:34832-35166): every `ui3_hotkey_*` setting
//! with UI3's default bindings, PTZ keys with press/release semantics.

use crate::app::Services;
use crate::settings::Hotkey;
use crate::views::main_view::{Content, MainView, Tab};
use bi_proto::api::{flags, ptz};

fn camera_cycle(view: &MainView, dir: i32) -> Option<String> {
    let members: Vec<String> = view.camera(&view.home_group).and_then(|g| g.group.clone()).unwrap_or_default();
    let members: Vec<String> = members.into_iter().filter(|id| view.camera(id).is_some_and(|c| c.is_enabled && !c.hidden)).collect();
    if members.is_empty() {
        return None;
    }
    let cur = view.current_source_id().unwrap_or_default().to_string();
    let i = members.iter().position(|m| m.eq_ignore_ascii_case(&cur));
    let n = members.len() as i32;
    let next = match i {
        Some(i) => (i as i32 + dir).rem_euclid(n),
        None => if dir > 0 { 0 } else { n - 1 },
    };
    Some(members[next as usize].clone())
}

fn group_cycle(view: &MainView, dir: i32) -> Option<String> {
    let groups: Vec<String> = view.groups().map(|g| g.id.clone()).collect();
    if groups.is_empty() {
        return None;
    }
    let i = groups.iter().position(|g| g.eq_ignore_ascii_case(&view.home_group)).unwrap_or(0) as i32;
    Some(groups[(i + dir).rem_euclid(groups.len() as i32) as usize].clone())
}

pub fn handle(view: &mut MainView, ctx: &egui::Context, svc: &mut Services) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    let events: Vec<(egui::Key, bool, bool, egui::Modifiers)> = ctx.input(|i| {
        i.events.iter().filter_map(|e| if let egui::Event::Key { key, pressed, repeat, modifiers, .. } = e { Some((*key, *pressed, *repeat, *modifiers)) } else { None }).collect()
    });
    for (key, pressed, repeat, m) in events {
        // PTZ release.
        if !pressed {
            view.ptz.hotkey_up(key);
            continue;
        }
        let hk = |name: &str| svc.settings.hotkey(name).is_some_and(|h: Hotkey| h.matches(key, m));
        // PTZ hold keys (no repeat).
        let ptz_keys = [
            ("ui3_hotkey_ptzUp", ptz::UP),
            ("ui3_hotkey_ptzDown", ptz::DOWN),
            ("ui3_hotkey_ptzLeft", ptz::LEFT),
            ("ui3_hotkey_ptzRight", ptz::RIGHT),
            ("ui3_hotkey_ptzIn", ptz::ZOOM_IN),
            ("ui3_hotkey_ptzOut", ptz::ZOOM_OUT),
            ("ui3_hotkey_ptzFocusNear", ptz::FOCUS_NEAR),
            ("ui3_hotkey_ptzFocusFar", ptz::FOCUS_FAR),
        ];
        if let Some((_, code)) = ptz_keys.iter().find(|(n, _)| hk(n)) {
            if !repeat && let Some(cam) = view.current_single_camera().filter(|c| c.ptz).map(|c| c.id.clone()) {
                view.ptz.camera_changed(&cam);
                view.ptz.hotkey_down(key, *code);
            }
            continue;
        }
        for n in 1..=40 {
            if hk(&format!("ui3_hotkey_ptzPreset{n}")) {
                if let Some(cam) = view.current_single_camera().filter(|c| c.ptz).map(|c| c.id.clone()) {
                    view.ptz.camera_changed(&cam);
                    view.ptz.enqueue(&cam, crate::views::ptz::PtzAction::Button(ptz::PRESET_BASE + n));
                }
            }
        }
        let client = view.client.clone();
        if hk("ui3_hotkey_maximizeVideoArea") {
            view.maximized = !view.maximized;
            svc.settings.set_bool("ui3_is_maximized", view.maximized);
        } else if hk("ui3_hotkey_togglefullscreen") {
            let on = !view.fullscreen;
            crate::views::controls::set_fullscreen(view, ctx, svc, on);
        } else if hk("ui3_hotkey_tab_live") {
            view.set_tab(ctx, svc, Tab::Live);
        } else if hk("ui3_hotkey_tab_clips") {
            view.set_tab(ctx, svc, Tab::Clips);
        } else if hk("ui3_hotkey_tab_timeline") {
            view.set_tab(ctx, svc, Tab::Timeline);
        } else if hk("ui3_hotkey_cameraLabels") {
            let v = !svc.settings.bool("ui3_cameraLabels_enabled");
            svc.settings.set_bool("ui3_cameraLabels_enabled", v);
        } else if hk("ui3_hotkey_downloadframe") {
            crate::views::dialogs::save_snapshot(view, svc);
        } else if hk("ui3_hotkey_toggleMute") {
            let mute = !svc.audio.muted();
            svc.audio.set_muted(mute);
            svc.settings.set_bool("ui3_audioMute", mute);
            view.reload_current(ctx, svc);
        } else if hk("ui3_hotkey_nextCamera") || hk("ui3_hotkey_prevCamera") {
            let dir = if hk("ui3_hotkey_nextCamera") { 1 } else { -1 };
            if let Some(id) = camera_cycle(view, dir) {
                view.select_source(ctx, svc, &id);
            }
        } else if hk("ui3_hotkey_nextGroup") || hk("ui3_hotkey_prevGroup") {
            let dir = if hk("ui3_hotkey_nextGroup") { 1 } else { -1 };
            if let Some(id) = group_cycle(view, dir) {
                view.home_group = id.clone();
                svc.settings.set("ui3_defaultCameraGroupId", id.clone());
                view.select_source(ctx, svc, &id);
            }
        } else if hk("ui3_hotkey_playpause") {
            match view.content {
                Content::Clip { .. } => view.playback.toggle_pause(ctx, &client, &mut view.player, svc),
                _ if view.tab == Tab::Timeline => view.timeline.toggle_pause(ctx, &client, &mut view.player, svc),
                _ => {}
            }
        } else if hk("ui3_hotkey_toggleReverse") {
            view.playback.toggle_reverse(ctx, &client, &mut view.player, svc);
        } else if hk("ui3_hotkey_playback_faster") {
            view.playback.change_speed(ctx, &client, &mut view.player, svc, 1);
        } else if hk("ui3_hotkey_playback_slower") {
            view.playback.change_speed(ctx, &client, &mut view.player, svc, -1);
        } else if hk("ui3_hotkey_skipAhead") || hk("ui3_hotkey_skipBack") {
            let d = (svc.settings.f64("ui3_skipAmount") * 1000.0) as i64;
            let d = if hk("ui3_hotkey_skipAhead") { d } else { -d };
            if matches!(view.content, Content::Clip { .. }) {
                view.playback.skip(ctx, &client, &mut view.player, svc, d);
            } else if view.tab == Tab::Timeline {
                view.timeline.last_set_time = view.timeline.current_time() + d;
                view.timeline.is_live = false;
                view.timeline.user_did_set_time(ctx, &client, &mut view.player, svc);
            }
        } else if hk("ui3_hotkey_skipAhead1Frame") {
            view.playback.step_frame(ctx, &client, &mut view.player, svc, 1);
        } else if hk("ui3_hotkey_skipBack1Frame") {
            view.playback.step_frame(ctx, &client, &mut view.player, svc, -1);
        } else if hk("ui3_hotkey_newerClip") || hk("ui3_hotkey_olderClip") {
            let dir = if hk("ui3_hotkey_olderClip") { 1 } else { -1 };
            let cur = view.playback.clip.as_ref().map(|c| c.path.clone()).or_else(|| view.clips.focus.and_then(|i| view.clips.clips.get(i)).map(|c| c.path.clone()));
            let next = match cur {
                Some(p) => view.clips.neighbor(&p, dir),
                None => view.clips.clips.first().cloned(),
            };
            if let Some(n) = next {
                if matches!(view.content, Content::Clip { .. }) {
                    crate::views::playback::open_clip(view, ctx, svc, n);
                } else {
                    view.clips.select_only(&n.path);
                }
            }
        } else if hk("ui3_hotkey_open_selected_clip") {
            if let Some(c) = view.clips.focus.and_then(|i| view.clips.clips.get(i)).cloned() {
                crate::views::playback::open_clip(view, ctx, svc, c);
            }
        } else if hk("ui3_hotkey_toggleFlag") {
            let items = view.clips.selected_items();
            if !items.is_empty() {
                let on = !items.iter().all(|i| i.has_flag(flags::FLAGGED));
                view.clips.set_flag(ctx, &client, items, flags::FLAGGED, on);
            }
        } else if hk("ui3_hotkey_delete") {
            if svc.settings.can_delete_clips(view.session.login.admin) {
                let items = view.clips.selected_items();
                if !items.is_empty() {
                    if svc.settings.delete_needs_confirm(items.len()) {
                        view.clips.delete_confirm = Some(items);
                    } else {
                        view.clips.delete(ctx, &client, items);
                    }
                }
            }
        } else if hk("ui3_hotkey_close_top_dialog") {
            view.dialogs.close_top();
        } else if hk("ui3_hotkey_close_clip") {
            if matches!(view.content, Content::Clip { .. }) {
                view.playback.close();
                let home = view.home_group.clone();
                view.load_live(ctx, svc.settings, Some(svc.audio), &home);
            }
        } else if hk("ui3_hotkey_close_camera") {
            if let Some(id) = view.current_source_id()
                && !id.eq_ignore_ascii_case(&view.home_group)
            {
                let home = view.home_group.clone();
                view.select_source(ctx, svc, &home);
            }
        } else if hk("ui3_hotkey_digitalZoomIn") || hk("ui3_hotkey_digitalZoomOut") {
            if let (Some(area), Some(size)) = (view.video.last_area, view.video.picture_size()) {
                let f = if hk("ui3_hotkey_digitalZoomIn") { 1.25 } else { 0.8 };
                view.video.zoom_at(area, size, area.center(), f);
            }
        } else if let Some(d) = [
            ("ui3_hotkey_digitalPanUp", egui::vec2(0.0, 40.0)),
            ("ui3_hotkey_digitalPanDown", egui::vec2(0.0, -40.0)),
            ("ui3_hotkey_digitalPanLeft", egui::vec2(40.0, 0.0)),
            ("ui3_hotkey_digitalPanRight", egui::vec2(-40.0, 0.0)),
        ]
        .iter()
        .find(|(n, _)| hk(n))
        .map(|(_, d)| *d)
        {
            if let (Some(area), Some(size)) = (view.video.last_area, view.video.picture_size()) {
                view.video.pan_by(area, size, d);
            }
        }
    }
    // Escape always restores from maximized video area / mini mode (UI3 behaviour).
    if view.maximized && ctx.input(|i| i.key_pressed(egui::Key::Escape)) && view.dialogs.open.is_empty() {
        if view.mini_mode {
            crate::views::controls::exit_mini_mode(view, ctx);
        }
        crate::views::controls::set_maximized(view, svc, false);
    }
}
