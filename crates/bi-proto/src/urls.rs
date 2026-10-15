//! Request path builders. Mirrors the URL construction in `FetchH264VideoModule.OpenVideo` (ui3.js:20674-20827),
//! `GroupCfg.GetUrlArgs` (ui3.js:25544), `ClipOverlayCfg.GetUrlArgs` (ui3.js:25446) and the thumbnail/snapshot helpers.
//!
//! All functions return a path relative to the server base URL, *without* the session argument; the client appends
//! `session=` itself (see [`crate::client::BiClient::url`]).

use std::fmt::Write;

/// `&audio=` value: 0 none, 1 mu-law, 2 FLAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioRequest {
    #[default]
    None,
    MuLaw,
    Flac,
}

impl AudioRequest {
    pub fn arg(self) -> &'static str {
        match self {
            AudioRequest::None => "&audio=0",
            AudioRequest::MuLaw => "&audio=1",
            AudioRequest::Flac => "&audio=2",
        }
    }
}

/// Tri-state override: 0 unset, 1 off, 2 on (UI3 convention).
pub type TriState = u8;

/// `GroupCfg` flag/mask args.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupArgs {
    pub show_camera_names: TriState,
    pub show_camera_borders: TriState,
    pub show_hidden_cameras: TriState,
    pub hide_disabled_cameras: TriState,
    pub hide_inactive_cameras: TriState,
}

impl GroupArgs {
    pub fn args(&self) -> String {
        let (mut flags, mut mask) = (0u32, 0u32);
        for (i, v) in [self.show_camera_names, self.show_camera_borders, self.show_hidden_cameras, self.hide_disabled_cameras, self.hide_inactive_cameras]
            .into_iter()
            .enumerate()
        {
            if v == 1 || v == 2 {
                mask |= 1 << i;
                if v == 2 {
                    flags |= 1 << i;
                }
            }
        }
        if mask > 0 { format!("&flags={flags}&mask={mask}") } else { String::new() }
    }
}

/// `ClipOverlayCfg` args (motion overlay / text overlay), tri-state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayArgs {
    pub motion: TriState,
    pub text: TriState,
}

impl OverlayArgs {
    pub fn args(&self) -> String {
        let mut s = String::new();
        match self.motion {
            1 => s.push_str("&addmotion=0"),
            2 => s.push_str("&addmotion=1"),
            _ => {}
        }
        match self.text {
            1 => s.push_str("&addoverlay=0"),
            2 => s.push_str("&addoverlay=1"),
            _ => {}
        }
        s
    }
}

/// Skip dead air setting: None = inherit (`&skipdeadair=`).
pub fn skip_dead_air_arg(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "&skipdeadair=1",
        Some(false) => "&skipdeadair=0",
        None => "&skipdeadair=",
    }
}

/// `GetSpeedBasedSubstreamArgument` (ui3.js:12208).
pub fn speed_substream_arg(play_speed: f64, reverse: bool, fwd_threshold: f64, rev_threshold: f64) -> &'static str {
    if play_speed != 1.0 {
        let t = if reverse { rev_threshold } else { fwd_threshold };
        if t > 0.0 && play_speed >= t {
            return "&decode=-1";
        }
    }
    ""
}

pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'@' => out.push(b as char),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Live stream: `video/<id>/2.0?audio..profile..group&extend=2`.
pub fn live_video(cam_id: &str, audio: AudioRequest, profile_args: &str, group_args: &str) -> String {
    format!("video/{}/2.0?{}{}{}&extend=2", encode(cam_id), audio.arg().trim_start_matches('&'), profile_args, group_args)
}

/// Where to start a recorded clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipStart {
    /// 0..=10000 of the clip.
    Pos(u32),
    /// Exact milliseconds into the clip.
    Time(i64),
}

