//! Incremental parser for the Blue Iris `/video/`, `/file/clips/` and `/time/` binary stream container.
//!
//! Port of `FetchVideoH264Streamer` (ui3.js:36856), `StatusBlock`, `BITMAPINFOHEADER`, `WAVEFORMATEX`,
//! `BIVideoFrame` and `IdentifyVideoKeyframe` (ui3.js:37334-37489).
//!
//! Layout (note the mixed endianness, which matches what Blue Iris actually sends):
//! ```text
//! stream header:  "blue" u8 streams(1=video,2=audio+video) u8 hdrLen  [hdrLen bytes: BITMAPINFOHEADER (+ WAVEFORMATEX)]
//! block header:   "Blue" u8 type
//!   type 0 video: u16be pos, u32be time, u64le utc, u32be size, [size bytes Annex-B]
//!   type 1 audio: i32be size, [size bytes]
//!   type 2 status: u8 total (incl. the 6 header bytes), [total-6 bytes StatusBlock]
//!   type 4: end of stream
//! ```

use bytes::{Buf, Bytes, BytesMut};
use thiserror::Error;

const UINT32_RANGE: i64 = 4_294_967_296;
const MAX_VIDEO_FRAME: usize = 10_000_000;
const MAX_AUDIO_FRAME: usize = 2_000_000;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("stream did not start with \"blue\"")]
    BadStreamMagic,
    #[error("availableStreams ({0}) was supposed to be 1 (video) or 2 (audio+video)")]
    BadStreamCount(u8),
    #[error("block did not start with \"Blue\"")]
    BadBlockMagic,
    #[error("unknown block type {0}")]
    UnknownBlockType(u8),
    #[error("video frame size of {0} was rejected")]
    VideoFrameTooLarge(usize),
    #[error("audio frame size of {0} was rejected")]
    AudioFrameTooLarge(i64),
    #[error("status block size was invalid ({0})")]
    BadStatusSize(u8),
    #[error("video frame received before bitmap header")]
    FrameBeforeHeader,
}

/// Video codec as signalled by `BITMAPINFOHEADER.biCompression`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    H264,
    H265,
    Jpeg,
    Unknown([u8; 4]),
}

