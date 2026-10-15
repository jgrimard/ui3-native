//! MP4 muxing of Blue Iris Annex-B streams without re-encoding (H.264 and H.265).
//!
//! Native replacement for UI3's in-browser AVI exporter (`ClientsideClipExportDialog` / `ClipExportStreamer`,
//! ui3.js:31026-31316), which cannot handle H.265. FFmpeg's mov muxer converts Annex-B extradata and packets
//! to avcC/hvcC internally.

use anyhow::{Result, bail};
use bi_proto::VideoCodec;
use bi_proto::stream_parser::nal_types;
use ffmpeg_sys_next as ff;
use std::ffi::CString;
use std::path::Path;
use std::ptr;

pub struct Mp4Muxer {
    fmt: *mut ff::AVFormatContext,
    stream: *mut ff::AVStream,
    pkt: *mut ff::AVPacket,
    started: bool,
    first_pts: Option<i64>,
    last_dts: i64,
    finished: bool,
}

unsafe impl Send for Mp4Muxer {}

/// Splits Annex-B data into NAL units (including their start codes).
fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let s = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push(s);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let e = starts.get(k + 1).copied().unwrap_or(data.len());
        out.push(&data[s..e]);
    }
    out
}

/// Parameter-set NAL units (VPS/SPS/PPS) at the start of a keyframe, in Annex-B form.
pub fn parameter_sets(data: &[u8], codec: VideoCodec) -> Vec<u8> {
    let h265 = codec == VideoCodec::H265;
    let mut out = Vec::new();
    for nal in split_nals(data) {
        let t = nal_types(nal, h265).next().unwrap_or(0);
        let is_ps = if h265 { (32..=34).contains(&t) } else { t == 7 || t == 8 };
        if is_ps {
            out.extend_from_slice(nal);
        }
    }
    out
}

impl Mp4Muxer {
    pub fn create(path: &Path, codec: VideoCodec, width: i32, height: i32) -> Result<Self> {
        let id = match codec {
            VideoCodec::H264 => ff::AVCodecID::AV_CODEC_ID_H264,
            VideoCodec::H265 => ff::AVCodecID::AV_CODEC_ID_HEVC,
            other => bail!("cannot mux {other:?} into MP4"),
        };
        unsafe {
            let cpath = CString::new(path.to_string_lossy().as_bytes())?;
            let mut fmt: *mut ff::AVFormatContext = ptr::null_mut();
            let r = ff::avformat_alloc_output_context2(&mut fmt, ptr::null(), c"mp4".as_ptr(), cpath.as_ptr());
            if r < 0 || fmt.is_null() {
                bail!("avformat_alloc_output_context2: {}", crate::decoder::av_err(r));
            }
            let st = ff::avformat_new_stream(fmt, ptr::null());
            let par = (*st).codecpar;
            (*par).codec_type = ff::AVMediaType::AVMEDIA_TYPE_VIDEO;
            (*par).codec_id = id;
            (*par).width = width;
            (*par).height = height;
            if codec == VideoCodec::H265 {
                // "hvc1" plays in more players (QuickTime, browsers) than the default "hev1".
                (*par).codec_tag = u32::from_le_bytes(*b"hvc1");
            }
            (*st).time_base = ff::AVRational { num: 1, den: 1000 };
            let r = ff::avio_open(&mut (*fmt).pb, cpath.as_ptr(), ff::AVIO_FLAG_WRITE as i32);
            if r < 0 {
                ff::avformat_free_context(fmt);
                bail!("avio_open({}): {}", path.display(), crate::decoder::av_err(r));
            }
            Ok(Mp4Muxer { fmt, stream: st, pkt: ff::av_packet_alloc(), started: false, first_pts: None, last_dts: -1, finished: false })
        }
    }

    /// Writes one access unit. The first call must be a keyframe carrying parameter sets.
    pub fn write(&mut self, data: &[u8], time_ms: i64, keyframe: bool, codec: VideoCodec) -> Result<()> {
        unsafe {
            if !self.started {
                if !keyframe {
                    return Ok(());
                }
                let ps = parameter_sets(data, codec);
                if ps.is_empty() {
                    bail!("first keyframe has no parameter sets");
                }
                let par = (*self.stream).codecpar;
                (*par).extradata = ff::av_mallocz(ps.len() + ff::AV_INPUT_BUFFER_PADDING_SIZE as usize) as *mut u8;
                ptr::copy_nonoverlapping(ps.as_ptr(), (*par).extradata, ps.len());
                (*par).extradata_size = ps.len() as i32;
                let mut opts: *mut ff::AVDictionary = ptr::null_mut();
                ff::av_dict_set(&mut opts, c"movflags".as_ptr(), c"+faststart".as_ptr(), 0);
                let r = ff::avformat_write_header(self.fmt, &mut opts);
                ff::av_dict_free(&mut opts);
                if r < 0 {
                    bail!("avformat_write_header: {}", crate::decoder::av_err(r));
                }
                self.started = true;
            }
            let base = *self.first_pts.get_or_insert(time_ms);
            let mut ts = (time_ms - base).max(0);
            if ts <= self.last_dts {
                ts = self.last_dts + 1;
            }
            self.last_dts = ts;
            let r = ff::av_new_packet(self.pkt, data.len() as i32);
            if r < 0 {
                bail!("av_new_packet");
            }
            ptr::copy_nonoverlapping(data.as_ptr(), (*self.pkt).data, data.len());
            (*self.pkt).pts = ts;
            (*self.pkt).dts = ts;
            (*self.pkt).stream_index = 0;
            if keyframe {
                (*self.pkt).flags |= ff::AV_PKT_FLAG_KEY as i32;
            }
            let tb = (*self.stream).time_base;
            ff::av_packet_rescale_ts(self.pkt, ff::AVRational { num: 1, den: 1000 }, tb);
            let r = ff::av_interleaved_write_frame(self.fmt, self.pkt);
            if r < 0 {
                bail!("av_interleaved_write_frame: {}", crate::decoder::av_err(r));
            }
            Ok(())
        }
    }

    pub fn finish(mut self) -> Result<()> {
        self.finish_inner()
    }

    fn finish_inner(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        unsafe {
            if self.started {
                let r = ff::av_write_trailer(self.fmt);
                if r < 0 {
                    bail!("av_write_trailer: {}", crate::decoder::av_err(r));
                }
            }
            ff::avio_closep(&mut (*self.fmt).pb);
        }
        Ok(())
    }
}

impl Drop for Mp4Muxer {
    fn drop(&mut self) {
        let _ = self.finish_inner();
        unsafe {
            ff::av_packet_free(&mut self.pkt);
            ff::avformat_free_context(self.fmt);
        }
    }
}
