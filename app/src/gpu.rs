//! Access to eframe's wgpu device for uploading decoded video into egui textures.

use crate::player::DisplayFrame;
use bi_render::{Nv12Pipeline, VideoTexture};
use std::sync::Arc;

/// Zero-copy failed once in this session; never re-enable it.
static ZC_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Clone)]
pub struct Gpu {
    pub device: egui_wgpu::wgpu::Device,
    pub queue: egui_wgpu::wgpu::Queue,
    pub renderer: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    pub pipe: Arc<Nv12Pipeline>,
    pub adapter_name: String,
    pub backend: String,
    /// D3D11 device on the same adapter for zero-copy decoding (None if unsupported).
    pub shared_d3d11: Option<Arc<bi_media::d3d::SharedD3D11>>,
    zero_copy_on: Arc<std::sync::atomic::AtomicBool>,
}

impl Gpu {
    pub fn from_frame(frame: &eframe::Frame) -> Option<Self> {
        let rs = frame.wgpu_render_state()?;
        let info = rs.adapter.get_info();
        let shared_d3d11 = if bi_render::supports_zero_copy(&rs.device) {
            match bi_render::adapter_luid(&rs.device).map(bi_media::d3d::SharedD3D11::new) {
                Some(Ok(d)) => Some(d),
                Some(Err(e)) => {
                    tracing::warn!("zero-copy unavailable: {e:#}");
                    None
                }
                None => None,
            }
        } else {
            tracing::info!("zero-copy unavailable: the GPU device has no NV12 texture support");
            None
        };
        Some(Gpu {
            device: rs.device.clone(),
            queue: rs.queue.clone(),
            renderer: rs.renderer.clone(),
            pipe: Nv12Pipeline::new(&rs.device),
            adapter_name: info.name,
            backend: format!("{:?}", info.backend),
            shared_d3d11,
            zero_copy_on: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// Enables or disables zero-copy decoding for streams opened from now on (follows the Native setting).
    pub fn apply_zero_copy(&self, enabled: bool) {
        use std::sync::atomic::Ordering;
        let on = enabled && self.shared_d3d11.is_some();
        if self.zero_copy_on.swap(on, Ordering::Relaxed) != on || (on && crate::player::zero_copy_device().is_none() && !ZC_FAILED.load(Ordering::Relaxed)) {
            crate::player::set_zero_copy_device(if on && !ZC_FAILED.load(Ordering::Relaxed) { self.shared_d3d11.clone() } else { None });
        }
    }

    pub fn new_texture(&self) -> VideoTexture {
        VideoTexture::new(self.pipe.clone())
    }

    pub fn upload(&self, tex: &mut VideoTexture, f: &DisplayFrame) {
        let mut r = self.renderer.write();
        match f {
            DisplayFrame::Nv12(n) => tex.upload_nv12(&self.device, &self.queue, &mut r, n),
            DisplayFrame::Rgba(i) => tex.upload_rgba(&self.device, &self.queue, &mut r, i),
            DisplayFrame::Gpu(g) => {
                if let Err(e) = tex.upload_shared(&self.device, &self.queue, &mut r, g) {
                    // Any failure: stop zero-copy for good; decoders switch back to downloading frames.
                    if !ZC_FAILED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        tracing::warn!("zero-copy rendering failed ({e}); falling back to downloading frames");
                    }
                    crate::player::set_zero_copy_device(None);
                }
            }
        }
    }

    pub fn release(&self, tex: &mut VideoTexture) {
        tex.release(&mut self.renderer.write());
    }
}
