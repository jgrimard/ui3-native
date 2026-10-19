//! Clip / alert list. Port of `ClipLoader` (ui3.js:13520), `ClipFilterSearch`, `DateFilter`, `ClipListDynamicTileLoader`
//! (virtualized list), `AsyncThumbnailDownloader`, `BigThumbHelper` + `ClipThumbnailVideoPreview_BruteForce` (hover
//! preview animation) and `ClipListContextMenu`.

use crate::app::Services;
use crate::gpu::Gpu;
use crate::runtime::{self, Pending};
use crate::settings::Settings;
use crate::views::main_view::{Content, MainView};
use crate::icons::{self, Icon};
use crate::views::{sidebar, style, w};
use anyhow::Result;
use bi_media::jpeg::decode_jpeg_rgba;
use bi_proto::BiClient;
use bi_proto::api::{ClipItem, flags};
use bi_proto::commands::{ClipQuery, DB_VIEWS};
use chrono::{Datelike, NaiveDate};
use egui::{Color32, RichText, Sense, TextureHandle, pos2, vec2};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

enum Thumb {
    Loading,
    Ready(TextureHandle),
    Failed,
}

#[derive(Clone)]
enum Row {
    Date(NaiveDate),
    Clip(usize),
}

struct Preview {
    rec_id: String,
    frame: usize,
    loops: usize,
    started: Instant,
    job: Option<Pending<Result<TextureHandle>>>,
    tex: Option<TextureHandle>,
    next_at: Instant,
}

type LoadKey = (String, String, String, Option<(NaiveDate, NaiveDate)>);

pub struct ClipsPanel {
    pub db_view: String,
    pub camera_filter: String,
    /// Single camera last seen in the video area; `camera_filter` follows it when it changes.
    followed_camera: String,
    pub search: String,
    search_applied: String,
    search_changed_at: Option<Instant>,
    pub date_filter: bool,
    pub date_start: NaiveDate,
    pub date_end: NaiveDate,
    pub clips: Vec<ClipItem>,
    rows: Vec<Row>,
    job: Option<Pending<Result<Vec<ClipItem>>>>,
    loaded_for: Option<LoadKey>,
    pub error: Option<String>,
    thumbs: HashMap<String, Thumb>,
    thumb_queue: VecDeque<String>,
    thumb_jobs: Vec<(String, Pending<Result<egui::ColorImage>>)>,
    pub selected: HashSet<String>,
    pub focus: Option<usize>,
    anchor: Option<usize>,
    preview: Option<Preview>,
    last_refresh: Instant,
    scroll_to: Option<usize>,
    pub tz_offset_ms: i64,
    action_jobs: Vec<Pending<Result<String>>>,
    memo_edit: Option<(String, String)>,
    pub delete_confirm: Option<Vec<ClipItem>>,
}

impl ClipsPanel {
    pub fn new(settings: &Settings) -> Self {
        let today = chrono::Local::now().date_naive();
        ClipsPanel {
            db_view: settings.str("ui3_current_dbView"),
            camera_filter: String::new(),
            followed_camera: String::new(),
            search: String::new(),
            search_applied: String::new(),
            search_changed_at: None,
            date_filter: false,
            date_start: today - chrono::Days::new(7),
            date_end: today,
            clips: Vec::new(),
            rows: Vec::new(),
            job: None,
            loaded_for: None,
            error: None,
            thumbs: HashMap::new(),
            thumb_queue: VecDeque::new(),
            thumb_jobs: Vec::new(),
            selected: HashSet::new(),
            focus: None,
            anchor: None,
            preview: None,
            last_refresh: Instant::now(),
            scroll_to: None,
            tz_offset_ms: 0,
            action_jobs: Vec::new(),
            memo_edit: None,
            delete_confirm: None,
        }
    }

    fn query(&self, camera: &str) -> ClipQuery {
        let (start, end) = if self.date_filter {
            let s = self.date_start.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() - self.tz_offset_ms / 1000;
            let e = self.date_end.and_hms_opt(23, 59, 59).unwrap().and_utc().timestamp() - self.tz_offset_ms / 1000;
            (s, e)
        } else {
            // UI3 loads everything with paging when no date filter is set; bound it to make continuation work.
            (0, 0)
        };
        ClipQuery { camera: camera.to_string(), start, end, view: self.db_view.clone(), search: self.search_applied.clone(), tiles: false }
    }

    pub fn ensure_loaded(&mut self, ctx: &egui::Context, client: &BiClient, settings: &Settings, home_group: &str) {
        let cam = if self.camera_filter.is_empty() { home_group.to_string() } else { self.camera_filter.clone() };
        let key = (cam.clone(), self.db_view.clone(), self.search_applied.clone(), self.date_filter.then_some((self.date_start, self.date_end)));
        if self.loaded_for.as_ref() == Some(&key) {
            return;
        }
        self.loaded_for = Some(key);
        self.reload(ctx, client, &cam);
        let _ = settings;
    }

    fn reload(&mut self, ctx: &egui::Context, client: &BiClient, camera: &str) {
        let q = self.query(camera);
        let c = client.clone();
        self.error = None;
        self.job = Some(runtime::spawn(ctx, async move { c.clip_list(&q, |_| {}).await }));
        self.last_refresh = Instant::now();
    }

    pub fn set_view(&mut self, ctx: &egui::Context, client: &BiClient, settings: &mut Settings, view: &str, home_group: &str) {
        self.db_view = view.to_string();
        settings.set("ui3_current_dbView", view.to_string());
        self.loaded_for = None;
        self.ensure_loaded(ctx, client, settings, home_group);
    }

    pub fn release(&mut self, _gpu: &Gpu) {
        self.thumbs.clear();
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let mut last: Option<NaiveDate> = None;
        for (i, c) in self.clips.iter().enumerate() {
            let d = server_date(c.date * 1000, self.tz_offset_ms);
            if last != Some(d) {
                self.rows.push(Row::Date(d));
                last = Some(d);
            }
            self.rows.push(Row::Clip(i));
        }
    }

