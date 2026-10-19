//! Media side of the native UI3 port: FFmpeg/D3D11VA video decode, JPEG decode and audio output.

pub mod audio;
pub mod d3d;
pub mod decoder;
pub mod elementary;
pub mod flac;
pub mod jpeg;
pub mod mux;

pub use decoder::{DecodedFrame, HwAccel, Nv12Frame, VideoDecoder};

/// FFmpeg libavcodec version string, e.g. "62.28.103".
pub fn ffmpeg_version() -> String {
    let v = unsafe { ffmpeg_sys_next::avcodec_version() };
    format!("{}.{}.{}", v >> 16, (v >> 8) & 0xFF, v & 0xFF)
}

/// Silences FFmpeg's stderr logging except errors.
pub fn quiet_ffmpeg_logs() {
    unsafe { ffmpeg_sys_next::av_log_set_level(ffmpeg_sys_next::AV_LOG_ERROR) }
}
