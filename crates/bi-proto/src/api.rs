//! Typed models for the Blue Iris `/json` API responses UI3 consumes.
//!
//! Every struct keeps unknown fields in `extra` so nothing Blue Iris sends is lost, and all
//! fields are parsed leniently (see [`crate::lenient`]).

use crate::lenient;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Raw `/json` envelope: `{result, session?, data?}`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct JsonResponse {
    #[serde(default, deserialize_with = "lenient::string")]
    pub result: String,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub data: Value,
}

impl JsonResponse {
    pub fn is_success(&self) -> bool {
        self.result == "success"
    }
    pub fn fail_reason(&self) -> Option<&str> {
        self.data.get("reason").and_then(|v| v.as_str())
    }
    /// `IsInvalidSession` (ui3.js:17344), Blue Iris 5.8.1.1+ semantics.
    pub fn is_invalid_session(&self) -> bool {
        self.result == "fail" && self.fail_reason().is_some_and(|r| r.eq_ignore_ascii_case("invalid session"))
    }
}

/// `data` of a successful `login` (ui3.js:17077).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct LoginData {
    #[serde(default, deserialize_with = "lenient::string")]
    pub user: String,
    #[serde(default, deserialize_with = "lenient::bool")]
    pub admin: bool,
    #[serde(default, rename = "system name", deserialize_with = "lenient::string")]
    pub system_name: String,
    #[serde(default, deserialize_with = "lenient::string")]
    pub version: String,
    #[serde(default, deserialize_with = "lenient::string_vec")]
    pub profiles: Vec<String>,
    #[serde(default, deserialize_with = "lenient::string_vec")]
    pub schedules: Vec<String>,
    /// Server time zone, minutes east of UTC (e.g. -300 for EST).
    #[serde(default, deserialize_with = "lenient::opt_f64")]
    pub tzone: Option<f64>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    pub changeprofile: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    pub ptz: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    pub audio: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    pub clips: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    pub clipcreate: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    pub lan: Option<bool>,
    /// e.g. `"VBR q=..% ...kbps gop=.. 720p *"`, one per server-side streaming profile.
    #[serde(default)]
    pub streams: Vec<Value>,
    // Servers may send several of these at once; `sounds()` picks one like UI3 does.
    #[serde(default, deserialize_with = "lenient::string_vec")]
    pub www_sounds: Vec<String>,
    #[serde(default, rename = "www-sounds", deserialize_with = "lenient::string_vec")]
    pub www_sounds_dash: Vec<String>,
    #[serde(default, deserialize_with = "lenient::string_vec")]
    pub sounds: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Permissions {
    pub admin: bool,
    pub change_profile: bool,
    pub ptz: bool,
    pub ptz_preset_set: bool,
    pub audio: bool,
    pub clips: bool,
    pub clip_create: bool,
}

impl LoginData {
    pub fn permissions(&self) -> Permissions {
        let a = self.admin;
        Permissions {
            admin: a,
            change_profile: self.changeprofile.unwrap_or(a),
            ptz: self.ptz.unwrap_or(true),
            ptz_preset_set: a,
            audio: self.audio.unwrap_or(true),
            clips: self.clips.unwrap_or(true),
            clip_create: self.clipcreate.unwrap_or(a),
        }
    }
    /// Milliseconds to add to a UTC timestamp to get the server's wall-clock time.
    /// (UI3: serverTimeZoneOffsetMs = tzone * -60000; display = utc - serverTimeZoneOffsetMs.)
    pub fn tz_offset_ms(&self) -> i64 {
        self.tzone.map(|t| (t * 60000.0) as i64).unwrap_or(0)
    }
    /// Sound files on the server (UI3: `www_sounds`, else `www-sounds`, else `sounds`).
    pub fn sounds(&self) -> &[String] {
        [&self.www_sounds, &self.www_sounds_dash, &self.sounds].into_iter().find(|v| !v.is_empty()).map(|v| v.as_slice()).unwrap_or(&[])
    }
    pub fn server_streams(&self) -> Vec<ServerStream> {
        let mut v: Vec<ServerStream> = self.streams.iter().filter_map(|s| s.as_str()).map(ServerStream::parse).collect();
        if v.is_empty() {
            v = vec![ServerStream::default(); 3];
        }
        v
    }
    /// Blue Iris version as comparable tuple (e.g. "5.9.9.33" -> [5,9,9,33]).
    pub fn version_parts(&self) -> Vec<u32> {
        parse_version(&self.version)
    }
}