impl VideoCodec {
    pub fn from_fourcc(cc: [u8; 4]) -> Self {
        let up = cc.map(|b| b.to_ascii_uppercase());
        match &up {
            b"H264" => VideoCodec::H264,
            b"H265" | b"HEVC" => VideoCodec::H265,
            b"JPEG" | b"MJPG" => VideoCodec::Jpeg,
            _ => VideoCodec::Unknown(cc),
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264",
            VideoCodec::H265 => "H.265",
            VideoCodec::Jpeg => "JPEG",
            VideoCodec::Unknown(_) => "Unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitmapInfoHeader {
    pub size: u32,
    pub width: i32,
    pub height: i32,
    pub planes: u16,
    pub bit_count: u16,
    pub compression: [u8; 4],
    pub size_image: u32,
    pub x_pels_per_meter: i32,
    pub y_pels_per_meter: i32,
    pub clr_used: u32,
    pub clr_important: u32,
}

impl BitmapInfoHeader {
    pub fn codec(&self) -> VideoCodec {
        VideoCodec::from_fourcc(self.compression)
    }
    fn parse(b: &[u8]) -> Self {
        // Tolerate a short header by zero-padding, like the JS DataView reads would mostly do.
        let mut buf = [0u8; 40];
        let n = b.len().min(40);
        buf[..n].copy_from_slice(&b[..n]);
        let mut c = &buf[..];
        BitmapInfoHeader {
            size: c.get_u32_le(),
            width: c.get_i32_le(),
            height: c.get_i32_le(),
            planes: c.get_u16_le(),
            bit_count: c.get_u16_le(),
            compression: {
                let mut cc = [0u8; 4];
                c.copy_to_slice(&mut cc);
                cc
            },
            size_image: c.get_u32_le(),
            x_pels_per_meter: c.get_i32_le(),
            y_pels_per_meter: c.get_i32_le(),
            clr_used: c.get_u32_le(),
            clr_important: c.get_u32_le(),
        }
    }
}

/// Audio format tags seen from Blue Iris.
pub const WAVE_FORMAT_MULAW: u16 = 7;
pub const WAVE_FORMAT_FLAC: u16 = 0xF1AC;

/// Loosely based on WAVEFORMATEX (ui3.js:37371).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveFormatEx {
    pub format_tag: u16,
    pub channels: u16,
    pub samples_per_sec: u32,
    pub avg_bytes_per_sec: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
    pub cb_size: u16,
    pub raw: Bytes,
}

impl WaveFormatEx {
    fn parse(raw: Bytes) -> Option<Self> {
        if raw.len() < 14 {
            return None;
        }
        let mut c = &raw[..];
        let format_tag = c.get_u16_le();
        let channels = c.get_u16_le();
        let samples_per_sec = c.get_u32_le();
        let avg_bytes_per_sec = c.get_u32_le();
        let block_align = c.get_u16_le();
        let (bits_per_sample, cb_size) = if raw.len() >= 18 { (c.get_u16_le(), c.get_u16_le()) } else { (0, 0) };
        Some(WaveFormatEx { format_tag, channels, samples_per_sec, avg_bytes_per_sec, block_align, bits_per_sample, cb_size, raw })
    }
    pub fn is_mulaw(&self) -> bool {
        self.format_tag == WAVE_FORMAT_MULAW
    }
    pub fn is_flac(&self) -> bool {
        self.format_tag == WAVE_FORMAT_FLAC
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusBlock {
    pub rec: bool,
    pub motion: bool,
    pub check_fps: bool,
    pub triggered: bool,
    pub signal_lost: bool,
    pub push_error: bool,
    pub flash_error: bool,
    pub force_movie: bool,
    pub other0: u8,
    pub other1: u8,
    /// Frames per second in hundredths.
    pub fps_x100: i32,
    /// Audio peak out of 32767.
    pub apeak: i32,
    pub tpause: i32,
}

impl StatusBlock {
    fn parse(b: &[u8]) -> Self {
        let mut buf = [0u8; 22];
        let n = b.len().min(22);
        buf[..n].copy_from_slice(&b[..n]);
        let mut c = &buf[10..];
        StatusBlock {
            rec: buf[0] != 0,
            motion: buf[1] != 0,
            check_fps: buf[2] != 0,
            triggered: buf[3] != 0,
            signal_lost: buf[4] != 0,
            push_error: buf[5] != 0,
            flash_error: buf[6] != 0,
            force_movie: buf[7] != 0,
            other0: buf[8],
            other1: buf[9],
            fps_x100: c.get_i32(),
            apeak: c.get_i32(),
            tpause: c.get_i32(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Position within the clip, 0..=10000.
    pub pos: u16,
    /// Raw u32 time field as received.
    pub raw_time: u32,
    /// Playback-clock time in ms since the first frame, unwrapped and divided by the timestamp scale.
    pub time_ms: i64,
    /// Milliseconds since the unix epoch (UTC).
    pub utc_ms: u64,
    pub codec: VideoCodec,
    pub keyframe: bool,
    /// Annex-B NAL units (or a JPEG for JPEG streams).
    pub data: Bytes,
}

#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub data: Bytes,
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Header { streams: u8, video: Option<BitmapInfoHeader>, audio: Option<WaveFormatEx> },
    Video(VideoFrame),
    Audio(AudioFrame),
    Status(StatusBlock),
    /// Block type 4: natural end of stream.
    End,
}

#[derive(Debug, Clone, Copy)]
enum State {
    StreamHeaderStart,
    StreamHeaderRest { len: usize },
    BlockHeaderStart,
    BlockHeaderRest { kind: u8 },
    VideoData { pos: u16, raw_time: u32, utc: u64, size: usize },
    AudioData { size: usize },
    StatusData { size: usize },
    Ended,
}

/// Incremental parser. Feed bytes with [`StreamParser::push`], then drain events with [`StreamParser::next_event`].
pub struct StreamParser {
    buf: BytesMut,
    state: State,
    bitmap: Option<BitmapInfoHeader>,
    pending_streams: u8,
    timestamp_scale: f64,
    base_time: i64,
    last_time: i64,
    have_base: bool,
}

impl Default for StreamParser {
    fn default() -> Self {
        Self::new(1.0)
    }
}

impl StreamParser {
    /// `timestamp_scale` is `speed/100` for recorded playback (UI3 `timestampScale`); use 1.0 for live.
    pub fn new(timestamp_scale: f64) -> Self {
        StreamParser {
            buf: BytesMut::with_capacity(256 * 1024),
            state: State::StreamHeaderStart,
            bitmap: None,
            pending_streams: 0,
            timestamp_scale,
            base_time: 0,
            last_time: -1,
            have_base: false,
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    pub fn bitmap_header(&self) -> Option<&BitmapInfoHeader> {
        self.bitmap.as_ref()
    }

    pub fn is_ended(&self) -> bool {
        matches!(self.state, State::Ended)
    }

    /// Returns the next complete event, `Ok(None)` if more data is required.
    pub fn next_event(&mut self) -> Result<Option<StreamEvent>, ProtocolError> {
        loop {
            match self.state {
                State::Ended => return Ok(None),
                State::StreamHeaderStart => {
                    if self.buf.len() < 6 {
                        return Ok(None);
                    }
                    if &self.buf[..4] != b"blue" {
                        return Err(ProtocolError::BadStreamMagic);
                    }
                    let streams = self.buf[4];
                    if streams != 1 && streams != 2 {
                        return Err(ProtocolError::BadStreamCount(streams));
                    }
                    let len = self.buf[5] as usize;
                    self.buf.advance(6);
                    self.state = State::StreamHeaderRest { len };
                    self.pending_streams = streams;
                }
                State::StreamHeaderRest { len } => {
                    if self.buf.len() < len {
                        return Ok(None);
                    }
                    let hdr = self.buf.split_to(len).freeze();
                    let mut off = 0usize;
                    let mut video = None;
                    if hdr.len() >= 4 {
                        let bi_size = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
                        if bi_size > 0 {
                            let end = (bi_size).min(hdr.len());
                            video = Some(BitmapInfoHeader::parse(&hdr[..end]));
                            off = end;
                        }
                    }
                    let audio = if off < hdr.len() { WaveFormatEx::parse(hdr.slice(off..)) } else { None };
                    self.bitmap = video.clone();
                    self.state = State::BlockHeaderStart;
                    return Ok(Some(StreamEvent::Header { streams: self.pending_streams, video, audio }));
                }
                State::BlockHeaderStart => {
                    if self.buf.len() < 5 {
                        return Ok(None);
                    }
                    if &self.buf[..4] != b"Blue" {
                        return Err(ProtocolError::BadBlockMagic);
                    }
                    let kind = self.buf[4];
                    self.buf.advance(5);
                    self.state = State::BlockHeaderRest { kind };
                }
                State::BlockHeaderRest { kind } => match kind {
                    0 => {
                        if self.buf.len() < 18 {
                            return Ok(None);
                        }
                        let pos = self.buf.get_u16();
                        let raw_time = self.buf.get_u32();
                        let utc = self.buf.get_u64_le();
                        let size = self.buf.get_u32() as usize;
                        if size > MAX_VIDEO_FRAME {
                            return Err(ProtocolError::VideoFrameTooLarge(size));
                        }
                        self.state = State::VideoData { pos, raw_time, utc, size };
                    }
                    1 => {
                        if self.buf.len() < 4 {
                            return Ok(None);
                        }
                        let size = self.buf.get_i32() as i64;
                        if !(0..=MAX_AUDIO_FRAME as i64).contains(&size) {
                            return Err(ProtocolError::AudioFrameTooLarge(size));
                        }
                        self.state = State::AudioData { size: size as usize };
                    }
                    2 => {
                        if self.buf.is_empty() {
                            return Ok(None);
                        }
                        let total = self.buf.get_u8();
                        if total < 6 {
                            return Err(ProtocolError::BadStatusSize(total));
                        }
                        self.state = State::StatusData { size: total as usize - 6 };
                    }
                    4 => {
                        self.state = State::Ended;
                        return Ok(Some(StreamEvent::End));
                    }
                    k => return Err(ProtocolError::UnknownBlockType(k)),
                },
                State::VideoData { pos, raw_time, utc, size } => {
                    if self.buf.len() < size {
                        return Ok(None);
                    }
                    let data = self.buf.split_to(size).freeze();
                    let codec = self.bitmap.as_ref().ok_or(ProtocolError::FrameBeforeHeader)?.codec();
                    let time_ms = self.rebase_time(raw_time);
                    let keyframe = match codec {
                        VideoCodec::H264 => is_keyframe(&data, false),
                        VideoCodec::H265 => is_keyframe(&data, true),
                        _ => true,
                    };
                    self.state = State::BlockHeaderStart;
                    return Ok(Some(StreamEvent::Video(VideoFrame { pos, raw_time, time_ms, utc_ms: utc, codec, keyframe, data })));
                }
                State::AudioData { size } => {
                    if self.buf.len() < size {
                        return Ok(None);
                    }
                    let data = self.buf.split_to(size).freeze();
                    self.state = State::BlockHeaderStart;
                    return Ok(Some(StreamEvent::Audio(AudioFrame { data })));
                }
                State::StatusData { size } => {
                    if self.buf.len() < size {
                        return Ok(None);
                    }
                    let data = self.buf.split_to(size);
                    self.state = State::BlockHeaderStart;
                    return Ok(Some(StreamEvent::Status(StatusBlock::parse(&data))));
                }
            }
        }
    }

    /// Frame times no longer always start from 0 and the u32 can wrap (ui3.js:37193-37214).
    fn rebase_time(&mut self, raw: u32) -> i64 {
        let t = raw as i64;
        if !self.have_base {
            self.base_time = t;
            self.have_base = true;
        }
        if self.last_time != -1 {
            if self.last_time - 1_000_000_000 > t {
                self.base_time -= UINT32_RANGE;
            }
            if self.last_time + 1_000_000_000 < t {
                self.base_time += UINT32_RANGE;
            }
        }
        self.last_time = t;
        let mut time = t - self.base_time;
        if self.timestamp_scale != 0.0 {
            time = (time as f64 / self.timestamp_scale).round() as i64;
        }
        time.abs()
    }
}

/// Iterates the NAL unit type of every Annex-B start code in `data`.
pub fn nal_types(data: &[u8], h265: bool) -> impl Iterator<Item = u8> + '_ {
    let mut zeros = 0usize;
    let mut i = 0usize;
    std::iter::from_fn(move || {
        while i < data.len() {
            let b = data[i];
            i += 1;
            if b == 0 {
                zeros += 1;
                continue;
            }
            let start = (zeros == 2 || zeros == 3) && b == 1 && i < data.len();
            zeros = 0;
            if start {
                let h = data[i];
                return Some(if h265 { (h >> 1) & 0x3F } else { h & 0x1F });
            }
        }
        None
    })
}

/// Port of `IdentifyVideoKeyframe` (ui3.js:37431): H.264 IDR (5), H.265 IDR/CRA (19..=21).
pub fn is_keyframe(data: &[u8], h265: bool) -> bool {
    nal_types(data, h265).any(|t| if h265 { (19..=21).contains(&t) } else { t == 5 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream_header(codec: &[u8; 4], audio: Option<(u16, u32)>) -> Vec<u8> {
        let mut bih = Vec::new();
        bih.extend_from_slice(&40u32.to_le_bytes());
        bih.extend_from_slice(&1920i32.to_le_bytes());
        bih.extend_from_slice(&1080i32.to_le_bytes());
        bih.extend_from_slice(&1u16.to_le_bytes());
        bih.extend_from_slice(&24u16.to_le_bytes());
        bih.extend_from_slice(codec);
        bih.extend_from_slice(&[0u8; 20]);
        let mut wfx = Vec::new();
        if let Some((tag, rate)) = audio {
            wfx.extend_from_slice(&tag.to_le_bytes());
            wfx.extend_from_slice(&1u16.to_le_bytes());
            wfx.extend_from_slice(&rate.to_le_bytes());
            wfx.extend_from_slice(&rate.to_le_bytes());
            wfx.extend_from_slice(&1u16.to_le_bytes());
            wfx.extend_from_slice(&8u16.to_le_bytes());
            wfx.extend_from_slice(&0u16.to_le_bytes());
        }
        let mut out = b"blue".to_vec();
        out.push(if audio.is_some() { 2 } else { 1 });
        out.push((bih.len() + wfx.len()) as u8);
        out.extend(bih);
        out.extend(wfx);
        out
    }

    fn video_block(time: u32, utc: u64, data: &[u8]) -> Vec<u8> {
        let mut out = b"Blue".to_vec();
        out.push(0);
        out.extend_from_slice(&1234u16.to_be_bytes());
        out.extend_from_slice(&time.to_be_bytes());
        out.extend_from_slice(&utc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn parses_h265_stream_with_audio_status_and_end_byte_by_byte() {
        let mut s = stream_header(b"H265", Some((WAVE_FORMAT_MULAW, 8000)));
        // VPS(32) SPS(33) PPS(34) IDR_W_RADL(19)
        let key = [0, 0, 0, 1, 0x40, 1, 0, 0, 0, 1, 0x42, 1, 0, 0, 1, 0x44, 1, 0, 0, 1, 0x26, 1, 0xAA];
        s.extend(video_block(5000, 1_700_000_000_000, &key));
        s.extend(video_block(5040, 1_700_000_000_040, &[0, 0, 1, 0x02, 1, 0xBB])); // TRAIL_R(1)
        s.extend(b"Blue\x01");
        s.extend(3i32.to_be_bytes());
        s.extend([1, 2, 3]);
        s.extend(b"Blue\x02");
        s.push(6 + 22);
        let mut st = vec![1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        st.extend(1500i32.to_be_bytes());
        st.extend(100i32.to_be_bytes());
        st.extend(0i32.to_be_bytes());
        s.extend(st);
        s.extend(b"Blue\x04");

        let mut p = StreamParser::new(1.0);
        let mut events = Vec::new();
        for b in s {
            p.push(&[b]);
            while let Some(e) = p.next_event().unwrap() {
                events.push(e);
            }
        }
        assert_eq!(events.len(), 6);
        match &events[0] {
            StreamEvent::Header { streams, video, audio } => {
                assert_eq!(*streams, 2);
                let v = video.as_ref().unwrap();
                assert_eq!(v.codec(), VideoCodec::H265);
                assert_eq!((v.width, v.height), (1920, 1080));
                let a = audio.as_ref().unwrap();
                assert!(a.is_mulaw());
                assert_eq!(a.samples_per_sec, 8000);
            }
            e => panic!("{e:?}"),
        }
        match &events[1] {
            StreamEvent::Video(f) => {
                assert!(f.keyframe);
                assert_eq!(f.time_ms, 0);
                assert_eq!(f.pos, 1234);
                assert_eq!(f.utc_ms, 1_700_000_000_000);
                assert_eq!(f.codec, VideoCodec::H265);
            }
            e => panic!("{e:?}"),
        }
        match &events[2] {
            StreamEvent::Video(f) => {
                assert!(!f.keyframe);
                assert_eq!(f.time_ms, 40);
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(&events[3], StreamEvent::Audio(a) if a.data.as_ref() == [1, 2, 3]));
        match &events[4] {
            StreamEvent::Status(s) => {
                assert!(s.rec && s.triggered && !s.motion);
                assert_eq!(s.fps_x100, 1500);
                assert_eq!(s.apeak, 100);
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(events[5], StreamEvent::End));
        assert!(p.is_ended());
    }

    #[test]
    fn time_wraps_and_scales() {
        let mut p = StreamParser::new(2.0);
        let mut s = stream_header(b"H264", None);
        s.extend(video_block(u32::MAX - 99, 0, &[0, 0, 1, 0x65]));
        s.extend(video_block(100, 0, &[0, 0, 1, 0x41]));
        p.push(&s);
        let mut times = Vec::new();
        while let Some(e) = p.next_event().unwrap() {
            if let StreamEvent::Video(f) = e {
                times.push((f.time_ms, f.keyframe));
            }
        }
        // 200ms elapsed across the wrap, divided by scale 2 => 100.
        assert_eq!(times, vec![(0, true), (100, false)]);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut p = StreamParser::default();
        p.push(b"nope\x01\x00");
        assert_eq!(p.next_event().unwrap_err(), ProtocolError::BadStreamMagic);
    }

    #[test]
    fn keyframe_detection() {
        assert!(is_keyframe(&[0, 0, 0, 1, 0x67, 0, 0, 1, 0x68, 0, 0, 1, 0x65], false));
        assert!(!is_keyframe(&[0, 0, 0, 1, 0x41], false));
        assert!(is_keyframe(&[0, 0, 1, 21 << 1, 1], true)); // CRA
        assert!(!is_keyframe(&[0, 0, 1, 1 << 1, 1], true));
    }
}
