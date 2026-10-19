//! H.264 / H.265 video decoding with FFmpeg, hardware accelerated through D3D11VA when available.
//!
//! Replaces UI3's browser decoders (`HTML5_MSE_Player`, `WebCodec_Player`, `OpenH264_Player`, ui3.js:21460-23228).
//! Blue Iris sends one complete Annex-B access unit per video block, so packets go straight to the decoder
//! without a bitstream parser.
//!
//! Output is always NV12 in system memory (Phase A of the plan); hardware frames are downloaded with
//! `av_hwframe_transfer_data`, anything that is not NV12 (software yuv420p, 10-bit P010, yuvj420p...) is
//! converted with swscale.

use anyhow::{Result, anyhow, bail};
use bi_proto::VideoCodec;
use ffmpeg_sys_next as ff;
use std::ffi::CStr;
use std::ptr;

/// A decoded picture in NV12 layout with tightly packed rows.
#[derive(Clone)]
pub struct Nv12Frame {
    pub width: u32,
    pub height: u32,
    /// `width * height` bytes.
    pub y: Vec<u8>,
    /// `width * height/2` bytes (interleaved U,V at half resolution; width rounded up to even).
    pub uv: Vec<u8>,
    /// Caller-provided timestamp passed through the decoder (ms).
    pub pts: i64,
    /// True when the frame came from the hardware decoder.
    pub hw: bool,
    /// True if the source signalled full-range (JPEG) levels.
    pub full_range: bool,
}

impl Nv12Frame {
    pub fn uv_width(&self) -> u32 {
        self.width.div_ceil(2)
    }
    pub fn uv_height(&self) -> u32 {
        self.height.div_ceil(2)
    }
}

impl std::fmt::Debug for Nv12Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Nv12Frame({}x{} pts={} hw={})", self.width, self.height, self.pts, self.hw)
    }
}

/// Decoder output: either downloaded to system memory or kept on the GPU (zero-copy).
#[derive(Debug, Clone)]
pub enum DecodedFrame {
    Cpu(Nv12Frame),
    Gpu(crate::d3d::GpuFrame),
}