/// Recorded clip stream: `file/clips/<path>?pos|time&speed=..&audio..profile&extend=2..overlay..substream`.
/// `speed` is 100 * rate (negative = reverse, 0 = single JPEG).
pub fn clip_video(clip_path: &str, start: ClipStart, speed: i32, audio: AudioRequest, profile_args: &str, overlay_args: &str, substream_arg: &str) -> String {
    let mut s = format!("file/clips/{}?", encode(clip_path));
    if let ClipStart::Pos(p) = start {
        let p = if speed == 0 { p.min(9999) } else { p.min(10000) };
        let _ = write!(s, "pos={p}");
    }
    let _ = write!(s, "&speed={speed}");
    // Audio is only delivered at exactly 100 speed.
    s.push_str(if speed == 100 { audio.arg() } else { "" });
    if speed == 0 {
        s.push_str(&replace_arg(profile_args, "q", "50"));
    } else {
        s.push_str(profile_args);
    }
    s.push_str("&extend=2");
    if let ClipStart::Time(ms) = start {
        let _ = write!(s, "&time={}", ms.max(0));
    }
    s.push_str(overlay_args);
    s.push_str(substream_arg);
    s.replace("?&", "?")
}

/// Single JPEG frame of a clip at `ms` (seek-bar scrubbing, `loadSeekHintImg`). `quality_args` from
/// [`crate::profile::seek_preview_quality_args`]; add `&decode=-1` to use the sub stream.
pub fn clip_frame_jpeg(clip_path: &str, ms: i64, quality_args: &str, extra: &str) -> String {
    format!("file/clips/{}?time={}{}{}", encode(clip_path), ms.max(0), quality_args, extra)
}

/// Timeline stream: `time/<id>?opaque=..&pos=<utcMs>[&jump]..&speed&skipdeadair&extend=2..`.
#[allow(clippy::too_many_arguments)]
pub fn timeline_video(
    id: &str,
    opaque: &str,
    pos_utc_ms: i64,
    jump: i32,
    audio: AudioRequest,
    profile_args: &str,
    group_args: &str,
    speed: i32,
    skip_dead_air: Option<bool>,
    overlay_args: &str,
    substream_arg: &str,
) -> String {
    let mut s = format!("time/{}?opaque={}&pos={}", encode(id), encode(opaque), pos_utc_ms);
    if jump != 0 {
        let _ = write!(s, "&jump={jump}");
    }
    s.push_str(audio.arg());
    s.push_str(profile_args);
    s.push_str(group_args);
    let _ = write!(s, "&speed={speed}");
    s.push_str(skip_dead_air_arg(skip_dead_air));
    s.push_str("&extend=2");
    s.push_str(overlay_args);
    s.push_str(substream_arg);
    s
}

/// Timeline single frame (`downloadSeekPreview`, ui3.js:10456). Frame time returns in `X-UTC`.
pub fn timeline_frame_jpeg(id: &str, opaque: &str, pos_utc_ms: i64, jump: i32, quality_args: &str, group_args: &str, overlay_args: &str) -> String {
    let mut s = format!("time/{}?jpeg&speed=0&pos={}&opaque={}", encode(id), pos_utc_ms, encode(opaque));
    if jump != 0 {
        let _ = write!(s, "&jump={jump}");
    }
    s.push_str(quality_args);
    s.push_str(group_args);
    s.push_str(overlay_args);
    s
}

/// Control a running timeline stream: `time/set?opaque=..&speed=..|&pos=..|&jump=..`.
pub fn timeline_set(opaque: &str, speed: Option<i32>, pos: Option<i64>, jump: Option<i32>) -> String {
    let mut s = format!("time/set?opaque={}", encode(opaque));
    if let Some(v) = speed {
        let _ = write!(s, "&speed={v}");
    }
    if let Some(v) = pos {
        let _ = write!(s, "&pos={v}");
    }
    if let Some(v) = jump {
        let _ = write!(s, "&jump={v}");
    }
    s
}

/// Live JPEG: `image/<id>?...`.
pub fn live_jpeg(id: &str, profile_args: &str, group_args: &str, overlay_args: &str, nocache: u64) -> String {
    format!("image/{}?nc={}{}{}{}", encode(id), nocache, profile_args, group_args, overlay_args).replace("?&", "?")
}

