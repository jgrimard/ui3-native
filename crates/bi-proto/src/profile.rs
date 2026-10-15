//! Client-side streaming profiles. Port of `StreamingProfile` (ui3.js:26025), `quality_qp_map` (ui3.js:27180),
//! the default profile set (`GenerateDefaultProfiles`, ui3.js:26631) and `getSeekPreviewQualityArgs` (ui3.js:26514).

use serde::{Deserialize, Serialize};
use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VCodec {
    #[default]
    H264,
    Jpeg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RateControl {
    #[default]
    Vbr,
    Cbr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CodecPref {
    #[default]
    Inherit,
    H264,
    H265,
}

pub const CODEC_FLAG_H264: u32 = 0b01;
pub const CODEC_FLAG_H265: u32 = 0b10;

/// Codecs this client can decode. The native client decodes both, which is what lets Blue Iris
/// send H.265 untouched (`vcs=3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportedCodecs {
    pub h264: bool,
    pub h265: bool,
}

impl Default for SupportedCodecs {
    fn default() -> Self {
        SupportedCodecs { h264: true, h265: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamingProfile {
    pub name: String,
    pub abbr: String,
    /// "#RRGGBB"
    pub abbr_color: String,
    pub vcodec: VCodec,
    /// Server-side stream index 0..=2.
    pub stream: i32,
    /// -1 = inherit
    pub w: i32,
    pub h: i32,
    /// Blue Iris 6 quality %, -1 = inherit.
    pub q6: i32,
    pub rc: RateControl,
    pub kbps: i32,
    pub fps: i32,
    pub gop: i32,
    /// 0 inherit, 1 disable, 2 enable (sent as value-1).
    pub zfl: i32,
    /// 0 inherit, else preset index + 1 (ultrafast, superfast, veryfast).
    pub preset: i32,
    /// 0 inherit, else profile index + 1.
    pub profile: i32,
    /// 0 inherit, 1 disable, 2 enable.
    pub direct_to_wire: i32,
    /// 0 inherit, 1 disable, 2 enable.
    pub full_range_color: i32,
    pub codec_pref: CodecPref,
}

impl Default for StreamingProfile {
    fn default() -> Self {
        StreamingProfile {
            name: "Unnamed Streaming Profile".into(),
            abbr: String::new(),
            abbr_color: "#004882".into(),
            vcodec: VCodec::H264,
            stream: 0,
            w: -1,
            h: -1,
            q6: quality::bi5_to_bi6_quality(50),
            rc: RateControl::Vbr,
            kbps: -1,
            fps: -1,
            gop: 1000,
            zfl: 0,
            preset: 0,
            profile: 0,
            direct_to_wire: 0,
            full_range_color: 0,
            codec_pref: CodecPref::Inherit,
        }
    }
}

pub const BITRATE_FALLBACK_KBPS: i32 = 24000;
pub const STREAMING_QUALITY_DEFAULT: i32 = 50;

pub fn is_kbps_valid(k: i32) -> bool {
    (10..=100000).contains(&k)
}

impl StreamingProfile {
    pub fn display_name(&self) -> String {
        self.name.replace('^', "")
    }

    /// `SetQualityBI5`
    pub fn set_quality_bi5(&mut self, q: i32) {
        self.q6 = if q < 0 {
            -1
        } else if self.vcodec == VCodec::H264 {
            quality::bi5_to_bi6_quality(q)
        } else {
            q
        };
    }

    /// Bounding box of this profile (square if only one dimension set).
    pub fn rect(&self) -> Option<(i32, i32)> {
        match (self.w > 0, self.h > 0) {
            (true, true) => Some((self.w, self.h)),
            (true, false) => Some((self.w, self.w)),
            (false, true) => Some((self.h, self.h)),
            _ => None,
        }
    }

    /// `GetSizeToRequest` (ui3.js:23752) without the JPEG zoom/dpi heuristics: start from the source's native size,
    /// confine to the profile bounding box (rotated to match orientation), round and make divisible by 8.
    pub fn size_to_request(&self, native: (i32, i32), min_dimension: Option<i32>) -> (i32, i32) {
        let (mut w, mut h) = (native.0.max(1) as f64, native.1.max(1) as f64);
        if let Some((mut bw, mut bh)) = self.rect() {
            if ((bw as f64 / bh as f64) < 1.0) != ((w / h) < 1.0) {
                std::mem::swap(&mut bw, &mut bh);
            }
            let scale = (bw as f64 / w).min(bh as f64 / h);
            if scale < 1.0 {
                w *= scale;
                h *= scale;
            }
        }
        if let Some(m) = min_dimension {
            let m = m as f64;
            if w < m && h < m {
                let scale = (m / w).min(m / h);
                w *= scale;
                h *= scale;
            }
        }
        let r8 = |v: f64| -> i32 {
            let v = v.round() as i32;
            let r = v - v % 8;
            r.max(8)
        };
        (r8(w), r8(h))
    }

    /// `GetUrlArgs` (ui3.js:26127). `size` is the size to request (see [`Self::size_to_request`]).
    pub fn url_args(&self, size: (i32, i32), codecs: SupportedCodecs, bitrate_max_kbps: Option<i32>) -> String {
        let mut sb = String::new();
        let _ = write!(sb, "&stream={}&w={}&h={}", self.stream, size.0, size.1);
        match self.vcodec {
            VCodec::H264 => {
                // A direct-to-wire profile without a quality leaves it to Blue Iris: high `q` at full resolution makes
                // Blue Iris skip encoded reference frames when it transcodes (recordings), corrupting the picture.
                if self.rc == RateControl::Vbr && !(self.q6 < 0 && self.direct_to_wire == 2) {
                    let q = if self.q6 < 0 { STREAMING_QUALITY_DEFAULT } else { self.q6 };
                    let _ = write!(sb, "&q={}", q.clamp(0, 100));
                }
                let mut kbps = if is_kbps_valid(self.kbps) { self.kbps } else { BITRATE_FALLBACK_KBPS };
                kbps = kbps.clamp(10, 100000);
                if let Some(max) = bitrate_max_kbps
                    && is_kbps_valid(max)
                    && max < kbps
                {
                    kbps = max;
                }
                let _ = write!(sb, "&kbps={kbps}");
                if self.fps >= 0 {
                    let _ = write!(sb, "&fps={}", self.fps);
                }
                if self.gop >= 1 {
                    let _ = write!(sb, "&gop={}", self.gop);
                }
                if self.zfl > 0 {
                    let _ = write!(sb, "&zfl={}", self.zfl - 1);
                }
                if self.preset > 0 {
                    let _ = write!(sb, "&preset={}", self.preset - 1);
                }
                if self.profile > 0 {
                    let _ = write!(sb, "&profile={}", self.profile - 1);
                }
                if self.direct_to_wire > 0 {
                    let _ = write!(sb, "&d2w={}", self.direct_to_wire - 1);
                }
                if self.full_range_color > 0 {
                    let _ = write!(sb, "&frc={}", self.full_range_color - 1);
                }
                match self.codec_pref {
                    CodecPref::H264 if codecs.h264 => {
                        let _ = write!(sb, "&vc={CODEC_FLAG_H264}");
                    }
                    CodecPref::H265 if codecs.h265 => {
                        let _ = write!(sb, "&vc={CODEC_FLAG_H265}");
                    }
                    _ => {}
                }
                let vcs = (if codecs.h264 { CODEC_FLAG_H264 } else { 0 }) | (if codecs.h265 { CODEC_FLAG_H265 } else { 0 });
                if vcs != 0 {
                    let _ = write!(sb, "&vcs={vcs}");
                }
                sb.push_str(if self.rc == RateControl::Vbr { "&rc=0" } else { "&rc=1" });
            }
            VCodec::Jpeg => {
                if self.q6 >= 0 {
                    let _ = write!(sb, "&q={}", self.q6);
                }
            }
        }
        sb
    }

    pub fn tooltip(&self) -> String {
        let mut s = String::new();
        match self.vcodec {
            VCodec::H264 => s.push_str(match self.codec_pref {
                CodecPref::Inherit => "h.264 or h.265 ",
                CodecPref::H264 => "h264 ",
                CodecPref::H265 => "h265 ",
            }),
            VCodec::Jpeg => s.push_str("jpeg "),
        }
        match (self.w > 0, self.h > 0) {
            (true, true) => {
                let _ = write!(s, "{}x{} ", self.w, self.h);
            }
            (true, false) => {
                let _ = write!(s, "{}w ", self.w);
            }
            (false, true) => {
                let _ = write!(s, "{}h ", self.h);
            }
            _ => {}
        }
        if self.fps >= 0 {
            let _ = write!(s, "@{}fps ", self.fps);
        }
        match self.rc {
            RateControl::Vbr => {
                if self.q6 > -1 {
                    let _ = write!(s, "q{} ", self.q6);
                }
                if is_kbps_valid(self.kbps) {
                    let _ = write!(s, "<{} Kbps ", self.kbps);
                }
            }
            RateControl::Cbr => {
                if is_kbps_valid(self.kbps) {
                    let _ = write!(s, "{} Kbps ", self.kbps);
                }
            }
        }
        let _ = write!(s, "\nInherits from Streaming {}", self.stream);
        s
    }
}

fn mk(name: &str, abbr: &str, clr: &str, vcodec: VCodec, w: i32, h: i32, q5: i32, kbps: i32) -> StreamingProfile {
    let mut p = StreamingProfile { name: name.into(), abbr: abbr.into(), abbr_color: clr.into(), vcodec, w, h, kbps, ..Default::default() };
    p.set_quality_bi5(q5);
    p
}

fn vbr(mut p: StreamingProfile) -> StreamingProfile {
    p.preset = 2;
    p.zfl = 2;
    p
}

/// `GenerateDefaultProfiles` (ui3.js:26631).
pub fn default_profiles(include_8k: bool) -> Vec<StreamingProfile> {
    use VCodec::*;
    let mut v = Vec::new();
    if include_8k {
        v.push(vbr(mk("4320p VBR^", "V8", "#8F29AE", H264, 7680, 4320, 20, 5000)));
    }
    v.push(vbr(mk("2160p VBR^", "V4", "#008248", H264, 3840, 2160, 20, 3000)));
    v.push(vbr(mk("1080p VBR^", "V2", "#004882", H264, 1920, 1080, 20, 1000)));
    if include_8k {
        v.push(mk("4320p^", "8K", "#8F29AE", H264, 7680, 4320, 50, 16384));
    }
    v.push(mk("2160p^", "4K", "#008248", H264, 3840, 2160, 50, 8192));
    v.push(mk("1440p^", "4MP", "#0048A2", H264, 2560, 1440, 40, 4096));
    v.push(mk("1080p^", "2MP", "#004882", H264, 1920, 1080, 35, 2048));
    v.push(mk("720p^", "1MP", "#003862", H264, 1280, 720, 35, 1024));
    v.push(mk("480p", "480p", "#884400", H264, 856, 480, 30, 456));
    v.push(mk("360p", "360p", "#883000", H264, 640, 360, 30, 256));
    v.push(mk("240p", "240p", "#882000", H264, 427, 240, 30, 114));
    v.push(mk("144p", "144p", "#880000", H264, 256, 144, 30, 41));
    v.push(mk("Jpeg HD", "HD", "#004882", Jpeg, 99999, 99999, 85, -1));
    v.push(mk("Jpeg SD", "SD", "#884400", Jpeg, 640, 640, 50, -1));
    v.push(mk("Jpeg Low", "LD", "#880000", Jpeg, 320, 320, 20, -1));
    v
}

/// A "native" profile: no server-side transcoding hints at all, direct-to-wire enabled, both codecs accepted.
/// This is the profile that lets Blue Iris pass the camera's H.265 straight through.
pub fn native_passthrough_profile() -> StreamingProfile {
    StreamingProfile {
        name: "Native (direct H.264/H.265)^".into(),
        abbr: "D2W".into(),
        abbr_color: "#6A1B9A".into(),
        direct_to_wire: 2,
        codec_pref: CodecPref::Inherit,
        gop: 1000,
        q6: -1,
        ..Default::default()
    }
}

/// `getSeekPreviewQualityArgs` (ui3.js:26514). `bitrate_mbps` is the profile's kbps/1000 if valid, otherwise the measured rate.
pub fn seek_preview_quality_args(profile: &StreamingProfile, native: (i32, i32), bitrate_mbps: f64, is_timeline: bool) -> String {
    let (mut w, mut h) = profile.size_to_request(native, None);
    let mut quality: Option<i32> = None;
    if profile.vcodec == VCodec::Jpeg && profile.q6 != 0 {
        quality = Some(profile.q6);
    }
    if !is_timeline && profile.vcodec == VCodec::H264 {
        let mbps = if is_kbps_valid(profile.kbps) { profile.kbps as f64 / 1000.0 } else { bitrate_mbps };
        let (bbox, q) = if mbps < 0.1 {
            (Some(320), 20)
        } else if mbps < 0.3 {
            (Some(480), 20)
        } else if mbps < 0.5 {
            (Some(640), 25)
        } else if mbps < 0.75 {
            (Some(640), 40)
        } else if mbps < 1.5 {
            (Some(1280), 50)
        } else if mbps < 4.0 {
            (Some(1920), 60)
        } else {
            (None, 75)
        };
        if let Some(b) = bbox {
            let scale = (b as f64 / w as f64).min(b as f64 / h as f64);
            if scale < 1.0 {
                w = (w as f64 * scale).round() as i32;
                h = (h as f64 * scale).round() as i32;
            }
        }
        quality = Some(q);
    }
    let mut s = format!("&w={w}&h={h}&stream={}", profile.stream);
    if let Some(q) = quality {
        let _ = write!(s, "&q={q}");
    }
    s
}

/// `quality_qp_map` (ui3.js:27180).
pub mod quality {
    const BI5_H264_MIN_QP: i32 = 1;
    const BI5_H264_MAX_QP: i32 = 51;
    const BI6_H264_MIN_QP: i32 = 16;
    const BI6_H264_MAX_QP: i32 = 48;
    const BI6_H265_MIN_QP: i32 = 19;
    const BI6_H265_MAX_QP: i32 = 51;
    const BI5_RANGE: i32 = BI5_H264_MAX_QP - BI5_H264_MIN_QP;
    const BI6_RANGE: i32 = BI6_H264_MAX_QP - BI6_H264_MIN_QP;
    const BI6_H265_RANGE: i32 = BI6_H265_MAX_QP - BI6_H265_MIN_QP;

    fn ceil_div(a: i32, b: i32) -> i32 {
        (a as f64 / b as f64).ceil() as i32
    }

    fn qualities_from_qp(qp: i32, qp_min: i32, qp_range: i32) -> Vec<i32> {
        let k = (qp_range + qp_min) - qp;
        if k < 0 || k > qp_range {
            return vec![];
        }
        let lo = ceil_div(100 * k, qp_range);
        let hi = ceil_div(100 * (k + 1), qp_range) - 1;
        (lo..=hi.min(100)).collect()
    }

    pub fn bi5_q_to_h264_qp(q: i32) -> i32 {
        BI5_H264_MAX_QP - (q * BI5_RANGE) / 100
    }
    pub fn bi6_q_to_h264_qp(q: i32) -> i32 {
        BI6_H264_MAX_QP - (q * BI6_RANGE) / 100
    }
    pub fn bi6_q_to_h265_qp(q: i32) -> i32 {
        BI6_H265_MAX_QP - (q * BI6_H265_RANGE) / 100
    }
    pub fn bi5_to_bi6_quality(q: i32) -> i32 {
        let qs = qualities_from_qp(bi5_q_to_h264_qp(q), BI6_H264_MIN_QP, BI6_RANGE);
        if !qs.is_empty() {
            return qs[(qs.len() - 1) / 2];
        }
        if q < 50 { 0 } else { 100 }
    }
    pub fn bi6_to_bi5_quality(q: i32) -> i32 {
        let qs = qualities_from_qp(bi6_q_to_h264_qp(q), BI5_H264_MIN_QP, BI5_RANGE);
        if !qs.is_empty() {
            return qs[(qs.len() - 1) / 2];
        }
        if q < 50 { 0 } else { 70 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_mapping_matches_ui3() {
        // UI3 comment: "For H.264, 69-71% in BI6 is equivalent to 50%-51% in BI5."
        let q = quality::bi5_to_bi6_quality(50);
        assert!((69..=71).contains(&q), "{q}");
        assert_eq!(quality::bi5_q_to_h264_qp(50), 26);
        assert_eq!(quality::bi6_q_to_h264_qp(q), 26);
    }

    #[test]
    fn url_args_for_default_1080p_vbr() {
        let p = default_profiles(false).into_iter().find(|p| p.name == "1080p VBR^").unwrap();
        let size = p.size_to_request((3840, 2160), None);
        assert_eq!(size, (1920, 1080));
        let args = p.url_args(size, SupportedCodecs::default(), None);
        assert_eq!(
            args,
            format!("&stream=0&w=1920&h=1080&q={}&kbps=1000&gop=1000&zfl=1&preset=1&vcs=3&rc=0", p.q6)
        );
    }

    #[test]
    fn passthrough_requests_h265_ok() {
        let p = native_passthrough_profile();
        let args = p.url_args((2560, 1440), SupportedCodecs::default(), None);
        assert!(args.contains("&d2w=1"));
        assert!(args.contains("&vcs=3"));
        assert!(!args.contains("&q="), "{args}");
    }

    #[test]
    fn size_rotates_profile_box_for_portrait() {
        let p = mk("x", "", "", VCodec::H264, 1920, 1080, 50, 1000);
        assert_eq!(p.size_to_request((1080, 1920), None), (1080, 1920));
        assert_eq!(p.size_to_request((2160, 3840), None), (1080, 1920));
    }

    #[test]
    fn seek_preview_args() {
        let p = mk("x", "", "", VCodec::H264, 1920, 1080, 50, 1000);
        assert_eq!(seek_preview_quality_args(&p, (1920, 1080), 0.0, false), "&w=1280&h=720&stream=0&q=50");
    }
}