impl DecodedFrame {
    pub fn pts(&self) -> i64 {
        match self {
            DecodedFrame::Cpu(f) => f.pts,
            DecodedFrame::Gpu(g) => g.pts,
        }
    }
    pub fn size(&self) -> (u32, u32) {
        match self {
            DecodedFrame::Cpu(f) => (f.width, f.height),
            DecodedFrame::Gpu(g) => (g.slot.width, g.slot.height),
        }
    }
    pub fn is_hw(&self) -> bool {
        match self {
            DecodedFrame::Cpu(f) => f.hw,
            DecodedFrame::Gpu(_) => true,
        }
    }
    /// The CPU frame, if this is one (tests / software consumers).
    pub fn cpu(&self) -> Option<&Nv12Frame> {
        match self {
            DecodedFrame::Cpu(f) => Some(f),
            DecodedFrame::Gpu(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HwAccel {
    /// Try D3D11VA, fall back to software.
    #[default]
    Auto,
    D3D11Va,
    Software,
}

pub fn av_err(code: i32) -> String {
    let mut buf = [0i8; 256];
    unsafe {
        ff::av_strerror(code, buf.as_mut_ptr() as *mut _, buf.len());
        CStr::from_ptr(buf.as_ptr() as *const _).to_string_lossy().into_owned()
    }
}

unsafe extern "C" fn get_format_hw(ctx: *mut ff::AVCodecContext, fmts: *const ff::AVPixelFormat) -> ff::AVPixelFormat {
    unsafe {
        let mut p = fmts;
        let mut first_sw = ff::AVPixelFormat::AV_PIX_FMT_NONE;
        while *p != ff::AVPixelFormat::AV_PIX_FMT_NONE {
            if *p == ff::AVPixelFormat::AV_PIX_FMT_D3D11 && !(*ctx).hw_device_ctx.is_null() {
                return *p;
            }
            if first_sw == ff::AVPixelFormat::AV_PIX_FMT_NONE {
                let desc = ff::av_pix_fmt_desc_get(*p);
                if !desc.is_null() && ((*desc).flags & ff::AV_PIX_FMT_FLAG_HWACCEL as u64) == 0 {
                    first_sw = *p;
                }
            }
            p = p.add(1);
        }
        first_sw
    }
}

pub struct VideoDecoder {
    ctx: *mut ff::AVCodecContext,
    pkt: *mut ff::AVPacket,
    frame: *mut ff::AVFrame,
    sw_frame: *mut ff::AVFrame,
    sws: *mut ff::SwsContext,
    codec: VideoCodec,
    hw_requested: bool,
    decoder_name: String,
    /// Count of consecutive errors, used to fall back to software.
    errors: u32,
    accel: HwAccel,
    adapter: Option<u32>,
    last_output_hw: bool,
    /// Zero-copy output ring (when created with a shared D3D11 device).
    ring: Option<crate::d3d::SharedRing>,
    last_zero_copy: bool,
}

unsafe impl Send for VideoDecoder {}

impl VideoDecoder {
    /// `adapter`: DXGI adapter index for D3D11VA (None = default adapter).
    pub fn new(codec: VideoCodec, accel: HwAccel, adapter: Option<u32>) -> Result<Self> {
        Self::new_with_device(codec, accel, adapter, None)
    }

    /// `shared`: decode on this D3D11 device and output GPU frames (zero-copy) instead of downloading.
    pub fn new_with_device(codec: VideoCodec, accel: HwAccel, adapter: Option<u32>, shared: Option<std::sync::Arc<crate::d3d::SharedD3D11>>) -> Result<Self> {
        let id = match codec {
            VideoCodec::H264 => ff::AVCodecID::AV_CODEC_ID_H264,
            VideoCodec::H265 => ff::AVCodecID::AV_CODEC_ID_HEVC,
            VideoCodec::Jpeg => ff::AVCodecID::AV_CODEC_ID_MJPEG,
            VideoCodec::Unknown(cc) => bail!("unsupported codec {:?}", String::from_utf8_lossy(&cc)),
        };
        unsafe {
            let dec = ff::avcodec_find_decoder(id);
            if dec.is_null() {
                bail!("FFmpeg has no decoder for {codec:?}");
            }
            let ctx = ff::avcodec_alloc_context3(dec);
            if ctx.is_null() {
                bail!("avcodec_alloc_context3 failed");
            }
            let mut me = VideoDecoder {
                ctx,
                pkt: ff::av_packet_alloc(),
                frame: ff::av_frame_alloc(),
                sw_frame: ff::av_frame_alloc(),
                sws: ptr::null_mut(),
                codec,
                hw_requested: false,
                decoder_name: CStr::from_ptr((*dec).name).to_string_lossy().into_owned(),
                errors: 0,
                accel,
                adapter,
                last_output_hw: false,
                ring: None,
                last_zero_copy: false,
            };
            (*ctx).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as i32;
            (*ctx).flags2 |= ff::AV_CODEC_FLAG2_FAST;
            (*ctx).pkt_timebase = ff::AVRational { num: 1, den: 1000 };
            if let Some(sh) = shared.filter(|_| accel != HwAccel::Software && codec != VideoCodec::Jpeg) {
                (*ctx).hw_device_ctx = sh.hw_device_ref();
                (*ctx).get_format = Some(get_format_hw);
                (*ctx).extra_hw_frames = 4;
                me.hw_requested = true;
                me.ring = Some(crate::d3d::SharedRing::new(sh));
            } else if accel != HwAccel::Software && codec != VideoCodec::Jpeg {
                let mut hw_dev: *mut ff::AVBufferRef = ptr::null_mut();
                let dev_str = adapter.map(|a| std::ffi::CString::new(a.to_string()).unwrap());
                let r = ff::av_hwdevice_ctx_create(
                    &mut hw_dev,
                    ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
                    dev_str.as_ref().map(|s| s.as_ptr()).unwrap_or(ptr::null()),
                    ptr::null_mut(),
                    0,
                );
                if r >= 0 {
                    (*ctx).hw_device_ctx = ff::av_buffer_ref(hw_dev);
                    ff::av_buffer_unref(&mut hw_dev);
                    (*ctx).get_format = Some(get_format_hw);
                    // Leave headroom so we can hold a few decoder surfaces.
                    (*ctx).extra_hw_frames = 4;
                    me.hw_requested = true;
                } else if accel == HwAccel::D3D11Va {
                    bail!("D3D11VA device creation failed: {}", av_err(r));
                } else {
                    tracing::warn!("D3D11VA unavailable ({}), using software decoding", av_err(r));
                }
            }
            if !me.hw_requested {
                // Slice threading keeps latency at zero frames; frame threading would add a frame of delay per thread.
                (*ctx).thread_count = 0;
                (*ctx).thread_type = ff::FF_THREAD_SLICE;
            }
            let r = ff::avcodec_open2(ctx, dec, ptr::null_mut());
            if r < 0 {
                bail!("avcodec_open2 failed: {}", av_err(r));
            }
            Ok(me)
        }
    }

    pub fn codec(&self) -> VideoCodec {
        self.codec
    }

    /// e.g. "hevc (d3d11va)" for the stats overlay.
    pub fn description(&self) -> String {
        format!("{} ({})", self.decoder_name, if self.last_zero_copy { "d3d11va zero-copy" } else if self.last_output_hw { "d3d11va" } else { "software" })
    }

    pub fn is_hw(&self) -> bool {
        self.hw_requested
    }

    /// Feed one access unit and collect any frames that come out.
    pub fn decode(&mut self, data: &[u8], pts_ms: i64, out: &mut Vec<DecodedFrame>) -> Result<()> {
        unsafe {
            let r = ff::av_new_packet(self.pkt, data.len() as i32);
            if r < 0 {
                bail!("av_new_packet: {}", av_err(r));
            }
            ptr::copy_nonoverlapping(data.as_ptr(), (*self.pkt).data, data.len());
            (*self.pkt).pts = pts_ms;
            (*self.pkt).dts = pts_ms;
            let r = ff::avcodec_send_packet(self.ctx, self.pkt);
            ff::av_packet_unref(self.pkt);
            if r < 0 && r != ff::AVERROR(ff::EAGAIN) {
                self.errors += 1;
                if r == ff::AVERROR_INVALIDDATA {
                    // Corrupt or partial frame; keep going, the next keyframe will recover.
                    return Ok(());
                }
                bail!("avcodec_send_packet: {}", av_err(r));
            }
            self.receive_all(out)
        }
    }

    /// Drains remaining frames (end of stream).
    pub fn flush(&mut self, out: &mut Vec<DecodedFrame>) -> Result<()> {
        unsafe {
            ff::avcodec_send_packet(self.ctx, ptr::null());
            self.receive_all(out)?;
            ff::avcodec_flush_buffers(self.ctx);
        }
        Ok(())
    }

    /// Discards buffered frames (e.g. after a seek); the next frame must be a keyframe.
    pub fn reset(&mut self) {
        unsafe { ff::avcodec_flush_buffers(self.ctx) }
    }

    pub fn consecutive_errors(&self) -> u32 {
        self.errors
    }

    pub fn accel(&self) -> HwAccel {
        self.accel
    }

    pub fn adapter(&self) -> Option<u32> {
        self.adapter
    }

    unsafe fn receive_all(&mut self, out: &mut Vec<DecodedFrame>) -> Result<()> {
        unsafe {
            loop {
                let r = ff::avcodec_receive_frame(self.ctx, self.frame);
                if r == ff::AVERROR(ff::EAGAIN) || r == ff::AVERROR_EOF {
                    return Ok(());
                }
                if r < 0 {
                    self.errors += 1;
                    return Err(anyhow!("avcodec_receive_frame: {}", av_err(r)));
                }
                self.errors = 0;
                // Zero-copy: keep D3D11 frames on the GPU.
                if (*self.frame).format == ff::AVPixelFormat::AV_PIX_FMT_D3D11 as i32
                    && let Some(ring) = self.ring.as_mut()
                {
                    let r = ring.take(self.frame);
                    self.last_output_hw = true;
                    match r {
                        Ok(Some(g)) => {
                            self.last_zero_copy = true;
                            ff::av_frame_unref(self.frame);
                            out.push(DecodedFrame::Gpu(g));
                            continue;
                        }
                        Ok(None) => {
                            // Every slot is still displayed: drop this frame (live) rather than stall.
                            ff::av_frame_unref(self.frame);
                            continue;
                        }
                        Err(e) => {
                            tracing::warn!("zero-copy failed ({e:#}); downloading frames instead");
                            self.ring = None;
                        }
                    }
                }
                self.last_zero_copy = false;
                let res = self.convert_current();
                ff::av_frame_unref(self.frame);
                out.push(DecodedFrame::Cpu(res?));
            }
        }
    }

    unsafe fn convert_current(&mut self) -> Result<Nv12Frame> {
        unsafe {
            let mut src = self.frame;
            let pts = if (*src).best_effort_timestamp != ff::AV_NOPTS_VALUE { (*src).best_effort_timestamp } else { (*src).pts };
            let hw = (*src).format == ff::AVPixelFormat::AV_PIX_FMT_D3D11 as i32;
            self.last_output_hw = hw;
            if hw {
                ff::av_frame_unref(self.sw_frame);
                (*self.sw_frame).format = ff::AVPixelFormat::AV_PIX_FMT_NONE as i32;
                let r = ff::av_hwframe_transfer_data(self.sw_frame, src, 0);
                if r < 0 {
                    bail!("av_hwframe_transfer_data: {}", av_err(r));
                }
                src = self.sw_frame;
            }
            let w = (*src).width as u32;
            let h = (*src).height as u32;
            let full_range = (*src).color_range == ff::AVColorRange::AVCOL_RANGE_JPEG
                || (*src).format == ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P as i32
                || (*src).format == ff::AVPixelFormat::AV_PIX_FMT_YUVJ422P as i32
                || (*src).format == ff::AVPixelFormat::AV_PIX_FMT_YUVJ444P as i32;
            let uvw = w.div_ceil(2) as usize;
            let uvh = h.div_ceil(2) as usize;
            let mut y = vec![0u8; w as usize * h as usize];
            let mut uv = vec![0u8; uvw * 2 * uvh];
            if (*src).format == ff::AVPixelFormat::AV_PIX_FMT_NV12 as i32 {
                copy_plane((*src).data[0], (*src).linesize[0] as isize, &mut y, w as usize, h as usize);
                copy_plane((*src).data[1], (*src).linesize[1] as isize, &mut uv, uvw * 2, uvh);
            } else {
                // Convert anything else (yuv420p, yuvj420p, p010, ...) to NV12.
                self.sws = ff::sws_getCachedContext(
                    self.sws,
                    w as i32,
                    h as i32,
                    std::mem::transmute::<i32, ff::AVPixelFormat>((*src).format),
                    w as i32,
                    h as i32,
                    ff::AVPixelFormat::AV_PIX_FMT_NV12,
                    ff::SwsFlags::SWS_POINT as i32,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null(),
                );
                if self.sws.is_null() {
                    bail!("sws_getCachedContext failed for pixel format {}", (*src).format);
                }
                let dst_data = [y.as_mut_ptr(), uv.as_mut_ptr(), ptr::null_mut(), ptr::null_mut()];
                let dst_lines = [w as i32, (uvw * 2) as i32, 0, 0];
                ff::sws_scale(self.sws, (*src).data.as_ptr() as *const *const u8, (*src).linesize.as_ptr(), 0, h as i32, dst_data.as_ptr(), dst_lines.as_ptr());
            }
            Ok(Nv12Frame { width: w, height: h, y, uv, pts, hw, full_range })
        }
    }
}

unsafe fn copy_plane(src: *const u8, stride: isize, dst: &mut [u8], row_bytes: usize, rows: usize) {
    unsafe {
        for r in 0..rows {
            let s = src.offset(stride * r as isize);
            ptr::copy_nonoverlapping(s, dst.as_mut_ptr().add(r * row_bytes), row_bytes);
        }
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        unsafe {
            ff::avcodec_free_context(&mut self.ctx);
            ff::av_packet_free(&mut self.pkt);
            ff::av_frame_free(&mut self.frame);
            ff::av_frame_free(&mut self.sw_frame);
            if !self.sws.is_null() {
                ff::sws_freeContext(self.sws);
            }
        }
    }
}