/// High quality isolated snapshot (`JpegSnapshotArgs`).
pub fn live_snapshot(id: &str, quality_bi5: i32, now_ms: i64) -> String {
    format!("image/{}?time={}&decode=1&w=7680&h=7680&q={}", encode(id), now_ms, quality_bi5.clamp(0, 100))
}

pub fn clip_snapshot(clip_path: &str, ms: i64, quality_bi5: i32) -> String {
    format!("file/clips/{}?time={}&decode=1&w=7680&h=7680&q={}", encode(clip_path), ms.max(0), quality_bi5.clamp(0, 100))
}

/// Clip/alert thumbnail (`GetThumbnailPath`).
pub fn thumbnail(thumb_path: &str) -> String {
    format!("thumbs/{}", encode(thumb_path))
}

/// Native-resolution alert image.
pub fn alert_full_jpeg(alert_path: &str) -> String {
    format!("alerts/{}?fulljpeg", encode(alert_path))
}

/// Hover preview animation frame (`ClipThumbnailVideoPreview_BruteForce`).
pub fn clip_preview_frame(clip_path: &str, ms: i64, height: i32) -> String {
    format!("file/clips/{}?time={}&cache=1&h={}", encode(clip_path), ms.max(0), height)
}

pub fn ptz_preset_image(cam_id: &str, preset: i32) -> String {
    format!("image/{}/preset_{}.jpg", encode(cam_id), preset)
}

/// Download link for a clip file.
/// With `original_name`, the server names the download after the original file (`original_name=1`).
pub fn clip_download(clip_path: &str, original_name: bool) -> String {
    format!("clips/{}{}", encode(clip_path), if original_name { "?original_name=1" } else { "" })
}

/// Replaces (or appends) a `&key=value` argument in a query fragment.
pub fn replace_arg(args: &str, key: &str, value: &str) -> String {
    let mut out = String::new();
    for part in args.split('&').filter(|p| !p.is_empty()) {
        let k = part.split('=').next().unwrap_or("");
        if k != key {
            out.push('&');
            out.push_str(part);
        }
    }
    let _ = write!(out, "&{key}={value}");
    out
}

/// Removes `&key=...` arguments.
pub fn remove_args(args: &str, keys: &[&str]) -> String {
    let mut out = String::new();
    for part in args.split('&').filter(|p| !p.is_empty()) {
        let k = part.split('=').next().unwrap_or("");
        if !keys.contains(&k) {
            out.push('&');
            out.push_str(part);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_url() {
        assert_eq!(live_video("Front Door", AudioRequest::MuLaw, "&stream=0&w=8", "&flags=1&mask=1"), "video/Front%20Door/2.0?audio=1&stream=0&w=8&flags=1&mask=1&extend=2");
    }

    #[test]
    fn clip_url_paused_forces_q50_and_no_audio() {
        let u = clip_video("@123.bvr", ClipStart::Pos(10000), 0, AudioRequest::MuLaw, "&stream=0&q=70&kbps=1000", "", "");
        assert_eq!(u, "file/clips/@123.bvr?pos=9999&speed=0&stream=0&kbps=1000&q=50&extend=2");
    }

    #[test]
    fn clip_url_time_seek_reverse() {
        let u = clip_video("@1.bvr", ClipStart::Time(5000), -200, AudioRequest::MuLaw, "&stream=0", "&addmotion=1", "&decode=-1");
        assert_eq!(u, "file/clips/@1.bvr?speed=-200&stream=0&extend=2&time=5000&addmotion=1&decode=-1");
    }

    #[test]
    fn group_args() {
        let g = GroupArgs { show_camera_names: 2, show_camera_borders: 1, ..Default::default() };
        assert_eq!(g.args(), "&flags=1&mask=3");
        assert_eq!(GroupArgs::default().args(), "");
    }
}
