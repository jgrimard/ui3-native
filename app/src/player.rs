//! Video player: one Blue Iris stream -> decode thread -> latest frame for the UI.
//!
//! Native replacement for `FetchH264VideoModule` / `JpegVideoModule` (ui3.js:19811-21460) and the browser
//! decoders. Each `Player` owns:
//! * a tokio stream task (`bi_proto::stream::spawn_stream_channel`) with TCP backpressure,
//! * a decode thread running FFmpeg (D3D11VA when possible) which also paces recorded playback,
//! * a shared slot the UI thread drains once per repaint and uploads to the GPU.

use bi_media::audio::{AudioOutput, decode_mulaw};
use bi_media::decoder::{HwAccel, Nv12Frame, VideoDecoder};
use bi_media::jpeg::{RgbaImage, decode_jpeg_rgba};
use bi_proto::BiClient;
use bi_proto::stream::{EndReason, StreamHandle, StreamHeaders, StreamMessage, StreamOptions, spawn_stream_channel};
use bi_proto::stream_parser::{BitmapInfoHeader, StatusBlock, StreamEvent, VideoCodec, WaveFormatEx};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub enum DisplayFrame {
    Nv12(Arc<Nv12Frame>),
    Rgba(Arc<RgbaImage>),
    /// Zero-copy: the frame stays in a shared D3D11 texture.
    Gpu(bi_media::d3d::GpuFrame),
}

/// D3D11 device shared with the renderer (same adapter) for zero-copy decoding; None = download frames.
static ZERO_COPY: parking_lot::RwLock<Option<Arc<bi_media::d3d::SharedD3D11>>> = parking_lot::RwLock::new(None);

pub fn set_zero_copy_device(d: Option<Arc<bi_media::d3d::SharedD3D11>>) {
    *ZERO_COPY.write() = d;
}

pub fn zero_copy_device() -> Option<Arc<bi_media::d3d::SharedD3D11>> {
    ZERO_COPY.read().clone()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayMode {
    /// Show frames as soon as they are decoded.
    Live,
    /// Present frames on a clock driven by their timestamps (recorded clips, timeline).
    Paced,
}

#[derive(Debug, Clone)]
pub struct OpenParams {
    /// Request path relative to the server root (see `bi_proto::urls`).
    pub path: String,
    pub mode: PlayMode,
    /// UI3 `timestampScale` (= speed/100 for recordings, 1 for live).
    pub timestamp_scale: f64,
    /// Whether this player's audio should go to the speakers.
    pub audio: bool,
    pub accel: HwAccel,
    /// Recorded timeline: Blue Iris opens the stream with a few "No video" placeholder frames before the recording.
    /// Decode them but keep showing the current picture (the seek preview) until a real one arrives.
    pub hold_placeholder: bool,
}

/// A real picture is far larger than Blue Iris's placeholder (~3.5 KB at 3072x1728; real frames 300 KB+).
const PLACEHOLDER_MAX_BYTES_PER_PIXEL: f64 = 1.0 / 200.0;
/// Stop holding after this long so a real "No video" gap still shows.
const PLACEHOLDER_HOLD_MAX: Duration = Duration::from_secs(5);

/// Metadata of the most recently presented frame.
#[derive(Debug, Clone, Default)]
pub struct FrameMeta {
    /// 0..=10000 position within a clip.
    pub pos: u16,
    pub utc_ms: u64,
    pub time_ms: i64,
    #[allow(dead_code)]
    pub keyframe: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub codec: Option<VideoCodec>,
    pub decoder: String,
    pub width: u32,
    pub height: u32,
    pub video_kbps: f64,
    pub audio_kbps: f64,
    pub fps: f64,
    pub frames_decoded: u64,
    pub frames_dropped: u64,
    pub decode_ms_avg: f64,
    pub audio_format: Option<String>,
    pub audio_buffer_ms: f64,
    /// How far behind the clock presentation is running (paced mode).
    pub late_ms: f64,
    pub opened_at: Option<Instant>,
    pub first_frame_ms: Option<f64>,
}

#[derive(Default)]
struct Shared {
    frame: Option<DisplayFrame>,
    new_frame: bool,
    meta: FrameMeta,
    headers: StreamHeaders,
    bitmap: Option<BitmapInfoHeader>,
    wave: Option<WaveFormatEx>,
    status: Option<StatusBlock>,
    ended: Option<EndReason>,
    stats: Stats,
    error: Option<String>,
}

/// Rolling 1-second rate meter.
struct Rate {
    samples: VecDeque<(Instant, f64)>,
}

impl Rate {
    fn new() -> Self {
        Rate { samples: VecDeque::new() }
    }
    fn add(&mut self, v: f64) {
        let now = Instant::now();
        self.samples.push_back((now, v));
        while self.samples.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(2)) {
            self.samples.pop_front();
        }
    }
    fn per_sec(&self) -> f64 {
        let (Some(a), Some(b)) = (self.samples.front(), self.samples.back()) else { return 0.0 };
        let span = b.0.duration_since(a.0).as_secs_f64();
        if span < 0.25 {
            return 0.0;
        }
        self.samples.iter().skip(1).map(|(_, v)| v).sum::<f64>() / span
    }
}

