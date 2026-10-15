//! Transient notifications, the equivalent of UI3's `toaster` (toastr).

use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

struct Toast {
    level: Level,
    text: String,
    until: Instant,
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn add(&mut self, level: Level, text: impl Into<String>, ms: u64) {
        let text = text.into();
        match level {
            Level::Error => tracing::error!("{text}"),
            Level::Warning => tracing::warn!("{text}"),
            _ => tracing::info!("{text}"),
        }
        // Collapse duplicates.
        if let Some(t) = self.items.iter_mut().find(|t| t.text == text) {
            t.until = Instant::now() + Duration::from_millis(ms);
            return;
        }
        self.items.push(Toast { level, text, until: Instant::now() + Duration::from_millis(ms) });
        if self.items.len() > 8 {
            self.items.remove(0);
        }
    }
    pub fn info(&mut self, t: impl Into<String>) {
        self.add(Level::Info, t, 4000);
    }
    pub fn success(&mut self, t: impl Into<String>) {
        self.add(Level::Success, t, 4000);
    }
    pub fn warn(&mut self, t: impl Into<String>) {
        self.add(Level::Warning, t, 8000);
    }
    pub fn error(&mut self, t: impl Into<String>) {
        self.add(Level::Error, t, 12000);
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.items.retain(|t| t.until > now);
        if self.items.is_empty() {
            return;
        }
        let mut close = None;
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(300.0);
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 6.0);
                for (i, t) in self.items.iter().enumerate() {
                    // toastr: 300px, 80% opacity, 15px padding with a 50px icon gutter, 3px radius, soft shadow.
                    let (bg, icon) = match t.level {
                        Level::Info => (egui::Color32::from_rgb(0x2F, 0x96, 0xB4), crate::icons::INFORMATION),
                        Level::Success => (egui::Color32::from_rgb(0x51, 0xA3, 0x51), crate::icons::OK),
                        Level::Warning => (egui::Color32::from_rgb(0xF8, 0x94, 0x06), crate::icons::WARNING),
                        Level::Error => (egui::Color32::from_rgb(0xBD, 0x36, 0x2F), crate::icons::ERROR),
                    };
                    let r = egui::Frame::new()
                        .fill(bg.gamma_multiply(0.85))
                        .corner_radius(3)
                        .shadow(egui::Shadow { offset: [0, 0], blur: 12, spread: 0, color: egui::Color32::from_black_alpha(160) })
                        .inner_margin(egui::Margin { left: 50, right: 15, top: 15, bottom: 15 })
                        .show(ui, |ui| {
                            ui.set_width(235.0);
                            ui.label(egui::RichText::new(&t.text).color(egui::Color32::WHITE).size(14.0));
                        });
                    let ir = egui::Rect::from_center_size(egui::pos2(r.response.rect.left() + 27.0, r.response.rect.center().y), egui::vec2(24.0, 24.0));
                    crate::views::w::paint_icon(ui, icon, ir, egui::Color32::WHITE);
                    if r.response.interact(egui::Sense::click()).clicked() {
                        close = Some(i);
                    }
                }
            });
        if let Some(i) = close {
            self.items.remove(i);
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}