pub fn parse_version(v: &str) -> Vec<u32> {
    v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect()
}

/// `MakeStreamObject` (ui3.js:17482).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerStream {
    pub text: String,
    pub control: Option<String>,
    pub q6: Option<i32>,
    pub kbps: Option<i32>,
    pub gop: Option<i32>,
    pub res: Option<i32>,
    pub overrides: bool,
}

impl ServerStream {
    pub fn parse(s: &str) -> Self {
        let lower = s.to_ascii_lowercase();
        let num_after = |pat: &str| -> Option<i32> {
            let i = lower.find(pat)? + pat.len();
            let digits: String = lower[i..].chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        };
        let num_before = |suffix: &str, need_space: bool| -> Option<i32> {
            let mut search = 0;
            while let Some(rel) = lower[search..].find(suffix) {
                let end = search + rel;
                let start = lower[..end].rfind(|c: char| !c.is_ascii_digit()).map(|i| i + 1).unwrap_or(0);
                if start < end && (!need_space || (start > 0 && lower.as_bytes()[start - 1] == b' ')) {
                    return lower[start..end].parse().ok();
                }
                search = end + suffix.len();
            }
            None
        };
        ServerStream {
            text: s.to_string(),
            control: if lower.starts_with("vbr") {
                Some("VBR".into())
            } else if lower.starts_with("cbr") {
                Some("CBR".into())
            } else {
                None
            },
            q6: num_after("q=").filter(|_| lower.contains('%')),
            kbps: num_before("kbps", false),
            gop: num_after("gop="),
            res: num_before("p", true),
            overrides: s.contains('*'),
        }
    }
}

/// One entry of `camlist` (cameras, groups and cycles share this shape).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct CameraInfo {
    #[serde(rename = "optionValue", deserialize_with = "lenient::string")]
    pub id: String,
    #[serde(rename = "optionDisplay", deserialize_with = "lenient::string")]
    pub name: String,
    /// Member camera ids if this is a group.
    pub group: Option<Vec<String>>,
    /// Member rectangles `[l,t,r,b]` within the group image.
    pub rects: Option<Vec<[i32; 4]>>,
    #[serde(rename = "isEnabled", deserialize_with = "lenient::bool")]
    pub is_enabled: bool,
    #[serde(deserialize_with = "lenient::opt_bool")]
    pub webcast: Option<bool>,
    #[serde(deserialize_with = "lenient::bool")]
    pub hidden: bool,
    #[serde(deserialize_with = "lenient::bool")]
    pub ptz: bool,
    #[serde(deserialize_with = "lenient::opt_bool")]
    pub ptzdirect: Option<bool>,
    #[serde(deserialize_with = "lenient::bool")]
    pub audio: bool,
    #[serde(deserialize_with = "lenient::i64")]
    pub width: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub height: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub width2: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub height2: i64,
    #[serde(rename = "FPS", deserialize_with = "lenient::f64")]
    pub fps: f64,
    #[serde(rename = "BPS", deserialize_with = "lenient::f64")]
    pub bps: f64,
    #[serde(rename = "isOnline", deserialize_with = "lenient::bool")]
    pub is_online: bool,
    #[serde(rename = "isRecording", deserialize_with = "lenient::bool")]
    pub is_recording: bool,
    #[serde(rename = "isAlerting", deserialize_with = "lenient::bool")]
    pub is_alerting: bool,
    #[serde(rename = "isTriggered", deserialize_with = "lenient::bool")]
    pub is_triggered: bool,
    #[serde(rename = "isMotion", deserialize_with = "lenient::bool")]
    pub is_motion: bool,
    #[serde(rename = "isPaused", deserialize_with = "lenient::bool")]
    pub is_paused: bool,
    #[serde(rename = "isNoSignal", deserialize_with = "lenient::bool")]
    pub is_no_signal: bool,
    #[serde(rename = "isYellow", deserialize_with = "lenient::bool")]
    pub is_yellow: bool,
    #[serde(rename = "isManRec", deserialize_with = "lenient::bool")]
    pub is_man_rec: bool,
    #[serde(deserialize_with = "lenient::i64")]
    pub newalerts: i64,
    #[serde(rename = "nAlerts", deserialize_with = "lenient::i64")]
    pub n_alerts: i64,
    #[serde(rename = "nClips", deserialize_with = "lenient::i64")]
    pub n_clips: i64,
    #[serde(rename = "nTriggers", deserialize_with = "lenient::i64")]
    pub n_triggers: i64,
    #[serde(rename = "nNoSignal", deserialize_with = "lenient::i64")]
    pub n_no_signal: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub color: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub pause: i64,
    #[serde(deserialize_with = "lenient::string")]
    pub ip: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl CameraInfo {
    pub fn is_group(&self) -> bool {
        self.group.as_ref().is_some_and(|g| !g.is_empty())
    }
    /// `CameraIsCycle`: cycle cameras have ids starting with "@" (ui3 convention).
    pub fn is_cycle(&self) -> bool {
        self.id.starts_with('@') || self.extra.get("isCycle").is_some_and(lenient::value_to_bool)
    }
    pub fn webcast(&self) -> bool {
        self.webcast.unwrap_or(true)
    }
    pub fn ptz_direct(&self) -> bool {
        self.ptzdirect.unwrap_or(self.ptz)
    }
    pub fn color_rgb(&self) -> [u8; 3] {
        bi_color_to_rgb(self.color)
    }
}

