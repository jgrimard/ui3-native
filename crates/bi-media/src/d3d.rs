//! Zero-copy video path (Phase B).
//!
//! One D3D11 device is created on the *same adapter* as the renderer (matched by LUID) and handed to FFmpeg as the
//! D3D11VA hardware device for every decoder. Decoded surfaces (slices of FFmpeg's texture array) are copied
//! GPU-to-GPU into a small ring of shareable NV12/P010 textures (NT handles). A D3D11 fence makes the decode
//! thread wait until the copy has executed, then the handle is passed to the UI, which opens it on D3D12 through
//! wgpu-hal and samples it directly — no download to system memory and no re-upload.

use anyhow::{Context, Result, anyhow, bail};
use ffmpeg_sys_next as ff;
use std::sync::Arc;
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, LUID};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory4, IDXGIResource1};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::Interface;

/// D3D11 device shared by all decoders, plus the FFmpeg hw device context wrapping it.
pub struct SharedD3D11 {
    pub device: ID3D11Device,
    context: ID3D11DeviceContext4,
    fence: ID3D11Fence,
    fence_value: parking_lot::Mutex<u64>,
    hwdev: *mut ff::AVBufferRef,
    pub adapter_name: String,
}

unsafe impl Send for SharedD3D11 {}
unsafe impl Sync for SharedD3D11 {}

impl SharedD3D11 {
    /// Creates the device on the adapter with the given LUID (the renderer's adapter).
    pub fn new(luid: LUID) -> Result<Arc<Self>> {
        unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory1().context("CreateDXGIFactory1")?;
            let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(luid).context("EnumAdapterByLuid")?;
            let desc = adapter.GetDesc1()?;
            let adapter_name = String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_string();
            let mut device: Option<ID3D11Device> = None;
            let mut ctx: Option<ID3D11DeviceContext> = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )
            .context("D3D11CreateDevice")?;
            let device = device.ok_or_else(|| anyhow!("no device"))?;
            let ctx = ctx.ok_or_else(|| anyhow!("no context"))?;
            // Decoder threads and our copies share the immediate context.
            if let Ok(mt) = device.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }
            let device5: ID3D11Device5 = device.cast().context("ID3D11Device5 (Windows 10+) required")?;
            let mut fence: Option<ID3D11Fence> = None;
            device5.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut fence).context("CreateFence")?;
            let context: ID3D11DeviceContext4 = ctx.cast().context("ID3D11DeviceContext4")?;

            // Wrap for FFmpeg. FFmpeg releases `device` when the hw device context is freed, so give it its own ref.
            let hwdev = ff::av_hwdevice_ctx_alloc(ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA);
            if hwdev.is_null() {
                bail!("av_hwdevice_ctx_alloc failed");
            }
            let devctx = (*hwdev).data as *mut ff::AVHWDeviceContext;
            let d3d = (*devctx).hwctx as *mut ff::AVD3D11VADeviceContext;
            (*d3d).device = device.clone().into_raw() as *mut _;
            let r = ff::av_hwdevice_ctx_init(hwdev);
            if r < 0 {
                let mut h = hwdev;
                ff::av_buffer_unref(&mut h);
                bail!("av_hwdevice_ctx_init: {}", crate::decoder::av_err(r));
            }
            tracing::info!("zero-copy D3D11 device on {adapter_name}");
            Ok(Arc::new(SharedD3D11 { device, context, fence: fence.unwrap(), fence_value: parking_lot::Mutex::new(0), hwdev, adapter_name }))
        }
    }

    /// New reference to the FFmpeg hw device context (for `AVCodecContext.hw_device_ctx`).
    pub fn hw_device_ref(&self) -> *mut ff::AVBufferRef {
        unsafe { ff::av_buffer_ref(self.hwdev) }
    }

    /// Copies `src[slice]` into `dst` and blocks until the GPU has executed the copy.
    ///
    /// Several decoder threads share this device: the copy and the fence signal run under FFmpeg's D3D11 device
    /// lock (so they never interleave with FFmpeg's own use of the immediate context) and fence values are
    /// signaled in increasing order; only the wait happens outside the lock.
    fn copy_and_wait(&self, src: &ID3D11Texture2D, slice: u32, dst: &ID3D11Texture2D, w: u32, h: u32) -> Result<()> {
        unsafe {
            let bx = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
            let value = {
                let mut v = self.fence_value.lock();
                *v += 1;
                let value = *v;
                let _dev = self.lock_device();
                self.context.CopySubresourceRegion(dst, 0, 0, 0, 0, src, slice, Some(&bx));
                self.context.Signal(&self.fence, value).context("ID3D11DeviceContext4::Signal")?;
                self.context.Flush();
                value
            };
            if self.fence.GetCompletedValue() < value {
                let ev = CreateEventW(None, false, false, None)?;
                self.fence.SetEventOnCompletion(value, ev).context("SetEventOnCompletion")?;
                WaitForSingleObject(ev, 1000);
                let _ = CloseHandle(ev);
            }
            Ok(())
        }
    }

    /// Takes FFmpeg's lock for this D3D11 device (`AVD3D11VADeviceContext.lock`) until the guard drops.
    fn lock_device(&self) -> DeviceLock {
        unsafe {
            let devctx = (*self.hwdev).data as *mut ff::AVHWDeviceContext;
            let d3d = (*devctx).hwctx as *mut ff::AVD3D11VADeviceContext;
            if let Some(lock) = (*d3d).lock {
                lock((*d3d).lock_ctx);
            }
            DeviceLock { d3d }
        }
    }
}