    /// Index of the clip with this alert/clip path.
    pub fn index_of(&self, path: &str) -> Option<usize> {
        self.clips.iter().position(|c| c.path == path)
    }

    fn poll(&mut self, ctx: &egui::Context, client: &BiClient, svc: &mut Services, home_group: &str) {
        if let Some(j) = &mut self.job
            && let Some(r) = j.poll()
        {
            self.job = None;
            match r {
                Ok(list) => {
                    // Keep selection for clips that still exist.
                    let paths: HashSet<&str> = list.iter().map(|c| c.path.as_str()).collect();
                    self.selected.retain(|p| paths.contains(p.as_str()));
                    self.clips = list;
                    self.rebuild_rows();
                }
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }
        // Debounced search (UI3 waits 1s after typing).
        if let Some(t) = self.search_changed_at
            && t.elapsed() > Duration::from_millis(1000)
        {
            self.search_changed_at = None;
            let query = self.search.trim().to_string();
            if query != self.search_applied {
                self.search_applied = query;
                self.loaded_for = None;
                self.ensure_loaded(ctx, client, svc.settings, home_group);
            }
        }
        // Periodic refresh to pick up new alerts (UI3 refreshes the list as clips arrive).
        if self.job.is_none() && !self.date_filter && self.last_refresh.elapsed() > Duration::from_secs(15) {
            let cam = if self.camera_filter.is_empty() { home_group.to_string() } else { self.camera_filter.clone() };
            self.reload(ctx, client, &cam);
        }
        // Thumbnails: up to 4 concurrent downloads.
        self.thumb_jobs.retain_mut(|(key, job)| match job.poll() {
            Some(Ok(img)) => {
                let tex = ctx.load_texture(format!("thumb-{key}"), img, egui::TextureOptions::LINEAR);
                self.thumbs.insert(key.clone(), Thumb::Ready(tex));
                false
            }
            Some(Err(_)) => {
                self.thumbs.insert(key.clone(), Thumb::Failed);
                false
            }
            None => true,
        });
        while self.thumb_jobs.len() < 4 {
            let Some(key) = self.thumb_queue.pop_front() else { break };
            let c = client.clone();
            let path = bi_proto::urls::thumbnail(&format!("@{key}"));
            self.thumb_jobs.push((key, runtime::spawn(ctx, async move { fetch_color_image(&c, &path, None).await })));
        }
        // Thumbnail cache bound.
        if self.thumbs.len() > 1500 {
            self.thumbs.retain(|_, t| matches!(t, Thumb::Loading));
        }
        // Results of actions (flag, delete, memo...).
        let mut changed = false;
        self.action_jobs.retain_mut(|j| match j.poll() {
            Some(Ok(msg)) => {
                if !msg.is_empty() {
                    svc.toasts.success(msg);
                }
                changed = true;
                false
            }
            Some(Err(e)) => {
                svc.toasts.error(format!("{e:#}"));
                changed = true;
                false
            }
            None => true,
        });
        if changed {
            let cam = if self.camera_filter.is_empty() { home_group.to_string() } else { self.camera_filter.clone() };
            self.reload(ctx, client, &cam);
        }
    }

    fn thumb(&mut self, rec_id: &str) -> Option<TextureHandle> {
        match self.thumbs.get(rec_id) {
            Some(Thumb::Ready(t)) => Some(t.clone()),
            Some(_) => None,
            None => {
                self.thumbs.insert(rec_id.to_string(), Thumb::Loading);
                self.thumb_queue.push_back(rec_id.to_string());
                None
            }
        }
    }

    /// Flags / protect toggles (`UpdateClipFlags`, ui3.js:31577).
    pub fn set_flag(&mut self, ctx: &egui::Context, client: &BiClient, items: Vec<ClipItem>, flag: i64, on: bool) {
        let updates = items.into_iter().map(|it| (it.path, if on { flag } else { 0 }, flag)).collect();
        self.update_flags(ctx, client, updates);
    }

    /// UI3 `ToggleAlertAiConfirmed`: confirming sets AI_CONFIRMED, unconfirming sets ALERT_CANCELLED; the two bits
    /// are mutually exclusive, so the mask always covers both.
    pub fn set_ai_confirmed(&mut self, ctx: &egui::Context, client: &BiClient, items: Vec<ClipItem>, confirm: bool) {
        let mask = flags::AI_CONFIRMED | flags::ALERT_CANCELLED;
        let updates = items
            .into_iter()
            .filter(|it| it.has_flag(flags::AI_CONFIRMED) != confirm)
            .map(|it| (it.path, if confirm { flags::AI_CONFIRMED } else { flags::ALERT_CANCELLED }, mask))
            .collect();
        self.update_flags(ctx, client, updates);
    }

    /// Sends `(path, flags, mask)` updates and applies them to the local list optimistically.
    fn update_flags(&mut self, ctx: &egui::Context, client: &BiClient, updates: Vec<(String, i64, i64)>) {
        if updates.is_empty() {
            return;
        }
        let c = client.clone();
        let sent = updates.clone();
        self.action_jobs.push(runtime::spawn(ctx, async move {
            for (path, f, m) in &sent {
                c.update_clip_flags(path, *f, *m).await?;
            }
            Ok(String::new())
        }));
        for (path, f, m) in updates {
            for c in self.clips.iter_mut().filter(|c| c.path == path) {
                c.flags = (c.flags & !m) | (f & m);
            }
        }
    }

    pub fn delete(&mut self, ctx: &egui::Context, client: &BiClient, items: Vec<ClipItem>) {
        let c = client.clone();
        let n = items.len();
        self.action_jobs.push(runtime::spawn(ctx, async move {
            for it in &items {
                c.delete_clip(&it.path, !it.is_clip()).await?;
            }
            Ok(format!("Deleted {n} item{}", if n == 1 { "" } else { "s" }))
        }));
        let gone: HashSet<String> = self.selected.drain().collect();
        self.clips.retain(|c| !gone.contains(&c.path));
        self.rebuild_rows();
    }

    pub fn selected_items(&self) -> Vec<ClipItem> {
        self.clips.iter().filter(|c| self.selected.contains(&c.path)).cloned().collect()
    }

    /// Clip adjacent to `path` in list order (`direction` +1 = older/down, -1 = newer/up).
    pub fn neighbor(&self, path: &str, direction: i32) -> Option<ClipItem> {
        let i = self.index_of(path)? as i32 + direction;
        if i < 0 {
            return None;
        }
        self.clips.get(i as usize).cloned()
    }

    pub fn select_only(&mut self, path: &str) {
        self.selected.clear();
        self.selected.insert(path.to_string());
        self.focus = self.index_of(path);
        if let Some(i) = self.focus {
            self.scroll_to = self.rows.iter().position(|r| matches!(r, Row::Clip(j) if *j == i));
        }
    }
}

pub async fn fetch_color_image(c: &BiClient, path: &str, max_h: Option<u32>) -> Result<egui::ColorImage> {
    let r = c.get_bytes(path).await?;
    anyhow::ensure!(r.status < 400 && r.is_jpeg(), "HTTP {}", r.status);
    let body = r.body;
    let img = tokio::task::spawn_blocking(move || decode_jpeg_rgba(&body)).await??;
    let _ = max_h;
    Ok(egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.rgba))
}

