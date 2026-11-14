//! Audio decoding and playback. Port of `MuLawDecoder` (ui3.js:33417) and `PcmAudioPlayer` (ui3.js:33537).
//!
//! Blue Iris delivers audio blocks inside the video stream: G.711 mu-law (`wFormatTag` 7, mono, usually 8 kHz)
//! or FLAC chunks (`audio=2`). Samples are resampled (linear) to the output device rate and fed through a
//! lock-free ring buffer to a WASAPI stream owned by a dedicated thread (cpal streams are not `Send`).

use anyhow::{Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// G.711 mu-law to linear PCM (ITU-T reference algorithm).
pub fn mulaw_to_linear(u: u8) -> i16 {
    let u = !u;
    let sign = u & 0x80;
    let exponent = (u >> 4) & 0x07;
    let mantissa = u & 0x0F;
    let mut sample = (((mantissa as i32) << 3) + 0x84) << exponent;
    sample -= 0x84;
    (if sign != 0 { -sample } else { sample }) as i16
}

static MULAW_TABLE: std::sync::LazyLock<[f32; 256]> = std::sync::LazyLock::new(|| {
    let mut t = [0f32; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = mulaw_to_linear(i as u8) as f32 / 32768.0;
    }
    t
});

pub fn decode_mulaw(data: &[u8], out: &mut Vec<f32>) {
    let t = &*MULAW_TABLE;
    out.extend(data.iter().map(|&b| t[b as usize]));
}

struct Shared {
    volume_bits: AtomicU32,
    muted: AtomicBool,
    /// Device output rate (Hz).
    rate: AtomicU32,
    channels: AtomicU32,
    /// Peak of the most recent callback, as f32 bits (for a level meter).
    peak_bits: AtomicU32,
    /// Samples (per channel) currently queued.
    queued: AtomicU64,
    clear: AtomicBool,
    fx_volume_bits: AtomicU32,
    /// Set by the stream's error callback (device unplugged or reconfigured); the audio thread rebuilds the stream.
    failed: AtomicBool,
}

/// Global audio output. Cheap to clone.
#[derive(Clone)]
pub struct AudioOutput {
    shared: Arc<Shared>,
    producer: Arc<Mutex<Option<rtrb::Producer<f32>>>>,
    /// Sound effects (event sounds, status tones) mixed over camera audio.
    fx: Arc<Mutex<Option<rtrb::Producer<f32>>>>,
    resampler: Arc<Mutex<Resampler>>,
}

#[derive(Default)]
struct Resampler {
    src_rate: u32,
    /// Fractional read position carried between blocks.
    frac: f64,
    last: f32,
}

const RING_SECONDS: usize = 4;

impl AudioOutput {
    /// Opens the default output device. Returns a handle even if no device exists (audio is then dropped).
    pub fn open() -> Self {
        let shared = Arc::new(Shared {
            volume_bits: AtomicU32::new(1.0f32.to_bits()),
            muted: AtomicBool::new(false),
            rate: AtomicU32::new(48000),
            channels: AtomicU32::new(2),
            peak_bits: AtomicU32::new(0),
            queued: AtomicU64::new(0),
            clear: AtomicBool::new(false),
            fx_volume_bits: AtomicU32::new(1.0f32.to_bits()),
            failed: AtomicBool::new(false),
        });
        let producer = Arc::new(Mutex::new(None));
        let fx = Arc::new(Mutex::new(None));
        let out = AudioOutput { shared: shared.clone(), producer: producer.clone(), fx: fx.clone(), resampler: Arc::new(Mutex::new(Resampler::default())) };
        let (tx, rx) = std::sync::mpsc::channel::<Result<()>>();
        std::thread::Builder::new()
            .name("audio-out".into())
            .spawn(move || {
                // The stream lives on this thread for the lifetime of the process. It is rebuilt after a stream
                // error (device unplugged or reconfigured), and looked for again every few seconds while there is
                // no output device at all.
                let mut stream = build_stream(shared.clone(), producer.clone(), fx.clone());
                let _ = tx.send(stream.as_ref().map(|_| ()).map_err(|e| anyhow!("{e:#}")));
                let mut last_try = std::time::Instant::now();
                loop {
                    std::thread::park_timeout(std::time::Duration::from_secs(1));
                    let failed = shared.failed.swap(false, Ordering::Relaxed);
                    if failed || (stream.is_err() && last_try.elapsed() > std::time::Duration::from_secs(5)) {
                        last_try = std::time::Instant::now();
                        // Close the old stream before opening the new one.
                        drop(stream);
                        shared.queued.store(0, Ordering::Relaxed);
                        stream = build_stream(shared.clone(), producer.clone(), fx.clone());
                        if failed && let Err(e) = &stream {
                            tracing::warn!("audio output lost: {e:#}");
                        }
                    }
                }
            })
            .ok();
        match rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("audio output unavailable: {e:#}"),
            Err(_) => tracing::warn!("audio thread failed to start"),
        }
        out
    }

    pub fn set_volume(&self, v: f32) {
        self.shared.volume_bits.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.shared.volume_bits.load(Ordering::Relaxed))
    }
    pub fn set_muted(&self, m: bool) {
        self.shared.muted.store(m, Ordering::Relaxed);
    }
    pub fn muted(&self) -> bool {
        self.shared.muted.load(Ordering::Relaxed)
    }
    pub fn device_rate(&self) -> u32 {
        self.shared.rate.load(Ordering::Relaxed)
    }
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.shared.peak_bits.load(Ordering::Relaxed))
    }
    /// Milliseconds of audio queued for playback (used for A/V sync and latency control).
    pub fn buffered_ms(&self) -> f64 {
        self.shared.queued.load(Ordering::Relaxed) as f64 * 1000.0 / self.device_rate().max(1) as f64
    }

    pub fn set_effects_volume(&self, v: f32) {
        self.shared.fx_volume_bits.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn channels(&self) -> u16 {
        self.shared.channels.load(Ordering::Relaxed) as u16
    }

    /// Queues an effect already in device format (interleaved, device rate and channel count).
    pub fn play_effect(&self, interleaved: &[f32]) {
        if let Some(p) = self.fx.lock().as_mut() {
            let n = interleaved.len().min(p.slots());
            if let Ok(chunk) = p.write_chunk_uninit(n) {
                chunk.fill_from_iter(interleaved.iter().copied().take(n));
            }
        }
    }

    /// Triangle-wave notes like UI3's `ProgrammaticSoundPlayer.PlayNotes`: (volume 0..1, Hz, seconds).
    pub fn play_notes(&self, notes: &[(f32, f32, f32)]) {
        let rate = self.device_rate() as f32;
        let ch = self.channels().max(1) as usize;
        let mut out = Vec::new();
        for &(vol, freq, dur) in notes {
            let n = (dur * rate) as usize;
            let edge = ((dur / 3.0).min(0.02) * rate) as usize;
            for i in 0..n {
                let phase = (i as f32 * freq / rate).fract();
                let tri = 4.0 * (phase - 0.5).abs() - 1.0;
                let env = if i < edge {
                    i as f32 / edge.max(1) as f32
                } else if i + edge > n {
                    (n - i) as f32 / edge.max(1) as f32
                } else {
                    1.0
                };
                let v = tri * vol * env;
                for _ in 0..ch {
                    out.push(v);
                }
            }
        }
        self.play_effect(&out);
    }

    /// Drops all queued audio (on stream change / seek / pause).
    pub fn clear(&self) {
        self.shared.clear.store(true, Ordering::Relaxed);
        *self.resampler.lock() = Resampler::default();
    }

    /// Queues mono samples at `src_rate`. If more than `max_latency_ms` is already queued (live streams),
    /// the backlog is dropped first to keep latency bounded.
    pub fn push_mono(&self, samples: &[f32], src_rate: u32, max_latency_ms: Option<f64>) {
        if samples.is_empty() || src_rate == 0 {
            return;
        }
        if let Some(max) = max_latency_ms
            && self.buffered_ms() > max
        {
            // The output callback empties the ring on its next run; anything queued before that would be thrown
            // away with the backlog, so this block is dropped too.
            self.clear();
            return;
        }
        let dst_rate = self.device_rate();
        let ch = self.shared.channels.load(Ordering::Relaxed).max(1) as usize;
        let mut rs = self.resampler.lock();
        if rs.src_rate != src_rate {
            *rs = Resampler { src_rate, frac: 0.0, last: samples[0] };
        }
        let step = src_rate as f64 / dst_rate as f64;
        let mut out = Vec::with_capacity(((samples.len() as f64 / step) as usize + 2) * ch);
        let n = samples.len() as isize;
        let last = rs.last;
        let get = |k: isize| if k < 0 { last } else { samples[k as usize] };
        // Virtual sample array: x[-1] = last sample of the previous block, x[0..n] = this block.
        let mut pos = rs.frac;
        loop {
            let i = pos.floor() as isize;
            if i + 1 > n - 1 {
                break;
            }
            let t = (pos - i as f64) as f32;
            let (a, b) = (get(i), get(i + 1));
            let v = a + (b - a) * t;
            for _ in 0..ch {
                out.push(v);
            }
            pos += step;
        }
        rs.frac = pos - n as f64;
        rs.last = *samples.last().unwrap();
        drop(rs);
        if let Some(p) = self.producer.lock().as_mut() {
            let n = out.len().min(p.slots());
            if let Ok(chunk) = p.write_chunk_uninit(n) {
                let written = chunk.fill_from_iter(out.into_iter().take(n));
                self.shared.queued.fetch_add((written / ch) as u64, Ordering::Relaxed);
            }
        }
    }
}

