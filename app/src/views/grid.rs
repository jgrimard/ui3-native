//! Client-side grid (native extension, not in UI3): instead of the server-composited group image, open one
//! direct-to-wire stream per camera (sub stream by default) and lay the tiles out locally using the group's
//! rectangles. Each camera keeps its own codec (H.265 stays H.265) and is decoded on the GPU; Blue Iris does no
//! compositing or transcoding for the group.

use crate::app::Services;
use crate::gpu::Gpu;
use crate::player::{OpenParams, PlayMode, Player};
use crate::views::main_view::MainView;
use crate::views::style;
use bi_proto::BiClient;
use bi_proto::api::CameraInfo;
use bi_proto::profile::{SupportedCodecs, native_passthrough_profile};
use bi_proto::urls::{self, AudioRequest};
use bi_render::VideoTexture;
use egui::{Align2, Color32, FontId, Rect, Sense, pos2, vec2};

pub struct Tile {
    pub id: String,
    player: Player,
    tex: Option<VideoTexture>,
    /// Normalized (0..1) rect within the group layout.
    norm: Rect,
}

#[derive(Default)]
pub struct Grid {
    pub group: Option<String>,
    pub tiles: Vec<Tile>,
    aspect: f32,
}

impl Grid {
    pub fn is_active(&self) -> bool {
        self.group.is_some()
    }

    pub fn open(&mut self, ctx: &egui::Context, client: &BiClient, group: &CameraInfo, cams: &[CameraInfo], svc: &mut Services) {
        self.close(svc.gpu);
        let members = group.group.clone().unwrap_or_default();
        let rects = group.rects.clone().unwrap_or_default();
        // Normalize by the bounding box of the member rectangles (robust to group size mismatches).
        let gw = rects.iter().map(|r| r[2]).max().unwrap_or(group.width as i32).max(1) as f32;
        let gh = rects.iter().map(|r| r[3]).max().unwrap_or(group.height as i32).max(1) as f32;
        let n = members.len().max(1);
        let cols = (n as f32).sqrt().ceil() as usize;
        let rows = n.div_ceil(cols);
        self.aspect = if rects.len() == members.len() { gw / gh } else { 16.0 / 9.0 };
        let sub = svc.settings.native.grid_use_substream;
        for (i, id) in members.iter().enumerate() {
            let Some(cam) = cams.iter().find(|c| c.id.eq_ignore_ascii_case(id)) else { continue };
            if !cam.is_enabled || cam.hidden {
                continue;
            }
            let norm = match rects.get(i) {
                Some(r) if rects.len() == members.len() => Rect::from_min_max(pos2(r[0] as f32 / gw, r[1] as f32 / gh), pos2(r[2] as f32 / gw, r[3] as f32 / gh)),
                _ => {
                    let (c, r) = ((i % cols) as f32, (i / cols) as f32);
                    Rect::from_min_size(pos2(c / cols as f32, r / rows as f32), vec2(1.0 / cols as f32, 1.0 / rows as f32))
                }
            };
            // Request the camera's own resolution so Blue Iris can pass the stream straight through.
            let p = native_passthrough_profile();
            let native = if sub && cam.width2 > 0 { (cam.width2 as i32, cam.height2 as i32) } else { (cam.width as i32, cam.height as i32) };
            let mut args = p.url_args(native, SupportedCodecs::default(), None);
            if sub {
                args.push_str("&decode=-1");
            }
            let path = urls::live_video(id, AudioRequest::None, &args, "");
            let mut player = Player::new();
            player.open(client, OpenParams { path, hold_placeholder: false, mode: PlayMode::Live, timestamp_scale: 1.0, audio: false, accel: MainView::hw_accel(svc.settings) }, None, ctx);
            self.tiles.push(Tile { id: id.clone(), player, tex: None, norm });
        }
        self.group = Some(group.id.clone());
    }

    pub fn close(&mut self, gpu: &Gpu) {
        for t in &mut self.tiles {
            t.player.clear();
            if let Some(mut tex) = t.tex.take() {
                gpu.release(&mut tex);
            }
        }
        self.tiles.clear();
        self.group = None;
    }

    /// Uploads new frames and reconnects ended tiles.
    pub fn update(&mut self, gpu: &Gpu) {
        for t in &mut self.tiles {
            if let Some(f) = t.player.take_new_frame() {
                let tex = t.tex.get_or_insert_with(|| gpu.new_texture());
                gpu.upload(tex, &f);
            }
        }
    }

    /// Draws the grid; returns the id of a clicked / double-clicked / right-clicked camera.
    pub fn show(&mut self, ui: &mut egui::Ui, view_cams: &[CameraInfo], show_stats: bool) -> (Option<String>, Option<String>, Option<String>) {
        let area = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(area, Sense::click());
        let painter = ui.painter_at(area);
        // Fit the group layout's aspect ratio inside the area.
        let layout = bi_render::fit_rect([(self.aspect * 1000.0) as u32, 1000], area);
        let mut hovered = None;
        for t in &self.tiles {
            let r = Rect::from_min_max(layout.min + t.norm.min.to_vec2() * layout.size(), layout.min + t.norm.max.to_vec2() * layout.size()).shrink(1.0);
            painter.rect_filled(r, 0.0, style::VIDEO_BG);
            if let Some(tex) = &t.tex
                && let Some(id) = tex.texture_id()
            {
                let fit = bi_render::fit_rect(tex.size(), r);
                painter.image(id, fit, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            } else {
                painter.text(r.center(), Align2::CENTER_CENTER, "…", FontId::proportional(18.0), style::t().text);
            }
            let name = view_cams.iter().find(|c| c.id.eq_ignore_ascii_case(&t.id)).map(|c| c.name.clone()).unwrap_or_else(|| t.id.clone());
            let mut label = name;
            if show_stats {
                let s = t.player.stats();
                label = format!("{label}  {} {}x{} {:.0}kbps {}", s.codec.map(|c| c.name()).unwrap_or("-"), s.width, s.height, s.video_kbps, s.decoder);
            }
            let g = painter.layout_no_wrap(label, FontId::proportional(12.0), Color32::WHITE);
            let lr = Rect::from_min_size(r.min, g.size() + vec2(8.0, 4.0));
            painter.rect_filled(lr, 2.0, Color32::from_black_alpha(150));
            painter.galley(lr.min + vec2(4.0, 2.0), g, Color32::WHITE);
            if let Some(st) = t.player.status()
                && (st.triggered || st.motion)
            {
                painter.rect_stroke(r, 0.0, egui::Stroke::new(2.0, if st.triggered { style::REC_RED } else { style::MOTION_ORANGE }), egui::StrokeKind::Inside);
            }
            if resp.hover_pos().is_some_and(|p| r.contains(p)) {
                hovered = Some(t.id.clone());
            }
        }
        let clicked = resp.clicked().then(|| hovered.clone()).flatten();
        let dbl = resp.double_clicked().then(|| hovered.clone()).flatten();
        let ctxm = resp.secondary_clicked().then(|| hovered.clone()).flatten();
        (clicked, dbl, ctxm)
    }
}