pub struct Player {
    shared: Arc<Mutex<Shared>>,
    stream: Option<StreamHandle>,
    stop: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    pub params: Option<OpenParams>,
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Player {
    pub fn new() -> Self {
        Player { shared: Arc::new(Mutex::new(Shared::default())), stream: None, stop: Arc::new(AtomicBool::new(false)), generation: Arc::new(AtomicU64::new(0)), params: None }
    }

    /// Stops any current stream and opens a new one. The last displayed frame stays visible until replaced.
    pub fn open(&mut self, client: &BiClient, params: OpenParams, audio_out: Option<AudioOutput>, ctx: &egui::Context) {
        self.close_stream();
        if let Some(a) = &audio_out
            && params.audio
        {
            a.clear();
        }
        let gen_id = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        {
            let mut s = self.shared.lock();
            s.ended = None;
            s.error = None;
            s.status = None;
            // Frame metadata belongs to the stream it came from.
            s.meta = FrameMeta::default();
            s.headers = StreamHeaders::default();
            s.stats = Stats { opened_at: Some(Instant::now()), ..Default::default() };
        }
        let (handle, rx) = spawn_stream_channel(
            &crate::runtime::handle(),
            client.clone(),
            params.path.clone(),
            StreamOptions { timestamp_scale: params.timestamp_scale, idle_timeout: Duration::from_secs(20) },
            // Small queue: live stays low-latency, paced playback gets backpressure.
            if params.mode == PlayMode::Live { 64 } else { 8 },
        );
        self.stream = Some(handle);
        let shared = self.shared.clone();
        let generation = self.generation.clone();
        let ctx = ctx.clone();
        let p = params.clone();
        std::thread::Builder::new()
            .name("decode".into())
            .spawn(move || decode_thread(rx, p, shared, stop, generation, gen_id, audio_out, ctx))
            .expect("spawn decode thread");
        self.params = Some(params);
    }

    fn close_stream(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut s) = self.stream.take() {
            s.stop();
        }
    }