struct DeviceLock {
    d3d: *mut ff::AVD3D11VADeviceContext,
}

impl Drop for DeviceLock {
    fn drop(&mut self) {
        unsafe {
            if let Some(unlock) = (*self.d3d).unlock {
                unlock((*self.d3d).lock_ctx);
            }
        }
    }
}

impl Drop for SharedD3D11 {
    fn drop(&mut self) {
        unsafe { ff::av_buffer_unref(&mut self.hwdev) }
    }
}

/// One shareable texture in a decoder's output ring.
pub struct SharedSlot {
    tex: ID3D11Texture2D,
    /// NT handle for opening on D3D12.
    pub handle: HANDLE,
    pub width: u32,
    pub height: u32,
    pub ten_bit: bool,
    /// Unique id (stable across frames) so the renderer can cache the opened D3D12 resource.
    pub id: u64,
}

unsafe impl Send for SharedSlot {}
unsafe impl Sync for SharedSlot {}

impl Drop for SharedSlot {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// A frame living on the GPU. Holding it keeps its ring slot from being overwritten.
#[derive(Clone)]
pub struct GpuFrame {
    pub slot: Arc<SharedSlot>,
    pub pts: i64,
    pub full_range: bool,
}

impl std::fmt::Debug for GpuFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GpuFrame({}x{} pts={})", self.slot.width, self.slot.height, self.pts)
    }
}

static NEXT_SLOT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
const RING: usize = 6;

/// Per-decoder ring of shareable textures.
pub struct SharedRing {
    d3d: Arc<SharedD3D11>,
    slots: Vec<Arc<SharedSlot>>,
    next: usize,
}

impl SharedRing {
    pub fn new(d3d: Arc<SharedD3D11>) -> Self {
        SharedRing { d3d, slots: Vec::new(), next: 0 }
    }

    fn make_slot(&self, w: u32, h: u32, format: DXGI_FORMAT) -> Result<SharedSlot> {
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED.0) as u32,
            };
            let mut tex: Option<ID3D11Texture2D> = None;
            self.d3d.device.CreateTexture2D(&desc, None, Some(&mut tex)).context("CreateTexture2D(shared)")?;
            let tex = tex.unwrap();
            let res: IDXGIResource1 = tex.cast()?;
            let handle = res.CreateSharedHandle(None, GENERIC_ALL.0, None).context("CreateSharedHandle")?;
            Ok(SharedSlot { tex, handle, width: w, height: h, ten_bit: format == DXGI_FORMAT_P010, id: NEXT_SLOT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) })
        }
    }

    /// Copies an FFmpeg D3D11 frame into the next free slot. Returns None if every slot is still in use.
    ///
    /// # Safety
    /// `frame` must be an `AV_PIX_FMT_D3D11` frame from a decoder using this ring's device.
    pub unsafe fn take(&mut self, frame: *const ff::AVFrame) -> Result<Option<GpuFrame>> {
        unsafe {
            let src_ptr = (*frame).data[0] as *mut std::ffi::c_void;
            let slice = (*frame).data[1] as usize as u32;
            let src = ID3D11Texture2D::from_raw_borrowed(&src_ptr).ok_or_else(|| anyhow!("null D3D11 texture"))?;
            let mut sd = D3D11_TEXTURE2D_DESC::default();
            src.GetDesc(&mut sd);
            let w = ((*frame).width as u32) & !1;
            let h = ((*frame).height as u32) & !1;
            if sd.Format == DXGI_FORMAT_P010 {
                // The renderer samples NV12 only; 10-bit video takes the download path.
                bail!("10-bit (P010) frames are not rendered zero-copy");
            }
            let fmt = DXGI_FORMAT_NV12;
            if self.slots.first().is_some_and(|s| s.width != w || s.height != h || s.ten_bit != (fmt == DXGI_FORMAT_P010)) {
                self.slots.clear();
            }
            if self.slots.is_empty() {
                for _ in 0..RING {
                    self.slots.push(Arc::new(self.make_slot(w, h, fmt)?));
                }
            }
            // Next slot nobody else (UI) is holding.
            let mut chosen = None;
            for k in 0..RING {
                let i = (self.next + k) % RING;
                if Arc::strong_count(&self.slots[i]) == 1 {
                    chosen = Some(i);
                    break;
                }
            }
            let Some(i) = chosen else { return Ok(None) };
            self.next = (i + 1) % RING;
            let slot = self.slots[i].clone();
            self.d3d.copy_and_wait(src, slice, &slot.tex, w, h)?;
            let full_range = (*frame).color_range == ff::AVColorRange::AVCOL_RANGE_JPEG;
            let pts = if (*frame).best_effort_timestamp != ff::AV_NOPTS_VALUE { (*frame).best_effort_timestamp } else { (*frame).pts };
            Ok(Some(GpuFrame { slot, pts, full_range }))
        }
    }
}