fn build_stream(shared: Arc<Shared>, producer_slot: Arc<Mutex<Option<rtrb::Producer<f32>>>>, fx_slot: Arc<Mutex<Option<rtrb::Producer<f32>>>>) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| anyhow!("no default output device"))?;
    let supported = device.default_output_config()?;
    let config = supported.config();
    let rate = config.sample_rate;
    let channels = config.channels as usize;
    shared.rate.store(rate, Ordering::Relaxed);
    shared.channels.store(channels as u32, Ordering::Relaxed);
    let (prod, mut cons) = rtrb::RingBuffer::<f32>::new(rate as usize * channels * RING_SECONDS);
    *producer_slot.lock() = Some(prod);
    let (fx_prod, mut fx_cons) = rtrb::RingBuffer::<f32>::new(rate as usize * channels * 8);
    *fx_slot.lock() = Some(fx_prod);
    let sh = shared.clone();
    let mut fill = move |out: &mut dyn FnMut(usize, f32), out_add: &mut dyn FnMut(usize, f32), len: usize| {
        if sh.clear.swap(false, Ordering::Relaxed) {
            let n = cons.slots();
            if let Ok(c) = cons.read_chunk(n) {
                c.commit_all();
            }
            sh.queued.store(0, Ordering::Relaxed);
        }
        let vol = if sh.muted.load(Ordering::Relaxed) { 0.0 } else { f32::from_bits(sh.volume_bits.load(Ordering::Relaxed)) };
        let avail = cons.slots().min(len);
        let mut peak = 0f32;
        if let Ok(chunk) = cons.read_chunk(avail) {
            let (a, b) = chunk.as_slices();
            for (i, s) in a.iter().chain(b.iter()).enumerate() {
                peak = peak.max(s.abs());
                out(i, s * vol);
            }
            chunk.commit_all();
        }
        for i in avail..len {
            out(i, 0.0);
        }
        // Mix effects on top.
        let fx_vol = f32::from_bits(sh.fx_volume_bits.load(Ordering::Relaxed));
        let fx_avail = fx_cons.slots().min(len);
        if fx_avail > 0
            && let Ok(chunk) = fx_cons.read_chunk(fx_avail)
        {
            let (a, b) = chunk.as_slices();
            for (i, s) in a.iter().chain(b.iter()).enumerate() {
                out_add(i, s * fx_vol);
            }
            chunk.commit_all();
        }
        sh.peak_bits.store(peak.to_bits(), Ordering::Relaxed);
        let frames = (avail / channels) as u64;
        let _ = sh.queued.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |q| Some(q.saturating_sub(frames)));
    };
    let on_error = |sh: Arc<Shared>| {
        move |e: cpal::Error| {
            tracing::error!("audio stream error: {e}");
            sh.failed.store(true, Ordering::Relaxed);
        }
    };
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_output_stream::<f32, _, _>(
            config,
            move |data: &mut [f32], _| {
                let len = data.len();
                let p = data.as_mut_ptr();
                // Two closures write the same buffer at disjoint times (set, then add).
                fill(&mut |i, v| unsafe { *p.add(i) = v }, &mut |i, v| unsafe { *p.add(i) += v }, len)
            },
            on_error(shared.clone()),
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_output_stream::<i16, _, _>(
            config,
            move |data: &mut [i16], _| {
                let len = data.len();
                let p = data.as_mut_ptr();
                fill(
                    &mut |i, v| unsafe { *p.add(i) = (v.clamp(-1.0, 1.0) * 32767.0) as i16 },
                    &mut |i, v| unsafe { *p.add(i) = (*p.add(i) as f32 + v * 32767.0).clamp(-32768.0, 32767.0) as i16 },
                    len,
                )
            },
            on_error(shared.clone()),
            None,
        )?,
        f => return Err(anyhow!("unsupported output sample format {f:?}")),
    };
    stream.play()?;
    tracing::info!("audio output: {} Hz, {} ch", rate, channels);
    Ok(stream)
}