    /// Stops streaming but keeps the last frame (pause).
    pub fn stop(&mut self) {
        self.close_stream();
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Stops and clears the picture.
    pub fn clear(&mut self) {
        self.stop();
        let mut s = self.shared.lock();
        *s = Shared::default();
        self.params = None;
    }

    pub fn is_streaming(&self) -> bool {
        self.stream.as_ref().is_some_and(|s| !s.is_finished()) && self.shared.lock().ended.is_none()
    }

    /// Returns a new frame if one arrived since the last call.
    pub fn take_new_frame(&self) -> Option<DisplayFrame> {
        let mut s = self.shared.lock();
        if s.new_frame {
            s.new_frame = false;
            s.frame.clone()
        } else {
            None
        }
    }

    /// Replaces the displayed picture (e.g. a scrub JPEG) without touching the stream.
    pub fn set_still(&self, img: Arc<RgbaImage>) {
        let mut s = self.shared.lock();
        s.frame = Some(DisplayFrame::Rgba(img));
        s.new_frame = true;
    }

    pub fn meta(&self) -> FrameMeta {
        self.shared.lock().meta.clone()
    }
    pub fn headers(&self) -> StreamHeaders {
        self.shared.lock().headers.clone()
    }
    pub fn status(&self) -> Option<StatusBlock> {
        self.shared.lock().status.clone()
    }
    pub fn ended(&self) -> Option<EndReason> {
        self.shared.lock().ended.clone()
    }
    pub fn take_ended(&self) -> Option<EndReason> {
        self.shared.lock().ended.take()
    }
    pub fn stats(&self) -> Stats {
        self.shared.lock().stats.clone()
    }
    pub fn error(&self) -> Option<String> {
        self.shared.lock().error.clone()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.close_stream();
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_thread(
    mut rx: tokio::sync::mpsc::Receiver<StreamMessage>,
    p: OpenParams,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    my_gen: u64,
    audio: Option<AudioOutput>,
    ctx: egui::Context,
) {
    let current = || generation.load(Ordering::SeqCst) == my_gen && !stop.load(Ordering::SeqCst);
    let mut decoder: Option<VideoDecoder> = None;
    // Whether the current decoder outputs GPU frames (shared device).
    let mut decoder_zero_copy = false;
    let mut accel = p.accel;
    let mut out = Vec::new();
    let mut got_keyframe = false;
    let mut wave: Option<WaveFormatEx> = None;
    let mut pcm = Vec::new();
    let mut flac = bi_media::flac::FlacDecoder::new();
    let (mut vrate, mut arate, mut frate) = (Rate::new(), Rate::new(), Rate::new());
    let mut decode_ms_avg = 0.0f64;
    // Paced presentation clock: wall time corresponding to pts `clock_pts`.
    let mut clock: Option<(Instant, i64)> = None;
    let mut last_stats = Instant::now();
    let opened = Instant::now();
    // Pixel count from the stream header, and whether placeholder frames are being held back.
    let mut pixels = 0f64;
    let mut holding = p.hold_placeholder;

    let publish = |shared: &Mutex<Shared>, f: DisplayFrame, meta: FrameMeta| {
        let mut s = shared.lock();
        s.frame = Some(f);
        s.new_frame = true;
        s.meta = meta;
    };

    while let Some(msg) = rx.blocking_recv() {
        if !current() {
            return;
        }
        match msg {
            StreamMessage::Headers(h) => shared.lock().headers = h,
            StreamMessage::Jpeg { data, utc } => match decode_jpeg_rgba(&data) {
                Ok(img) => {
                    let meta = FrameMeta { utc_ms: utc.unwrap_or(0).max(0) as u64, ..Default::default() };
                    let size = [img.width, img.height];
                    publish(&shared, DisplayFrame::Rgba(Arc::new(img)), meta);
                    let mut s = shared.lock();
                    s.stats.width = size[0];
                    s.stats.height = size[1];
                    s.stats.codec = Some(VideoCodec::Jpeg);
                    s.stats.decoder = "jpeg".into();
                    drop(s);
                    ctx.request_repaint();
                }
                Err(e) => shared.lock().error = Some(format!("{e:#}")),
            },
            StreamMessage::Event(ev) => match ev {
                StreamEvent::Header { video, audio: a, .. } => {
                    // FLAC: Blue Iris sends STREAMINFO after the WAVEFORMATEX; the audio blocks are bare frames.
                    if let Some(w) = a.as_ref().filter(|w| w.is_flac() && w.raw.len() >= 18 + 34)
                        && let Err(e) = flac.set_streaminfo(&w.raw[18..])
                    {
                        tracing::warn!("FLAC STREAMINFO: {e:#}");
                    }
                    wave = a.clone();
                    let mut s = shared.lock();
                    s.bitmap = video.clone();
                    s.stats.audio_format = a.as_ref().map(|w| {
                        format!("{} {} Hz", if w.is_mulaw() { "μ-law" } else if w.is_flac() { "FLAC" } else { "PCM?" }, w.samples_per_sec)
                    });
                    s.wave = a;
                    if let Some(v) = &video {
                        s.stats.codec = Some(v.codec());
                        pixels = (v.width.unsigned_abs() as f64) * (v.height.unsigned_abs() as f64);
                    }
                }
                StreamEvent::Video(f) => {
                    vrate.add(f.data.len() as f64);
                    if !got_keyframe {
                        if !f.keyframe {
                            shared.lock().stats.frames_dropped += 1;
                            continue;
                        }
                        got_keyframe = true;
                    }
                    if holding && (f.data.len() as f64 >= pixels * PLACEHOLDER_MAX_BYTES_PER_PIXEL || opened.elapsed() > PLACEHOLDER_HOLD_MAX) {
                        holding = false;
                    }
                    if decoder_zero_copy && zero_copy_device().is_none() {
                        // Zero-copy was turned off (or failed): restart decoding at the next keyframe, downloading frames.
                        decoder = None;
                        decoder_zero_copy = false;
                        if !f.keyframe {
                            got_keyframe = false;
                            continue;
                        }
                    }
                    if decoder.as_ref().is_none_or(|d| d.codec() != f.codec) {
                        let shared_dev = if accel != HwAccel::Software { zero_copy_device() } else { None };
                        decoder_zero_copy = shared_dev.is_some();
                        match VideoDecoder::new_with_device(f.codec, accel, None, shared_dev) {
                            Ok(d) => decoder = Some(d),
                            Err(e) => {
                                shared.lock().error = Some(format!("decoder: {e:#}"));
                                continue;
                            }
                        }
                    }
                    let d = decoder.as_mut().unwrap();
                    let t0 = Instant::now();
                    out.clear();
                    if let Err(e) = d.decode(&f.data, f.time_ms, &mut out) {
                        tracing::warn!("decode error: {e:#}");
                    }
                    // Persistent hardware failures: fall back to software (e.g. unsupported profile).
                    if d.consecutive_errors() > 20 && accel != HwAccel::Software {
                        tracing::warn!("too many decode errors with hardware decoding; switching to software");
                        accel = HwAccel::Software;
                        decoder = None;
                        got_keyframe = false;
                        continue;
                    }
                    let dt = t0.elapsed().as_secs_f64() * 1000.0;
                    decode_ms_avg = decode_ms_avg * 0.9 + dt * 0.1;
                    let desc = d.description();
                    if holding {
                        out.clear();
                        continue;
                    }
                    for frame in out.drain(..) {
                        let pts = frame.pts();
                        let (fw, fh) = frame.size();
                        if p.mode == PlayMode::Paced {
                            let now = Instant::now();
                            let (base_t, base_pts) = *clock.get_or_insert((now + Duration::from_millis(60), pts));
                            let due = base_t + Duration::from_millis((pts - base_pts).max(0) as u64);
                            if due > now {
                                // Sleep in small steps so seeks/stops are responsive.
                                let mut remaining = due - now;
                                while remaining > Duration::ZERO {
                                    if !current() {
                                        return;
                                    }
                                    let step = remaining.min(Duration::from_millis(20));
                                    std::thread::sleep(step);
                                    remaining = remaining.saturating_sub(step);
                                }
                            } else {
                                let late = (now - due).as_secs_f64() * 1000.0;
                                shared.lock().stats.late_ms = late;
                                if late > 750.0 {
                                    // Fell far behind (network stall): restart the clock instead of fast-forwarding.
                                    clock = Some((now, pts));
                                }
                            }
                        }
                        if !current() {
                            return;
                        }
                        frate.add(1.0);
                        let meta = FrameMeta { pos: f.pos, utc_ms: f.utc_ms, time_ms: f.time_ms, keyframe: f.keyframe };
                        {
                            let mut s = shared.lock();
                            s.stats.width = fw;
                            s.stats.height = fh;
                            s.stats.decoder = desc.clone();
                            s.stats.frames_decoded += 1;
                            if s.stats.first_frame_ms.is_none() {
                                s.stats.first_frame_ms = s.stats.opened_at.map(|t| t.elapsed().as_secs_f64() * 1000.0);
                            }
                        }
                        let df = match frame {
                            bi_media::DecodedFrame::Cpu(f) => DisplayFrame::Nv12(Arc::new(f)),
                            bi_media::DecodedFrame::Gpu(g) => DisplayFrame::Gpu(g),
                        };
                        publish(&shared, df, meta);
                        ctx.request_repaint();
                    }
                }
                StreamEvent::Audio(a) => {
                    arate.add(a.data.len() as f64);
                    if let (Some(out_dev), Some(w)) = (audio.as_ref(), wave.as_ref())
                        && p.audio
                    {
                        pcm.clear();
                        let rate = if w.is_flac() {
                            match flac.decode_block(&a.data, &mut pcm) {
                                Ok(r) => r,
                                Err(e) => {
                                    tracing::debug!("FLAC block: {e:#}");
                                    0
                                }
                            }
                        } else {
                            decode_mulaw(&a.data, &mut pcm);
                            w.samples_per_sec
                        };
                        let max_latency = if p.mode == PlayMode::Live { Some(600.0) } else { None };
                        out_dev.push_mono(&pcm, rate, max_latency);
                    }
                }
                StreamEvent::Status(st) => shared.lock().status = Some(st),
                StreamEvent::End => {}
            },
            StreamMessage::Ended(r) => {
                if let Some(d) = decoder.as_mut() {
                    out.clear();
                    let _ = d.flush(&mut out);
                    if let Some(frame) = out.pop() {
                        let meta = shared.lock().meta.clone();
                        let df = match frame {
                            bi_media::DecodedFrame::Cpu(f) => DisplayFrame::Nv12(Arc::new(f)),
                            bi_media::DecodedFrame::Gpu(g) => DisplayFrame::Gpu(g),
                        };
                        publish(&shared, df, meta);
                    }
                }
                shared.lock().ended = Some(r);
                ctx.request_repaint();
                return;
            }
        }
        if last_stats.elapsed() > Duration::from_millis(500) {
            last_stats = Instant::now();
            let mut s = shared.lock();
            s.stats.video_kbps = vrate.per_sec() * 8.0 / 1000.0;
            s.stats.audio_kbps = arate.per_sec() * 8.0 / 1000.0;
            s.stats.fps = frate.per_sec();
            s.stats.decode_ms_avg = decode_ms_avg;
            s.stats.audio_buffer_ms = audio.as_ref().map(|a| a.buffered_ms()).unwrap_or(0.0);
        }
    }
}
