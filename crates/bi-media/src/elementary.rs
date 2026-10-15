//! Reads access units from a raw Annex-B `.h264` / `.hevc` file (or any container FFmpeg can demux) and can
//! synthesize Blue Iris `.bistream` captures from them. Used by tests and the offline mock server.

use anyhow::{Result, bail};
use bi_proto::VideoCodec;
use bi_proto::stream_writer::StreamWriter;
use ffmpeg_sys_next as ff;
use std::ffi::CString;
use std::path::Path;
use std::ptr;

pub struct AccessUnit {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

pub struct Elementary {
    pub codec: VideoCodec,
    pub width: i32,
    pub height: i32,
    pub fps: f64,
    pub units: Vec<AccessUnit>,
}

/// Demuxes all video packets. Raw elementary inputs come out as Annex-B access units; MP4/MKV inputs are run
/// through the `*_mp4toannexb` bitstream filter.
pub fn read_elementary(path: &Path) -> Result<Elementary> {
    unsafe {
        let cpath = CString::new(path.to_string_lossy().as_bytes())?;
        let mut fmt: *mut ff::AVFormatContext = ptr::null_mut();
        let r = ff::avformat_open_input(&mut fmt, cpath.as_ptr(), ptr::null(), ptr::null_mut());
        if r < 0 {
            bail!("avformat_open_input({}): {}", path.display(), crate::decoder::av_err(r));
        }
        struct Guard(*mut ff::AVFormatContext);
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe { ff::avformat_close_input(&mut self.0) }
            }
        }
        let _g = Guard(fmt);
        ff::avformat_find_stream_info(fmt, ptr::null_mut());
        let idx = ff::av_find_best_stream(fmt, ff::AVMediaType::AVMEDIA_TYPE_VIDEO, -1, -1, ptr::null_mut(), 0);
        if idx < 0 {
            bail!("no video stream in {}", path.display());
        }
        let st = *(*fmt).streams.add(idx as usize);
        let par = (*st).codecpar;
        let codec = match (*par).codec_id {
            ff::AVCodecID::AV_CODEC_ID_H264 => VideoCodec::H264,
            ff::AVCodecID::AV_CODEC_ID_HEVC => VideoCodec::H265,
            other => bail!("unsupported codec {other:?}"),
        };
        let fr = (*st).avg_frame_rate;
        let fps = if fr.den > 0 && fr.num > 0 { fr.num as f64 / fr.den as f64 } else { 15.0 };
        let is_annexb = (*par).extradata_size == 0 || ((*par).extradata_size > 3 && *(*par).extradata == 0);

        let mut bsf: *mut ff::AVBSFContext = ptr::null_mut();
        if !is_annexb {
            let name = if codec == VideoCodec::H265 { c"hevc_mp4toannexb" } else { c"h264_mp4toannexb" };
            let f = ff::av_bsf_get_by_name(name.as_ptr());
            ff::av_bsf_alloc(f, &mut bsf);
            ff::avcodec_parameters_copy((*bsf).par_in, par);
            ff::av_bsf_init(bsf);
        }

        let mut units = Vec::new();
        let pkt = ff::av_packet_alloc();
        let h265 = codec == VideoCodec::H265;
        let mut push = |pkt: *mut ff::AVPacket| {
            let data = std::slice::from_raw_parts((*pkt).data, (*pkt).size as usize).to_vec();
            let keyframe = bi_proto::stream_parser::is_keyframe(&data, h265);
            units.push(AccessUnit { data, keyframe });
        };
        while ff::av_read_frame(fmt, pkt) >= 0 {
            if (*pkt).stream_index == idx {
                if bsf.is_null() {
                    push(pkt);
                } else if ff::av_bsf_send_packet(bsf, pkt) >= 0 {
                    while ff::av_bsf_receive_packet(bsf, pkt) >= 0 {
                        push(pkt);
                        ff::av_packet_unref(pkt);
                    }
                }
            }
            ff::av_packet_unref(pkt);
        }
        let mut pkt = pkt;
        ff::av_packet_free(&mut pkt);
        if !bsf.is_null() {
            ff::av_bsf_free(&mut bsf);
        }
        Ok(Elementary { codec, width: (*par).width, height: (*par).height, fps, units })
    }
}

/// Builds a `.bistream` byte buffer (as Blue Iris would send for `/file/clips/` at speed 100) from an elementary
/// stream, optionally interleaving 8 kHz mu-law audio.
pub fn synth_bistream(e: &Elementary, start_utc_ms: u64, mulaw: Option<&[u8]>) -> Vec<u8> {
    let fourcc = if e.codec == VideoCodec::H265 { b"H265" } else { b"H264" };
    let mut w = StreamWriter::new(fourcc, e.width, e.height, mulaw.map(|_| 8000));
    let n = e.units.len().max(1);
    let frame_ms = 1000.0 / e.fps;
    let mut audio_pos = 0usize;
    for (i, au) in e.units.iter().enumerate() {
        let t = (i as f64 * frame_ms) as u32;
        let pos = ((i as f64 / n as f64) * 10000.0) as u16;
        w.video(pos, t, start_utc_ms + t as u64, &au.data);
        if let Some(a) = mulaw {
            let want = (((i + 1) as f64 * frame_ms) * 8.0) as usize;
            let end = want.min(a.len());
            if end > audio_pos {
                w.audio(&a[audio_pos..end]);
                audio_pos = end;
            }
        }
    }
    w.end();
    w.out
}
