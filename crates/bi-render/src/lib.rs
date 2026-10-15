//! GPU video surfaces for egui.
//!
//! A [`VideoTexture`] owns NV12 plane textures plus an RGBA target. Decoded frames are uploaded and converted on
//! the GPU (BT.709/BT.601, limited/full range), and the RGBA target is registered with egui as a native texture so
//! it can be drawn with `egui::Image` / `Painter::image` — digital zoom is just a UV rect, overlays compose normally.
//! JPEG frames (scrubbing, JPEG profiles) upload straight into the RGBA target.

use bi_media::Nv12Frame;
use bi_media::jpeg::RgbaImage;
use egui_wgpu::wgpu;
use std::sync::Arc;

/// Shared pipeline objects, created once per device.
pub struct Nv12Pipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Zero-copy: decoder ring textures opened on this device, by slot id.
    shared: parking_lot::Mutex<std::collections::HashMap<u64, SharedTex>>,
}

/// A decoder ring texture (D3D11 NT handle) opened as a wgpu NV12 texture.
struct SharedTex {
    slot: std::sync::Weak<bi_media::d3d::SharedSlot>,
    _tex: wgpu::Texture,
    y: wgpu::TextureView,
    uv: wgpu::TextureView,
}

impl Nv12Pipeline {
    pub fn new(device: &wgpu::Device) -> Arc<Self> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12"),
            source: wgpu::ShaderSource::Wgsl(include_str!("nv12.wgsl").into()),
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("nv12"),
            entries: &[
                tex_entry(0),
                tex_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("nv12"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("nv12"),
            layout: Some(&pl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::Rgba8Unorm, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nv12"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Arc::new(Nv12Pipeline { pipeline, layout, sampler, shared: parking_lot::Mutex::new(Default::default()) })
    }
}

struct Planes {
    w: u32,
    h: u32,
    y: wgpu::Texture,
    uv: wgpu::Texture,
    params: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

/// One video surface (one per live tile / player).
pub struct VideoTexture {
    pipe: Arc<Nv12Pipeline>,
    planes: Option<Planes>,
    rgba: Option<(wgpu::Texture, wgpu::TextureView)>,
    tex_id: Option<egui::TextureId>,
    size: [u32; 2],
    /// Frames uploaded since creation.
    pub frames: u64,
    /// Zero-copy frames the GPU may still be reading (their ring slots must not be rewritten yet).
    in_flight: std::collections::VecDeque<bi_media::d3d::GpuFrame>,
    shared_params: Option<wgpu::Buffer>,
}

impl VideoTexture {
    pub fn new(pipe: Arc<Nv12Pipeline>) -> Self {
        VideoTexture { pipe, planes: None, rgba: None, tex_id: None, size: [0, 0], frames: 0, in_flight: Default::default(), shared_params: None }
    }

    pub fn texture_id(&self) -> Option<egui::TextureId> {
        self.tex_id
    }

    /// Pixel size of the current picture.
    pub fn size(&self) -> [u32; 2] {
        self.size
    }

    /// Copies the picture currently shown (the converted RGBA texture) back to the CPU. Blocks until the GPU is done.
    pub fn read_rgba(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<RgbaImage> {
        let (tex, _) = self.rgba.as_ref()?;
        let [w, h] = self.size;
        if w == 0 || h == 0 {
            return None;
        }
        let row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video-readback"),
            size: row as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        rx.recv().ok()?.ok()?;
        let data = buf.slice(..).get_mapped_range().ok()?;
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            rgba.extend_from_slice(&data[y * row as usize..y * row as usize + (w * 4) as usize]);
        }
        drop(data);
        buf.unmap();
        Some(RgbaImage { width: w, height: h, rgba })
    }

    pub fn has_image(&self) -> bool {
        self.tex_id.is_some()
    }

    fn ensure_rgba(&mut self, device: &wgpu::Device, renderer: &mut egui_wgpu::Renderer, w: u32, h: u32) {
        if self.size == [w, h] && self.rgba.is_some() {
            return;
        }
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("video-rgba"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        match self.tex_id {
            Some(id) => renderer.update_egui_texture_from_wgpu_texture(device, &view, wgpu::FilterMode::Linear, id),
            None => self.tex_id = Some(renderer.register_native_texture(device, &view, wgpu::FilterMode::Linear)),
        }
        self.rgba = Some((tex, view));
        self.size = [w, h];
    }

    fn ensure_planes(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        if self.planes.as_ref().is_some_and(|p| p.w == w && p.h == h) {
            return;
        }
        let mk = |label, w, h, format| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let y = mk("video-y", w, h, wgpu::TextureFormat::R8Unorm);
        let uv = mk("video-uv", w.div_ceil(2), h.div_ceil(2), wgpu::TextureFormat::Rg8Unorm);
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12-params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let yv = y.create_view(&Default::default());
        let uvv = uv.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nv12"),
            layout: &self.pipe.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&yv) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&uvv) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.pipe.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: params.as_entire_binding() },
            ],
        });
        self.planes = Some(Planes { w, h, y, uv, params, bind });
    }

    /// Uploads an NV12 frame and converts it to RGBA on the GPU.
    pub fn upload_nv12(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, renderer: &mut egui_wgpu::Renderer, f: &Nv12Frame) {
        if f.width == 0 || f.height == 0 {
            return;
        }
        self.ensure_planes(device, f.width, f.height);
        self.ensure_rgba(device, renderer, f.width, f.height);
        let p = self.planes.as_ref().unwrap();
        let write = |tex: &wgpu::Texture, data: &[u8], bpr: u32, w: u32, h: u32| {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        };
        write(&p.y, &f.y, f.width, f.width, f.height);
        write(&p.uv, &f.uv, f.uv_width() * 2, f.uv_width(), f.uv_height());
        // BT.709 for HD and up, BT.601 for SD (common camera/encoder convention when VUI is absent).
        let bt709 = if f.height >= 720 { 1.0f32 } else { 0.0 };
        let params: [f32; 4] = [if f.full_range { 1.0 } else { 0.0 }, bt709, 0.0, 0.0];
        queue.write_buffer(&p.params, 0, &bytes_of(&params));
        let bind = p.bind.clone();
        self.convert(device, queue, &bind);
    }

    /// Runs the NV12 -> RGBA pass into the egui texture.
    fn convert(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bind: &wgpu::BindGroup) {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("nv12-convert") });
        {
            let (_, view) = self.rgba.as_ref().unwrap();
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("nv12-convert"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipe.pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([enc.finish()]);
        self.frames += 1;
    }

    /// Zero-copy: samples a decoder ring texture (shared NV12 opened on wgpu's D3D12 device) and converts it to RGBA.
    #[cfg(windows)]
    pub fn upload_shared(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, renderer: &mut egui_wgpu::Renderer, g: &bi_media::d3d::GpuFrame) -> Result<(), String> {
        let (w, h) = (g.slot.width, g.slot.height);
        if w == 0 || h == 0 {
            return Ok(());
        }
        if g.slot.ten_bit {
            return Err("10-bit frames are not supported zero-copy".into());
        }
        let (y, uv) = {
            let mut cache = self.pipe.shared.lock();
            // Forget textures whose ring slot is gone (decoder closed or resized).
            cache.retain(|_, t| t.slot.strong_count() > 0);
            if !cache.contains_key(&g.slot.id) {
                let t = open_shared(device, g)?;
                cache.insert(g.slot.id, t);
            }
            let t = &cache[&g.slot.id];
            (t.y.clone(), t.uv.clone())
        };
        self.ensure_rgba(device, renderer, w, h);
        let params = self.shared_params.get_or_insert_with(|| {
            device.create_buffer(&wgpu::BufferDescriptor { label: Some("nv12-params"), size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
        });
        let bt709 = if h >= 720 { 1.0f32 } else { 0.0 };
        queue.write_buffer(params, 0, &bytes_of(&[if g.full_range { 1.0 } else { 0.0 }, bt709, 0.0, 0.0]));
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nv12-shared"),
            layout: &self.pipe.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&y) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&uv) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.pipe.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: params.as_entire_binding() },
            ],
        });
        self.convert(device, queue, &bind);
        // Keep the last few frames referenced so the decoder does not overwrite a slot the GPU may still read.
        self.in_flight.push_back(g.clone());
        while self.in_flight.len() > 3 {
            self.in_flight.pop_front();
        }
        Ok(())
    }


    /// Uploads an RGBA image (decoded JPEG) directly.
    pub fn upload_rgba(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, renderer: &mut egui_wgpu::Renderer, img: &RgbaImage) {
        if img.width == 0 || img.height == 0 {
            return;
        }
        self.ensure_rgba(device, renderer, img.width, img.height);
        let (tex, _) = self.rgba.as_ref().unwrap();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &img.rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(img.width * 4), rows_per_image: Some(img.height) },
            wgpu::Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
        );
        self.frames += 1;
    }

    /// Frees the egui texture registration. Call before dropping when the renderer is still alive.
    pub fn release(&mut self, renderer: &mut egui_wgpu::Renderer) {
        self.in_flight.clear();
        if let Some(id) = self.tex_id.take() {
            renderer.free_texture(&id);
        }
        self.rgba = None;
        self.planes = None;
        self.size = [0, 0];
    }
}