/// Blue Iris colors are Win32 COLORREF (0x00BBGGRR).
pub fn bi_color_to_rgb(c: i64) -> [u8; 3] {
    let c = c as u32;
    [(c & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, ((c >> 16) & 0xFF) as u8]
}

/// `status` data (ui3.js:16289-16403).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct StatusData {
    #[serde(deserialize_with = "lenient::i64")]
    pub signal: i64,
    #[serde(deserialize_with = "lenient::string")]
    pub cpu: String,
    #[serde(deserialize_with = "lenient::string")]
    pub ram: String,
    #[serde(deserialize_with = "lenient::string")]
    pub mem: String,
    #[serde(deserialize_with = "lenient::string")]
    pub memphys: String,
    #[serde(deserialize_with = "lenient::string")]
    pub memload: String,
    pub disks: Vec<DiskInfo>,
    #[serde(deserialize_with = "lenient::string")]
    pub clips: String,
    #[serde(deserialize_with = "lenient::string")]
    pub uptime: String,
    #[serde(deserialize_with = "lenient::i64")]
    pub profile: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub lock: i64,
    #[serde(deserialize_with = "lenient::string")]
    pub schedule: String,
    #[serde(deserialize_with = "lenient::i64")]
    pub warnings: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub alerts: i64,
    #[serde(deserialize_with = "lenient::opt_f64")]
    pub tzone: Option<f64>,
    #[serde(deserialize_with = "lenient::opt_f64")]
    pub time: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct DiskInfo {
    #[serde(deserialize_with = "lenient::string")]
    pub disk: String,
    /// MiB
    #[serde(deserialize_with = "lenient::f64")]
    pub allocated: f64,
    #[serde(deserialize_with = "lenient::f64")]
    pub used: f64,
    #[serde(deserialize_with = "lenient::f64")]
    pub free: f64,
    #[serde(deserialize_with = "lenient::f64")]
    pub total: f64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One `cliplist`/`alertlist` item (see `ClipData`, ui3.js:13461).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct ClipItem {
    #[serde(deserialize_with = "lenient::string")]
    pub camera: String,
    /// `@123.bvr` for clips; for alerts this is the alert path.
    #[serde(deserialize_with = "lenient::string")]
    pub path: String,
    /// Present only on alerts: the underlying clip path.
    pub clip: Option<String>,
    /// ms into the clip (alerts).
    #[serde(deserialize_with = "lenient::i64")]
    pub offset: i64,
    /// unix seconds
    #[serde(deserialize_with = "lenient::i64")]
    pub date: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub color: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub flags: i64,
    #[serde(deserialize_with = "lenient::opt_f64")]
    pub msec: Option<f64>,
    #[serde(deserialize_with = "lenient::string")]
    pub filesize: String,
    #[serde(deserialize_with = "lenient::string")]
    pub filetype: String,
    #[serde(deserialize_with = "lenient::string")]
    pub res: String,
    #[serde(deserialize_with = "lenient::string")]
    pub memo: String,
    #[serde(deserialize_with = "lenient::string")]
    pub plate: String,
    #[serde(deserialize_with = "lenient::i64")]
    pub newalerttime: i64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// BIDBFLAG bits (ui3.js:40821).
pub mod flags {
    pub const AUDIO: i64 = 1;
    pub const FLAGGED: i64 = 1 << 1;
    pub const PROTECTED: i64 = 1 << 2;
    pub const CORRUPT: i64 = 1 << 3;
    pub const DELETE: i64 = 1 << 4;
    pub const DELETED: i64 = 1 << 5;
    pub const ARCHIVE: i64 = 1 << 6;
    pub const ARCHIVED: i64 = 1 << 7;
    pub const RECORDING: i64 = 1 << 8;
    pub const EXPORT: i64 = 1 << 9;
    pub const EXPORTED: i64 = 1 << 10;
    pub const SPECIALOBJ: i64 = 1 << 11;
    pub const AI_CONFIRMED_X: i64 = 1 << 14;
    pub const AI_OCCUPIED_X: i64 = 1 << 15;
    pub const ALERT_OFFSETTIME: i64 = 1 << 16;
    pub const ALERT_MOTION: i64 = 1 << 17;
    pub const ALERT_ONVIF: i64 = 1 << 18;
    pub const ALERT_AUDIO: i64 = 1 << 19;
    pub const ALERT_EXTERNAL: i64 = 1 << 20;
    pub const ALERT_DIO: i64 = 1 << 21;
    pub const ALERT_GROUP: i64 = 1 << 22;
    pub const ALERT_CANCELLED: i64 = 1 << 23;
    pub const ALERT_NOSIGNAL: i64 = 1 << 24;
    pub const ALERT_HIDDEN: i64 = 1 << 24;
    pub const AI_PERSON: i64 = 1 << 26;
    pub const AI_VEHICLE: i64 = 1 << 27;
    pub const AI_CONFIRMED: i64 = 1 << 28;
    pub const AI_OCCUPIED: i64 = 1 << 29;
    pub const ALERT_AI: i64 = 1 << 30;
    pub const AI_WILDLIFE: i64 = 1 << 31;
    pub const ALERT_TRIGGERBITS: i64 =
        ALERT_MOTION | ALERT_NOSIGNAL | ALERT_AUDIO | ALERT_EXTERNAL | ALERT_ONVIF | ALERT_DIO | ALERT_GROUP | ALERT_CANCELLED;

    pub fn has(v: i64, f: i64) -> bool {
        f != 0 && v & f == f
    }
}

impl ClipItem {
    pub fn is_clip(&self) -> bool {
        self.clip.is_none()
    }
    pub fn is_snapshot(&self) -> bool {
        match &self.clip {
            Some(c) => c.starts_with("@-1."),
            None => self.filesize.trim().eq_ignore_ascii_case("snapshot"),
        }
    }
    /// Path used for loading the video stream.
    pub fn video_path(&self) -> &str {
        match &self.clip {
            Some(c) if !self.is_snapshot() => c,
            _ => &self.path,
        }
    }
    /// Record id from `@123.bvr` -> `123`.
    pub fn rec_id(&self) -> &str {
        rec_id_from_path(&self.path)
    }
    pub fn has_flag(&self, f: i64) -> bool {
        flags::has(self.flags, f)
    }
    /// Rough length parsed from `filesize` (e.g. `"10sec (3.09M)"`, `"1:02:03 (1.2G)"`).
    pub fn rough_length_ms(&self) -> i64 {
        parse_rough_length_ms(&self.filesize)
    }
    /// `ClipData.msec` logic.
    pub fn duration_ms(&self) -> i64 {
        if self.is_snapshot() {
            2000
        } else if self.is_clip() {
            self.msec.map(|m| m as i64).unwrap_or_else(|| self.rough_length_ms())
        } else {
            self.offset + self.rough_length_ms()
        }
    }
}

pub fn rec_id_from_path(path: &str) -> &str {
    let p = path.strip_prefix('@').unwrap_or(path);
    p.split('.').next().unwrap_or(p)
}

/// Parses durations like "10sec", "5min", "2:15", "1:02:03", "1h 2m" at the start of the filesize string.
pub fn parse_rough_length_ms(s: &str) -> i64 {
    let head = s.split('(').next().unwrap_or("").trim();
    if head.is_empty() || head.eq_ignore_ascii_case("snapshot") {
        return 0;
    }
    if head.contains(':') {
        let parts: Vec<i64> = head.split(':').map(|p| p.trim().parse().unwrap_or(0)).collect();
        let secs = parts.iter().fold(0i64, |acc, v| acc * 60 + v);
        return secs * 1000;
    }
    let mut total = 0.0f64;
    let mut num = String::new();
    let mut unit = String::new();
    let flush = |num: &mut String, unit: &mut String, total: &mut f64| {
        if let Ok(n) = num.parse::<f64>() {
            let u = unit.to_ascii_lowercase();
            let mult = if u.starts_with('h') {
                3_600_000.0
            } else if u.starts_with("ms") {
                1.0
            } else if u.starts_with('m') {
                60_000.0
            } else {
                1000.0
            };
            *total += n * mult;
        }
        num.clear();
        unit.clear();
    };
    for ch in head.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            if !unit.is_empty() {
                flush(&mut num, &mut unit, &mut total);
            }
            num.push(ch);
        } else if ch.is_alphabetic() {
            unit.push(ch);
        } else if !num.is_empty() && !unit.is_empty() {
            flush(&mut num, &mut unit, &mut total);
        }
    }
    flush(&mut num, &mut unit, &mut total);
    total as i64
}

/// `timeline` data (ui3.js:9618).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct TimelineData {
    pub colors: Vec<Value>,
    pub alerts: Vec<TimelineAlert>,
    pub clips: Vec<TimelineClip>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct TimelineAlert {
    #[serde(deserialize_with = "lenient::i64")]
    pub x1: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub x2: i64,
    #[serde(rename = "type", deserialize_with = "lenient::i64")]
    pub kind: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub tracks: i64,
    /// Alert record path (e.g. "@123.bvr"), used for timeline thumbnails.
    #[serde(deserialize_with = "lenient::string")]
    pub record: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct TimelineClip {
    #[serde(deserialize_with = "lenient::i64")]
    pub x1: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub x2: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub track: i64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ptz` info response for a camera (ui3.js:7446).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct PtzInfo {
    #[serde(deserialize_with = "lenient::i64")]
    pub brightness: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub contrast: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub irmode: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub powermode: i64,
    #[serde(deserialize_with = "lenient::i64")]
    pub presetnum: i64,
    /// Either strings (index+1 = preset number) or `{num, desc}` objects.
    pub presets: Vec<Value>,
    #[serde(deserialize_with = "lenient::i64")]
    pub talksamplerate: i64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl PtzInfo {
    /// Preset number -> description.
    pub fn preset_map(&self) -> std::collections::BTreeMap<i64, String> {
        let mut m = std::collections::BTreeMap::new();
        for (i, p) in self.presets.iter().enumerate() {
            match p {
                Value::String(s) => {
                    m.insert(i as i64 + 1, s.clone());
                }
                Value::Object(o) => {
                    let n = o.get("num").and_then(crate::lenient::value_to_f64).unwrap_or((i + 1) as f64) as i64;
                    let d = o.get("desc").and_then(|d| d.as_str()).unwrap_or("").to_string();
                    m.insert(n, d);
                }
                _ => {}
            }
        }
        m
    }
}

/// PTZ button codes (ui3.js `PtzButtons`).
pub mod ptz {
    pub const LEFT: i32 = 0;
    pub const RIGHT: i32 = 1;
    pub const UP: i32 = 2;
    pub const DOWN: i32 = 3;
    pub const HOME: i32 = 4;
    pub const ZOOM_IN: i32 = 5;
    pub const ZOOM_OUT: i32 = 6;
    pub const FOCUS_NEAR: i32 = -1;
    pub const FOCUS_FAR: i32 = -2;
    pub const UP_LEFT: i32 = 59;
    pub const UP_RIGHT: i32 = 60;
    pub const DOWN_LEFT: i32 = 61;
    pub const DOWN_RIGHT: i32 = 62;
    pub const STOP: i32 = 64;
    pub const IR_OFF: i32 = 34;
    pub const IR_ON: i32 = 35;
    pub const IR_AUTO: i32 = 36;
    /// + 0..=15
    pub const BRIGHTNESS_BASE: i32 = 11;
    /// + 0..=6
    pub const CONTRAST_BASE: i32 = 27;
    pub const PRESET_BASE: i32 = 100;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_stream_strings() {
        let s = ServerStream::parse("VBR q=70% 2048kbps gop=1000 720p *");
        assert_eq!(s.control.as_deref(), Some("VBR"));
        assert_eq!(s.q6, Some(70));
        assert_eq!(s.kbps, Some(2048));
        assert_eq!(s.gop, Some(1000));
        assert_eq!(s.res, Some(720));
        assert!(s.overrides);
    }

    #[test]
    fn rough_lengths() {
        assert_eq!(parse_rough_length_ms("10sec (3.09M)"), 10_000);
        assert_eq!(parse_rough_length_ms("2:15 (1M)"), 135_000);
        assert_eq!(parse_rough_length_ms("1:02:03 (1G)"), 3_723_000);
        assert_eq!(parse_rough_length_ms("5min (1M)"), 300_000);
        assert_eq!(parse_rough_length_ms("Snapshot"), 0);
    }

    #[test]
    fn lenient_camera_parse() {
        let v = serde_json::json!({
            "optionValue": "Front", "optionDisplay": "Front Door", "isEnabled": 1, "ptz": "true",
            "width": "1920", "height": 1080, "FPS": 15.2, "color": 255, "unknownField": [1,2]
        });
        let c: CameraInfo = serde_json::from_value(v).unwrap();
        assert!(c.is_enabled && c.ptz && !c.is_group());
        assert_eq!((c.width, c.height), (1920, 1080));
        assert_eq!(c.color_rgb(), [255, 0, 0]);
        assert!(c.extra.contains_key("unknownField"));
    }

    #[test]
    fn clip_paths() {
        let alert: ClipItem = serde_json::from_value(serde_json::json!({
            "camera": "cam1", "path": "@555.bvr", "clip": "@123.bvr", "offset": 4000, "filesize": "10sec (1M)"
        }))
        .unwrap();
        assert!(!alert.is_clip());
        assert_eq!(alert.video_path(), "@123.bvr");
        assert_eq!(alert.rec_id(), "555");
        assert_eq!(alert.duration_ms(), 14_000);
    }

    #[test]
    fn login_with_both_sound_keys() {
        let v = serde_json::json!({
            "admin": true, "system name": "NVR", "version": "5.9.9.1",
            "profiles": ["Inactive", "Away", null], "schedules": "Default",
            "www_sounds": ["alarm.wav"], "sounds": ["old.wav", 3], "tzone": "-300"
        });
        let l: LoginData = serde_json::from_value(v).unwrap();
        assert_eq!(l.sounds(), ["alarm.wav"]);
        assert_eq!(l.sounds, ["old.wav", "3"]);
        assert_eq!(l.profiles, ["Inactive", "Away"]);
        assert_eq!(l.schedules, ["Default"]);
        assert_eq!(l.tz_offset_ms(), -300 * 60000);
    }
}