/// Container formats accepted for sound effects. Sound files come from the Blue Iris server, so FFmpeg is not
/// allowed to pick playlist-style demuxers that open other files or URLs.
const SOUND_FORMATS: &std::ffi::CStr = c"wav,mp3,ogg,flac,aiff,aac";
/// Longest sound effect kept, in seconds (bounds memory for a mislabelled long recording).
const SOUND_MAX_SECONDS: usize = 60;

/// Decodes an audio file (wav/mp3/...) with FFmpeg into interleaved f32 at the given rate and channel count.
pub fn decode_sound_file(path: &std::path::Path, rate: u32, channels: u16) -> Result<Vec<f32>> {
    use ffmpeg_sys_next as ff;
    use std::ptr;

    /// Frees whatever was allocated, on every exit path.
    struct Guard {
        fmt: *mut ff::AVFormatContext,
        ctx: *mut ff::AVCodecContext,
        swr: *mut ff::SwrContext,
        pkt: *mut ff::AVPacket,
        frame: *mut ff::AVFrame,
        out_layout: ff::AVChannelLayout,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                ff::av_frame_free(&mut self.frame);
                ff::av_packet_free(&mut self.pkt);
                ff::swr_free(&mut self.swr);
                ff::av_channel_layout_uninit(&mut self.out_layout);
                ff::avcodec_free_context(&mut self.ctx);
                ff::avformat_close_input(&mut self.fmt);
            }
        }
    }

    if rate == 0 || channels == 0 {
        bail!("no audio output format");
    }
    unsafe {
        let cpath = std::ffi::CString::new(path.to_string_lossy().as_bytes())?;
        let mut g = Guard { fmt: ptr::null_mut(), ctx: ptr::null_mut(), swr: ptr::null_mut(), pkt: ptr::null_mut(), frame: ptr::null_mut(), out_layout: std::mem::zeroed() };
        let mut opts: *mut ff::AVDictionary = ptr::null_mut();
        ff::av_dict_set(&mut opts, c"format_whitelist".as_ptr(), SOUND_FORMATS.as_ptr(), 0);
        ff::av_dict_set(&mut opts, c"protocol_whitelist".as_ptr(), c"file".as_ptr(), 0);
        let r = ff::avformat_open_input(&mut g.fmt, cpath.as_ptr(), ptr::null(), &mut opts);
        ff::av_dict_free(&mut opts);
        if r < 0 {
            bail!("cannot open {} as a sound file: {}", path.display(), crate::decoder::av_err(r));
        }
        let r = ff::avformat_find_stream_info(g.fmt, ptr::null_mut());
        if r < 0 {
            bail!("avformat_find_stream_info: {}", crate::decoder::av_err(r));
        }
        let idx = ff::av_find_best_stream(g.fmt, ff::AVMediaType::AVMEDIA_TYPE_AUDIO, -1, -1, ptr::null_mut(), 0);
        if idx < 0 {
            bail!("no audio stream");
        }
        let st = *(*g.fmt).streams.add(idx as usize);
        let dec = ff::avcodec_find_decoder((*(*st).codecpar).codec_id);
        if dec.is_null() {
            bail!("no decoder for this sound file's codec");
        }
        g.ctx = ff::avcodec_alloc_context3(dec);
        if g.ctx.is_null() {
            bail!("avcodec_alloc_context3 failed");
        }
        let r = ff::avcodec_parameters_to_context(g.ctx, (*st).codecpar);
        if r < 0 {
            bail!("avcodec_parameters_to_context: {}", crate::decoder::av_err(r));
        }
        let r = ff::avcodec_open2(g.ctx, dec, ptr::null_mut());
        if r < 0 {
            bail!("avcodec_open2: {}", crate::decoder::av_err(r));
        }
        if (*g.ctx).ch_layout.nb_channels <= 0 || (*g.ctx).sample_rate <= 0 {
            bail!("sound file has no usable audio format");
        }
        ff::av_channel_layout_default(&mut g.out_layout, channels as i32);
        let r = ff::swr_alloc_set_opts2(&mut g.swr, &g.out_layout, ff::AVSampleFormat::AV_SAMPLE_FMT_FLT, rate as i32, &(*g.ctx).ch_layout, (*g.ctx).sample_fmt, (*g.ctx).sample_rate, 0, ptr::null_mut());
        if r < 0 || g.swr.is_null() {
            bail!("swr_alloc_set_opts2: {}", crate::decoder::av_err(r));
        }
        let r = ff::swr_init(g.swr);
        if r < 0 {
            bail!("swr_init: {}", crate::decoder::av_err(r));
        }
        g.pkt = ff::av_packet_alloc();
        g.frame = ff::av_frame_alloc();
        if g.pkt.is_null() || g.frame.is_null() {
            bail!("out of memory");
        }
        let (swr, frame, pkt) = (g.swr, g.frame, g.pkt);
        let limit = rate as usize * channels as usize * SOUND_MAX_SECONDS;
        let mut out: Vec<f32> = Vec::new();
        let drain = |out: &mut Vec<f32>| {
            let max = ff::swr_get_out_samples(swr, (*frame).nb_samples).max(0) as usize;
            let mut buf = vec![0f32; max * channels as usize];
            let dst = buf.as_mut_ptr() as *mut u8;
            let n = ff::swr_convert(swr, &dst, max as i32, (*frame).extended_data as *const *const u8, (*frame).nb_samples);
            if n > 0 {
                out.extend_from_slice(&buf[..n as usize * channels as usize]);
            }
        };
        while out.len() < limit && ff::av_read_frame(g.fmt, pkt) >= 0 {
            if (*pkt).stream_index == idx && ff::avcodec_send_packet(g.ctx, pkt) >= 0 {
                while ff::avcodec_receive_frame(g.ctx, frame) >= 0 {
                    drain(&mut out);
                }
            }
            ff::av_packet_unref(pkt);
        }
        ff::avcodec_send_packet(g.ctx, ptr::null());
        while out.len() < limit && ff::avcodec_receive_frame(g.ctx, frame) >= 0 {
            drain(&mut out);
        }
        out.truncate(limit);
        if out.is_empty() {
            bail!("sound file decoded to no audio");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mulaw_reference_values() {
        assert_eq!(mulaw_to_linear(0xFF), 0);
        assert_eq!(mulaw_to_linear(0x7F), 0);
        assert_eq!(mulaw_to_linear(0x00), -32124);
        assert_eq!(mulaw_to_linear(0x80), 32124);
    }

    #[test]
    fn sound_decoder_rejects_playlists_and_decodes_wav() {
        let dir = std::env::temp_dir().join(format!("ui3n-sound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A playlist that points at another file must not be followed.
        let playlist = dir.join("evil.wav");
        std::fs::write(&playlist, "#EXTM3U\n#EXTINF:1,\nfile:///C:/Windows/win.ini\n").unwrap();
        assert!(decode_sound_file(&playlist, 48000, 2).is_err());
        // Not audio at all.
        let junk = dir.join("junk.wav");
        std::fs::write(&junk, vec![0x5Au8; 4096]).unwrap();
        assert!(decode_sound_file(&junk, 48000, 2).is_err());
        // A real 8 kHz mono 16-bit WAV of 800 samples becomes about 0.1 s of 48 kHz stereo.
        let mut wav = Vec::new();
        let data: Vec<u8> = (0..800i32).flat_map(|i| (((i % 40) - 20) as i16 * 1000).to_le_bytes()).collect();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        let good = dir.join("tone.wav");
        std::fs::write(&good, wav).unwrap();
        let pcm = decode_sound_file(&good, 48000, 2).unwrap();
        assert!((8000..=10000).contains(&pcm.len()), "{}", pcm.len());
        assert!(pcm.iter().any(|v| v.abs() > 0.1));
    }
}