/// LUID of the adapter wgpu renders on (D3D12 only), so the decoder can use the same GPU.
#[cfg(windows)]
pub fn adapter_luid(device: &wgpu::Device) -> Option<windows::Win32::Foundation::LUID> {
    unsafe { device.as_hal::<wgpu::hal::api::Dx12>().map(|hal| hal.raw_device().GetAdapterLuid()) }
}

/// Whether this device can sample decoder textures directly (NV12 texture support).
pub fn supports_zero_copy(device: &wgpu::Device) -> bool {
    cfg!(windows) && device.features().contains(wgpu::Features::TEXTURE_FORMAT_NV12)
}

/// Opens a ring slot's NT handle on wgpu's D3D12 device and wraps it as an NV12 texture with plane views.
#[cfg(windows)]
fn open_shared(device: &wgpu::Device, g: &bi_media::d3d::GpuFrame) -> Result<SharedTex, String> {
    use windows::Win32::Graphics::Direct3D12::ID3D12Resource;
    let (w, h) = (g.slot.width, g.slot.height);
    let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
    let hal_tex = unsafe {
        let hal = device.as_hal::<wgpu::hal::api::Dx12>().ok_or("not a D3D12 device")?;
        let mut res: Option<ID3D12Resource> = None;
        hal.raw_device().OpenSharedHandle(g.slot.handle, &mut res).map_err(|e| format!("OpenSharedHandle: {e}"))?;
        let res = res.ok_or("OpenSharedHandle returned no resource")?;
        wgpu::hal::dx12::Device::texture_from_raw(res, wgpu::TextureFormat::NV12, wgpu::TextureDimension::D2, size, 1, 1)
    };
    let desc = wgpu::TextureDescriptor {
        label: Some("video-shared-nv12"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::NV12,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    };
    // D3D11-shared textures start in COMMON, which D3D12 implicitly promotes to the shader-resource state.
    let tex = unsafe { device.create_texture_from_hal::<wgpu::hal::api::Dx12>(hal_tex, &desc, wgpu::TextureUses::RESOURCE) };
    let y = tex.create_view(&wgpu::TextureViewDescriptor { label: Some("nv12-y"), format: Some(wgpu::TextureFormat::R8Unorm), aspect: wgpu::TextureAspect::Plane0, ..Default::default() });
    let uv = tex.create_view(&wgpu::TextureViewDescriptor { label: Some("nv12-uv"), format: Some(wgpu::TextureFormat::Rg8Unorm), aspect: wgpu::TextureAspect::Plane1, ..Default::default() });
    Ok(SharedTex { slot: Arc::downgrade(&g.slot), _tex: tex, y, uv })
}

fn bytes_of(v: &[f32; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, f) in v.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
    }
    out
}

/// Fits a `src` aspect into `dst`, centered (letterbox). Returns the rect to draw into.
pub fn fit_rect(src: [u32; 2], dst: egui::Rect) -> egui::Rect {
    if src[0] == 0 || src[1] == 0 {
        return dst;
    }
    let sa = src[0] as f32 / src[1] as f32;
    let da = dst.width() / dst.height();
    let size = if sa > da { egui::vec2(dst.width(), dst.width() / sa) } else { egui::vec2(dst.height() * sa, dst.height()) };
    egui::Rect::from_center_size(dst.center(), size)
}