pub fn server_date(utc_ms: i64, tz_offset_ms: i64) -> NaiveDate {
    chrono::DateTime::from_timestamp_millis(utc_ms + tz_offset_ms).map(|d| d.naive_utc().date()).unwrap_or_default()
}

pub fn server_time_str(utc_ms: i64, tz_offset_ms: i64, h24: bool) -> String {
    chrono::DateTime::from_timestamp_millis(utc_ms + tz_offset_ms)
        .map(|d| if h24 { d.naive_utc().format("%H:%M:%S").to_string() } else { d.naive_utc().format("%I:%M:%S %p").to_string() })
        .unwrap_or_default()
}

/// UI3 `GetDateDisplayStr(date, true)`: "Today, October 14" / "October 13, 2026", plus the weekday.
fn date_label(d: NaiveDate, today: NaiveDate) -> (String, String) {
    let main = if d == today { format!("Today, {}", d.format("%B %-d")) } else { d.format("%B %-d, %Y").to_string() };
    (main, format!("({})", d.format("%A")))
}

/// `GetClipDurStrFromMs`: "S" for snapshots, else m:ss or h:mm:ss.
fn clip_dur_str(c: &ClipItem) -> String {
    if c.is_snapshot() {
        return "S".into();
    }
    let s = c.rough_length_ms().max(0) / 1000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Clip tile icons (`clipIcons.setIcon`, ui3.js:40990), gated by the `ui3_clipicon_*` settings.
fn clip_icons(c: &ClipItem, s: &Settings) -> Vec<(Icon, Option<Color32>, &'static str)> {
    let mut v = Vec::new();
    let f = c.flags;
    let has = |x| flags::has(f, x);
    let ai = Some(Color32::from_rgb(0xFF, 0x5F, 0x00));
    if s.bool("ui3_clipicon_trigger_sentry") && has(flags::AI_CONFIRMED) {
        v.push((icons::CB_CHECKED, None, "AI-confirmed (or manually confirmed) alert"));
    }
    if s.bool("ui3_clipicon_ai_person") && has(flags::AI_PERSON) {
        v.push((icons::MAN, ai, "AI detected a person"));
    }
    if s.bool("ui3_clipicon_ai_vehicle") && has(flags::AI_VEHICLE) {
        v.push((icons::DIRECTIONS_CAR, ai, "AI detected a vehicle"));
    }
    if s.bool("ui3_clipicon_ai_wildlife") && has(flags::AI_WILDLIFE) {
        v.push((icons::WILDLIFE, ai, "AI detected wildlife"));
    }
    if s.bool("ui3_clipicon_alert_cancelled") && has(flags::ALERT_CANCELLED) {
        v.push((icons::HOLD_PROFILE, None, "Alert was cancelled"));
    }
    if s.bool("ui3_clipicon_trigger_motion") && has(flags::ALERT_MOTION) {
        v.push((icons::RUN, None, "Triggered by motion detection"));
    }
    if s.bool("ui3_clipicon_trigger_audio") && has(flags::ALERT_AUDIO) {
        v.push((icons::VOLUME_UP, None, "Triggered by audio"));
    }
    if s.bool("ui3_clipicon_trigger_external") && (has(flags::ALERT_EXTERNAL)) {
        v.push((icons::ALERT1, None, "Triggered by external source such as DIO or manual trigger"));
    }
    if s.bool("ui3_clipicon_trigger_group") && has(flags::ALERT_GROUP) {
        v.push((icons::QUILT, None, "The group was triggered"));
    }
    if s.bool("ui3_clipicon_clip_audio") && has(flags::AUDIO) {
        v.push((icons::VOLUME_UP, None, "Clip has audio"));
    }
    if s.bool("ui3_clipicon_clip_backingup") && has(flags::ARCHIVE) {
        v.push((icons::CLOUD_UPLOADING, None, "Clip is currently being backed up"));
    }
    if s.bool("ui3_clipicon_clip_backup") && has(flags::ARCHIVED) {
        v.push((icons::CLOUD_UPLOADED, None, "Clip has been backed up"));
    }
    if s.bool("ui3_clipicon_protect") && has(flags::PROTECTED) {
        v.push((icons::LOCK, None, "Item is protected"));
    }
    if s.bool("ui3_clipicon_flag") && has(flags::FLAGGED) {
        v.push((icons::FLAG, None, "Item is flagged"));
    }
    if s.bool("ui3_clipicon_is_recording") && has(flags::RECORDING) {
        v.push((icons::STOPLIGHT, Some(style::REC_RED), "Clip is still recording"));
    }
    if s.bool("ui3_clipicon_nosignal") && has(flags::ALERT_NOSIGNAL) {
        v.push((icons::ERROR, None, "Camera had no signal"));
    }
    v
}

const LEFT_W: f32 = 190.0;

/// "All Recent" or the selected date range (UI3 `#dateRangeLabel`).
fn date_range_label(p: &ClipsPanel) -> String {
    if !p.date_filter {
        "All Recent".into()
    } else if p.date_start == p.date_end {
        p.date_start.format("%b %-d, %Y").to_string()
    } else {
        format!("{} - {}", p.date_start.format("%b %-d, %Y"), p.date_end.format("%b %-d, %Y"))
    }
}

/// Most tiles that can be selected at once (`clipTileSelectLimit`, ui3.js:13566).
const SELECT_LIMIT: usize = 1000;

pub fn show_list(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services) {
    let ctx = ui.ctx().clone();
    view.clips.tz_offset_ms = view.session.login.tz_offset_ms();
    let home = view.home_group.clone();
    // Follow the camera shown in the video area: a single camera filters the list; returning to a group clears it.
    let shown = view.current_source_id().and_then(|id| view.camera(id)).filter(|c| !c.is_group() && !c.is_cycle()).map(|c| c.id.clone()).unwrap_or_default();
    // Opening a clip leaves the filter alone (UI3 only follows live/timeline sources, ui3.js:13578).
    let source_is_live = matches!(view.content, Content::Live { .. } | Content::Timeline { .. });
    if source_is_live && shown != view.clips.followed_camera {
        view.clips.camera_filter = shown.clone();
        view.clips.followed_camera = shown;
    }
    view.clips.ensure_loaded(&ctx, &view.client, svc.settings, &home);
    view.clips.poll(&ctx, &view.client, svc, &home);
    let h24 = svc.settings.bool("ui3_time24hour");
    let th = style::t();
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

    // ---- #layoutLeftRecordingsControls ----
    ui.add_space(10.0);
    let mut open_filters = svc.settings.bool("ui3_collapsible_filterRecordings");
    egui::Frame::new().inner_margin(egui::Margin { left: 31, right: 30, top: 0, bottom: 7 }).show(ui, |ui| {
        ui.set_width(sidebar::WIDTH - 61.0);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        w::left_heading(ui, "Filter by:", &mut open_filters);
        if !open_filters {
            return;
        }
        ui.add_space(4.0);
        sidebar::current_group_dropdown(view, ui, svc, &ctx, "clipgroup");
        ui.add_space(4.0);
        // #dateRange
        let (rect, r) = ui.allocate_exact_size(vec2(LEFT_W, 46.0), Sense::click());
        let color = w::state_color(&r, th.text);
        if r.hovered() {
            ui.painter().rect_filled(rect, 0.0, th.ui_hover);
        }
        ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.0, th.button), egui::StrokeKind::Inside);
        w::paint_icon(ui, icons::CALENDAR2, egui::Rect::from_min_size(rect.min + vec2(10.0, 13.0), vec2(20.0, 20.0)), color);
        ui.painter().with_clip_rect(rect.shrink(2.0)).text(rect.left_center() + vec2(38.0, 0.0), egui::Align2::LEFT_CENTER, date_range_label(&view.clips), egui::FontId::proportional(16.0), color);
        let before = (view.clips.date_filter, view.clips.date_start, view.clips.date_end);
        egui::Popup::menu(&r).frame(w::list_frame().inner_margin(egui::Margin::same(10))).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 8.0);
            ui.set_min_width(230.0);
            ui.label(egui::RichText::new("Date range").size(16.0).color(th.text_active));
            egui::Grid::new("daterange").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                ui.label("From");
                if date_picker(ui, &mut view.clips.date_start, "ds") {
                    view.clips.date_filter = true;
                }
                ui.end_row();
                ui.label("To");
                if date_picker(ui, &mut view.clips.date_end, "de") {
                    view.clips.date_filter = true;
                }
                ui.end_row();
            });
            ui.horizontal(|ui| {
                if ui.button("Today").clicked() {
                    let t = server_date(chrono::Utc::now().timestamp_millis(), view.clips.tz_offset_ms);
                    view.clips.date_filter = true;
                    view.clips.date_start = t;
                    view.clips.date_end = t;
                    ui.close();
                }
                if ui.button("Last 7 days").clicked() {
                    let t = server_date(chrono::Utc::now().timestamp_millis(), view.clips.tz_offset_ms);
                    view.clips.date_filter = true;
                    view.clips.date_start = t - chrono::Days::new(6);
                    view.clips.date_end = t;
                    ui.close();
                }
                if ui.button("All Recent").clicked() {
                    view.clips.date_filter = false;
                    ui.close();
                }
            });
        });
        if before != (view.clips.date_filter, view.clips.date_start, view.clips.date_end) {
            if view.clips.date_end < view.clips.date_start {
                view.clips.date_end = view.clips.date_start;
            }
            view.clips.ensure_loaded(&ctx, &view.client, svc.settings, &home);
        }
        ui.add_space(4.0);
        // dbView dropdown
        let cur = DB_VIEWS.iter().find(|(id, _)| *id == view.clips.db_view).map(|(_, n)| *n).unwrap_or("All clips");
        let mut chosen = None;
        w::dropdown_box(ui, "dbview", cur, LEFT_W, 39.0, true, |ui| {
            for (id, name) in DB_VIEWS {
                if w::list_item(ui, None, name, *id == view.clips.db_view, w::ItemKind::Normal).clicked() {
                    chosen = Some(*id);
                    ui.close();
                }
            }
        });
        if let Some(v) = chosen {
            view.clips.set_view(&ctx, &view.client, svc.settings, v, &home);
        }
        // .clipFilterSearchContainer
        if svc.settings.bool("ui3_showClipListFilterSearch") {
            ui.add_space(4.0);
            let (rect, _) = ui.allocate_exact_size(vec2(LEFT_W, 39.0), Sense::hover());
            ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.0, th.button), egui::StrokeKind::Inside);
            w::paint_icon(ui, icons::SEARCH, egui::Rect::from_min_size(rect.min + vec2(11.0, 11.0), vec2(18.0, 18.0)), th.text);
            let edit_rect = egui::Rect::from_min_max(rect.min + vec2(36.0, 1.0), rect.max - vec2(4.0, 1.0));
            let r = ui.put(
                edit_rect,
                egui::TextEdit::singleline(&mut view.clips.search).hint_text("search").frame(egui::Frame::NONE).font(egui::FontId::proportional(16.0)).text_color(th.text).vertical_align(egui::Align::Center),
            );
            if r.changed() {
                view.clips.search_changed_at = Some(Instant::now());
            }
            // Enter / leaving the box searches immediately (ui3.js:11589).
            if r.lost_focus() {
                view.clips.search_changed_at = Some(Instant::now() - Duration::from_millis(1001));
            }
        }
    });
    if open_filters != svc.settings.bool("ui3_collapsible_filterRecordings") {
        svc.settings.set("ui3_collapsible_filterRecordings", if open_filters { "1" } else { "0" });
    }

    // ---- Virtualized list (#clipsbody) with the sticky top date tile ----
    let large = svc.settings.bool("ui3_cliplist_larger_thumbnails");
    let tile_h = if large { 137.0 } else { 53.0 };
    let date_h = 27.0;
    let today = server_date(chrono::Utc::now().timestamp_millis(), view.clips.tz_offset_ms);
    let rows = view.clips.rows.clone();
    // Row y offsets.
    let mut ys = Vec::with_capacity(rows.len() + 1);
    let mut y = 0.0;
    for (k, r) in rows.iter().enumerate() {
        ys.push(y);
        y += match r {
            // The sticky #clipListTopDate tile already shows the first date.
            Row::Date(_) if k == 0 => 0.0,
            Row::Date(_) => date_h,
            Row::Clip(_) => tile_h,
        };
    }
    let total_h = y;
    let list_rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(list_rect, 0.0, th.cliplist);

    // Sticky date tile for the first visible row.
    let top_tile = egui::Rect::from_min_size(list_rect.min, vec2(list_rect.width(), 20.0));
    let scroll_id = ui.id().with("clipscroll");
    let offset: f32 = ui.data(|d| d.get_temp(scroll_id).unwrap_or(0.0));
    let first = ys.partition_point(|y| *y <= offset).saturating_sub(1);
    let top_date = rows.iter().take(first + 1).rev().find_map(|r| if let Row::Date(d) = r { Some(*d) } else { None });
    ui.painter().rect_filled(top_tile, 0.0, th.datetile);
    let status = if view.clips.job.is_some() && view.clips.clips.is_empty() {
        "Loading…".to_string()
    } else if let Some(e) = &view.clips.error {
        e.clone()
    } else if let Some(d) = top_date {
        date_label(d, today).0
    } else if view.clips.clips.is_empty() {
        "No recordings were found matching your search criteria.".into()
    } else {
        "...".into()
    };
    let tr = ui.interact(top_tile, ui.id().with("topdate"), Sense::click()).on_hover_text("Click to scroll to the top");
    ui.painter().with_clip_rect(top_tile).text(top_tile.left_center() + vec2(21.0, 0.0), egui::Align2::LEFT_CENTER, status, egui::FontId::proportional(12.0), th.text_active);
    if !view.clips.selected.is_empty() {
        ui.painter().text(top_tile.right_center() - vec2(22.0, 0.0), egui::Align2::RIGHT_CENTER, format!("{} selected", view.clips.selected.len()), egui::FontId::proportional(12.0), th.text_active);
    }
    let mut scroll_to_top = tr.clicked();

    let body = egui::Rect::from_min_max(pos2(list_rect.left(), top_tile.bottom()), list_rect.max);
    let mut body_ui = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
    let mut open: Option<ClipItem> = None;
    let mut hovered_clip: Option<(ClipItem, egui::Rect)> = None;
    let playing_path = match &view.content {
        Content::Clip { key } => Some(key.clone()),
        _ => None,
    };
    let mut sa = egui::ScrollArea::vertical().id_salt("clipsbody").auto_shrink([false, false]);
    if let Some(r) = view.clips.scroll_to.take()
        && let Some(y) = ys.get(r)
        && (*y < offset || *y + tile_h > offset + body.height())
    {
        sa = sa.vertical_scroll_offset((*y - body.height() / 3.0).max(0.0));
    }
    if std::mem::take(&mut scroll_to_top) {
        sa = sa.vertical_scroll_offset(0.0);
    }
    let out = sa.show_viewport(&mut body_ui, |ui, vp| {
        ui.set_height(total_h);
        let origin = ui.min_rect().min;
        let start = ys.partition_point(|y| *y + tile_h < vp.min.y).saturating_sub(1);
        for ri in start..rows.len() {
            let ry = ys[ri];
            if ry > vp.max.y {
                break;
            }
            match rows[ri] {
                Row::Date(_) if ri == 0 => {}
                Row::Date(d) => {
                    let r = egui::Rect::from_min_size(origin + vec2(15.0, ry + 3.0), vec2(219.0, 20.0));
                    ui.painter().rect_filled(r, 0.0, th.datetile);
                    let (main, wd) = date_label(d, today);
                    ui.painter().text(r.left_center() + vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, main, egui::FontId::proportional(12.0), th.text_active);
                    ui.painter().text(r.right_center() - vec2(6.0, 0.0), egui::Align2::RIGHT_CENTER, wd, egui::FontId::proportional(12.0), th.text_active);
                }
                Row::Clip(i) => {
                    let c = view.clips.clips[i].clone();
                    let rect = egui::Rect::from_min_size(origin + vec2(15.0, ry), vec2(219.0, tile_h));
                    let resp = ui.interact(rect, ui.id().with(("clip", i)), Sense::click());
                    let selected = view.clips.selected.contains(&c.path) || playing_path.as_deref() == Some(c.path.as_str());
                    let hovered = resp.hovered();
                    let p = ui.painter_at(rect);
                    let (bg, fg) = match (selected, hovered) {
                        (true, true) => (Some(Color32::from_rgb(0x59, 0xB3, 0xF9)), Color32::BLACK),
                        (true, false) => (Some(Color32::from_rgb(0x21, 0x96, 0xF3)), Color32::BLACK),
                        (false, true) => (Some(Color32::from_rgb(0x00, 0x6E, 0xC5)), Color32::from_rgb(0xDD, 0xDD, 0xDD)),
                        _ => (None, th.text),
                    };
                    if let Some(bg) = bg {
                        p.rect_filled(rect, 0.0, bg);
                    }
                    let time_color = if bg.is_some() { fg } else { th.text_active };
                    let icon_color = if selected { th.badge_selected } else if hovered { th.badge_hover } else { th.badge };
                    let color = style::rgb3(bi_proto::api::bi_color_to_rgb(c.color));
                    let cam_name = view.camera(&c.camera).map(|c| c.name.clone()).unwrap_or_else(|| c.camera.clone());
                    let time = server_time_str(c.date * 1000, view.clips.tz_offset_ms, h24);
                    let (img_box, bar, desc_pos) = if large {
                        let img = egui::Rect::from_min_size(rect.min + vec2(16.0, 4.0), vec2(187.0, 106.0));
                        (img, egui::Rect::from_min_size(pos2(img.left(), img.bottom() + 2.0), vec2(187.0, 4.0)), pos2(img.left(), img.bottom() + 9.0))
                    } else {
                        let img = egui::Rect::from_min_size(pos2(rect.left() + 16.0, rect.center().y - 22.5), vec2(80.0, 45.0));
                        (img, egui::Rect::from_min_size(pos2(img.right() + 4.0, img.top()), vec2(4.0, 45.0)), pos2(img.right() + 21.0, rect.center().y - 15.0))
                    };
                    if large {
                        p.rect_filled(img_box, 0.0, Color32::BLACK);
                    }
                    let rec_id = c.rec_id().to_string();
                    let img_rect = match view.clips.thumb(&rec_id) {
                        Some(t) => {
                            let fit = bi_render::fit_rect([t.size()[0] as u32, t.size()[1] as u32], img_box);
                            // Small tiles right-align the thumbnail (`margin-left: auto`).
                            let fit = if large { fit } else { fit.translate(vec2(img_box.right() - fit.right(), 0.0)) };
                            p.image(t.id(), fit, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                            fit
                        }
                        None => {
                            p.rect_filled(img_box, 0.0, Color32::from_gray(24));
                            img_box
                        }
                    };
                    // .clipdur / .clipmemo overlays.
                    let small = egui::FontId::proportional(9.0);
                    let dur = p.layout_no_wrap(clip_dur_str(&c), small.clone(), th.text_active);
                    let dr = egui::Rect::from_min_size(img_rect.right_bottom() - vec2(dur.size().x + 6.0, dur.size().y + 4.0), dur.size() + vec2(4.0, 2.0));
                    p.rect_filled(dr, 0.0, Color32::from_black_alpha(153));
                    p.galley(dr.min + vec2(2.0, 1.0), dur, th.text_active);
                    let memo = [c.plate.as_str(), c.memo.as_str()].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join("; ");
                    if !memo.is_empty() {
                        let g = p.layout(memo, small, th.text_active, if large { 179.0 } else { 72.0 });
                        let mr = egui::Rect::from_min_size(img_rect.min + vec2(2.0, 2.0), g.size() + vec2(4.0, 2.0));
                        p.rect_filled(mr, 0.0, Color32::from_black_alpha(153));
                        p.galley(mr.min + vec2(2.0, 1.0), g, th.text_active);
                    }
                    p.rect_filled(bar, 0.0, color);
                    // .clipIconWrapper: bottom-right row of 20px icons (a column on the right for large tiles).
                    let mut tips = Vec::new();
                    let ic = clip_icons(&c, svc.settings);
                    for (k, (icon, col, tip)) in ic.iter().enumerate() {
                        let r = if large {
                            egui::Rect::from_min_size(pos2(rect.right() - 16.0, rect.top() + 4.0 + k as f32 * 17.0), vec2(16.0, 16.0))
                        } else {
                            egui::Rect::from_min_size(pos2(rect.right() - 21.0 * (ic.len() - k) as f32, rect.bottom() - 26.0), vec2(20.0, 20.0))
                        };
                        w::paint_icon(ui, *icon, r.shrink(1.0), col.unwrap_or(icon_color));
                        tips.push(*tip);
                    }
                    if large {
                        p.text(desc_pos, egui::Align2::LEFT_TOP, &time, egui::FontId::proportional(14.0), time_color);
                        p.text(pos2(img_box.right(), desc_pos.y + 2.0), egui::Align2::RIGHT_TOP, &cam_name, egui::FontId::proportional(11.0), fg);
                    } else {
                        p.with_clip_rect(egui::Rect::from_min_size(desc_pos, vec2(103.0, 40.0))).text(desc_pos, egui::Align2::LEFT_TOP, &time, egui::FontId::proportional(14.0), time_color);
                        // Keep the camera name clear of the icon row.
                        let icons_left = rect.right() - 21.0 * ic.len() as f32 - 3.0;
                        let name_clip = egui::Rect::from_min_max(desc_pos, pos2(icons_left.min(desc_pos.x + 103.0), desc_pos.y + 40.0));
                        let g = p.layout_no_wrap(cam_name.clone(), egui::FontId::proportional(11.0), fg);
                        let mut name = cam_name.clone();
                        if g.size().x > name_clip.width() - 1.0 {
                            while !name.is_empty() && p.layout_no_wrap(format!("{name}…"), egui::FontId::proportional(11.0), fg).size().x > name_clip.width() - 1.0 {
                                name.pop();
                            }
                            name.push('…');
                        }
                        p.with_clip_rect(name_clip).text(desc_pos + vec2(1.0, 18.0), egui::Align2::LEFT_TOP, &name, egui::FontId::proportional(11.0), fg);
                    }
                    let resp = if tips.is_empty() { resp } else { resp.on_hover_text(tips.join("\n")) };
                    if hovered {
                        hovered_clip = Some((c.clone(), rect));
                    }
                    if resp.clicked() {
                        let m = ui.input(|i| i.modifiers);
                        if m.ctrl {
                            if !view.clips.selected.remove(&c.path) {
                                if view.clips.selected.len() >= SELECT_LIMIT {
                                    svc.toasts.warn(format!("For performance reasons, you can't select more than {SELECT_LIMIT} items at once."));
                                } else {
                                    view.clips.selected.insert(c.path.clone());
                                }
                            }
                            view.clips.anchor = Some(i);
                        } else if m.shift
                            && let Some(a) = view.clips.anchor
                        {
                            let (lo, hi) = (a.min(i), a.max(i));
                            view.clips.selected = view.clips.clips[lo..=hi].iter().take(SELECT_LIMIT).map(|c| c.path.clone()).collect();
                        } else {
                            view.clips.selected.clear();
                            view.clips.selected.insert(c.path.clone());
                            view.clips.anchor = Some(i);
                            open = Some(c.clone());
                        }
                        view.clips.focus = Some(i);
                    }
                    resp.context_menu(|ui| {
                        if !view.clips.selected.contains(&c.path) {
                            view.clips.selected.clear();
                            view.clips.selected.insert(c.path.clone());
                        }
                        clip_context_menu(view, ui, svc, &c);
                    });
                }
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(scroll_id, out.state.offset.y));

    // Hover preview animation + big thumbnail (ui3_clipPreviewEnabled, ui3_cliplist_mouseover_thumbnails).
    update_preview(view, &ctx, svc, hovered_clip);

    if let Some(c) = open {
        crate::views::playback::open_clip(view, &ctx, svc, c);
    }
}

fn date_picker(ui: &mut egui::Ui, d: &mut NaiveDate, id: &str) -> bool {
    let mut j = jiff::civil::Date::new(d.year() as i16, d.month() as i8, d.day() as i8).unwrap_or_default();
    if ui.add(egui_extras::DatePickerButton::new(&mut j).id_salt(id).calendar_week(false)).changed()
        && let Some(n) = NaiveDate::from_ymd_opt(j.year() as i32, j.month() as u32, j.day() as u32)
    {
        *d = n;
        return true;
    }
    false
}

fn update_preview(view: &mut MainView, ctx: &egui::Context, svc: &mut Services, hovered: Option<(ClipItem, egui::Rect)>) {
    let enabled = svc.settings.bool("ui3_cliplist_mouseover_thumbnails");
    let Some((clip, rect)) = hovered.filter(|_| enabled) else {
        view.clips.preview = None;
        return;
    };
    let rec_id = clip.rec_id().to_string();
    if view.clips.preview.as_ref().is_none_or(|p| p.rec_id != rec_id) {
        view.clips.preview = Some(Preview { rec_id: rec_id.clone(), frame: 0, loops: 0, started: Instant::now(), job: None, tex: None, next_at: Instant::now() + Duration::from_millis(250) });
    }
    let n_frames = (svc.settings.f64("ui3_clip_preview_num_frames") as usize).clamp(2, 100);
    let animate = svc.settings.bool("ui3_clipPreviewEnabled") && clip.duration_ms() >= 500;
    // ui3_clip_preview_speed is 1..10; UI3 maps it to a per-frame delay.
    let speed = svc.settings.f64("ui3_clip_preview_speed").clamp(1.0, 10.0);
    let delay = Duration::from_millis((1100.0 - speed * 100.0) as u64);
    let client = view.client.clone();
    let cam_name = view.camera(&clip.camera).map(|c| c.name.clone()).unwrap_or(clip.camera.clone());
    let tz_off = view.clips.tz_offset_ms;
    let p = view.clips.preview.as_mut().unwrap();
    if let Some(j) = &mut p.job
        && let Some(r) = j.poll()
    {
        p.job = None;
        if let Ok(t) = r {
            p.tex = Some(t);
        }
        p.frame += 1;
        if p.frame >= n_frames {
            p.frame = 0;
            p.loops += 1;
        }
        p.next_at = Instant::now() + delay;
    }
    let want_more = animate && p.loops < 3;
    if p.job.is_none() && Instant::now() >= p.next_at && (want_more || p.tex.is_none()) {
        let dur = clip.duration_ms().max(1);
        let t = if animate { (p.frame as i64 * dur) / n_frames as i64 } else { 0 };
        let path = bi_proto::urls::clip_preview_frame(&format!("@{rec_id}"), t, 240);
        let c2 = client.clone();
        let ctx2 = ctx.clone();
        let key = format!("preview-{rec_id}-{}", p.frame);
        p.job = Some(runtime::spawn(ctx, async move {
            let img = fetch_color_image(&c2, &path, Some(240)).await?;
            Ok(ctx2.load_texture(key, img, egui::TextureOptions::LINEAR))
        }));
    }
    ctx.request_repaint_after(Duration::from_millis(50));
    // Big thumbnail popup next to the list.
    let (p_tex, p_frame, elapsed) = (p.tex.clone(), p.frame, p.started.elapsed());
    let tex = p_tex.or_else(|| match view.clips.thumbs.get(&rec_id) {
        Some(Thumb::Ready(t)) => Some(t.clone()),
        _ => None,
    });
    if let Some(t) = tex
        && elapsed > Duration::from_millis(200)
    {
        let size = t.size_vec2();
        let h = 240.0f32;
        let w = h * size.x / size.y.max(1.0);
        let label = format!(
            "{}  {}",
            cam_name,
            server_time_str(clip.date * 1000 + if animate { (p_frame as i64 * clip.duration_ms()) / n_frames as i64 } else { 0 }, tz_off, svc.settings.bool("ui3_time24hour"))
        );
        egui::Area::new(egui::Id::new("bigthumb")).order(egui::Order::Tooltip).fixed_pos(egui::pos2(rect.right() + 260.0, (rect.top() - 60.0).max(40.0))).interactable(false).show(ctx, |ui| {
            egui::Frame::new().fill(Color32::BLACK).stroke(egui::Stroke::new(1.0, style::t().button)).inner_margin(egui::Margin::same(2)).show(ui, |ui| {
                ui.add(egui::Image::new((t.id(), vec2(w, h))));
                ui.label(RichText::new(label).size(12.0).color(style::t().text_active));
            });
        });
    }
}

fn clip_context_menu(view: &mut MainView, ui: &mut egui::Ui, svc: &mut Services, c: &ClipItem) {
    let ctx = ui.ctx().clone();
    let items = view.clips.selected_items();
    let n = items.len();
    let plural = if n > 1 { format!(" ({n})") } else { String::new() };
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    ui.set_min_width(220.0);
    let item = |ui: &mut egui::Ui, icon: Icon, label: &str| w::list_item(ui, Some(icon), label, false, w::ItemKind::Normal);
    if item(ui, icons::PLAY, "Open").clicked() {
        crate::views::playback::open_clip(view, &ctx, svc, c.clone());
        ui.close();
    }
    w::list_separator(ui);
    let all_flagged = items.iter().all(|i| i.has_flag(flags::FLAGGED));
    if item(ui, icons::FLAG, &format!("{}{plural}", if all_flagged { "Unflag" } else { "Flag" })).clicked() {
        view.clips.set_flag(&ctx, &view.client, items.clone(), flags::FLAGGED, !all_flagged);
        ui.close();
    }
    let all_prot = items.iter().all(|i| i.has_flag(flags::PROTECTED));
    if item(ui, icons::LOCK, &format!("{}{plural}", if all_prot { "Unprotect" } else { "Protect" })).clicked() {
        view.clips.set_flag(&ctx, &view.client, items.clone(), flags::PROTECTED, !all_prot);
        ui.close();
    }
    if items.iter().all(|i| !i.is_clip()) {
        let all_conf = items.iter().all(|i| i.has_flag(flags::AI_CONFIRMED));
        if item(ui, icons::CB_CHECKED, &format!("{}{plural}", if all_conf { "Unmark as AI-confirmed" } else { "Mark as AI-confirmed" })).clicked() {
            view.clips.set_ai_confirmed(&ctx, &view.client, items.clone(), !all_conf);
            ui.close();
        }
    }
    if n == 1 && item(ui, icons::COPY, "Copy clip URL").clicked() {
        ctx.copy_text(format!("{}ui3.htm?rec={}", view.client.base_url(), c.rec_id()));
        svc.toasts.info("URL copied to the clipboard");
        ui.close();
    }
    if n == 1 && item(ui, icons::EDIT, "Edit Memo").clicked() {
        view.clips.memo_edit = Some((c.path.clone(), c.memo.clone()));
        ui.close();
    }
    w::list_separator(ui);
    if item(ui, icons::DOWNLOAD, &format!("Download{plural}")).clicked() {
        crate::views::dialogs::download_clips(view, svc, items.clone());
        ui.close();
    }
    if n == 1 && item(ui, icons::LAUNCH, "Export").clicked() {
        view.dialogs.open(crate::views::dialogs::Dialog::Export(Box::new(c.clone())));
        ui.close();
    }
    if n == 1 && item(ui, icons::INFORMATION, "Properties").clicked() {
        view.dialogs.open(crate::views::dialogs::Dialog::ClipProperties(Box::new(c.clone())));
        ui.close();
    }
    let mut larger = svc.settings.bool("ui3_cliplist_larger_thumbnails");
    if item(ui, icons::IMAGE_LARGER, if larger { "Shrink Thumbnails" } else { "Larger Thumbnails" }).clicked() {
        larger = !larger;
        svc.settings.set("ui3_cliplist_larger_thumbnails", if larger { "1" } else { "0" });
        ui.close();
    }
    if svc.settings.can_delete_clips(view.session.login.admin) {
        w::list_separator(ui);
        if item(ui, icons::TRASH, &format!("Delete{plural}")).clicked() {
            if svc.settings.delete_needs_confirm(items.len()) {
                view.clips.delete_confirm = Some(items);
            } else {
                view.clips.delete(&ctx, &view.client, items);
            }
            ui.close();
        }
    }
}

pub fn memo_and_delete_dialogs(view: &mut MainView, ctx: &egui::Context, svc: &mut Services) {
    let mut close_memo = false;
    if let Some((path, memo)) = &mut view.clips.memo_edit {
        w::dialog("Memo", "clip_memo").resizable(false).show(ctx, |ui| {
            ui.text_edit_singleline(memo);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    let (c, p, m) = (view.client.clone(), path.clone(), memo.clone());
                    view.clips.action_jobs.push(runtime::spawn(ctx, async move {
                        c.update_clip_memo(&p, &m).await?;
                        Ok("Memo saved".to_string())
                    }));
                    close_memo = true;
                }
                if ui.button("Cancel").clicked() {
                    close_memo = true;
                }
            });
        });
    }
    if close_memo {
        view.clips.memo_edit = None;
    }
    let mut confirm = None;
    if let Some(items) = &view.clips.delete_confirm {
        egui::Modal::new(egui::Id::new("delconfirm")).show(ctx, |ui| {
            ui.label(format!("Delete {} item{}? This cannot be undone.", items.len(), if items.len() == 1 { "" } else { "s" }));
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Delete").color(Color32::LIGHT_RED)).clicked() {
                    confirm = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    confirm = Some(false);
                }
            });
        });
    }
    if let Some(yes) = confirm {
        let items = view.clips.delete_confirm.take().unwrap();
        if yes {
            // Deleting the clip that is open in the video area returns to live video.
            let open_deleted = view.playback.clip.as_ref().is_some_and(|c| items.iter().any(|i| i.path == c.path));
            view.clips.delete(ctx, &view.client, items);
            if open_deleted {
                view.playback.close();
                let home = view.home_group.clone();
                view.load_live(ctx, svc.settings, Some(svc.audio), &home);
            }
        }
    }
}
