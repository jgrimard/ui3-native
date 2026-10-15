//! Settings store. Mirrors UI3's `settings` object (string-valued localStorage keys with defaults from
//! `defaultSettings`, ui3.js:942) so behaviour and the settings panel match UI3 one-to-one.
//! Persisted as JSON in %APPDATA%\UI3Native\settings.json.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub use crate::settings_defs::DEFS;

#[derive(Debug, Clone, Copy)]
pub enum Dv {
    Str(&'static str),
    Num(f64),
}

#[derive(Debug, Clone, Copy)]
pub enum Input {
    Hidden,
    Checkbox,
    /// Static options, plus the name of UI3's dynamic option provider (if any).
    Select(&'static [&'static str], Option<&'static str>),
    Number { min: f64, max: f64, step: f64, unit: &'static str },
    Range { min: f64, max: f64, step: f64, unit: &'static str },
    Color,
    Text,
    Password,
    Comment(&'static str),
    Hotkey,
}

#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    pub key: &'static str,
    pub default: Dv,
    pub input: Input,
    pub label: &'static str,
    pub desc: &'static str,
    pub hint: &'static str,
    pub category: &'static str,
}

pub const CATEGORIES: &[&str] = &[
    "General Settings",
    "Video Player",
    "Video Player (Advanced)",
    "Timeline",
    "UI Status Sounds",
    "Top Bar",
    "Side Bar",
    "Status Area",
    "Clips / Alerts",
    "Clip / Alert Icons",
    "Event-Triggered Icons",
    "Event-Triggered Sounds",
    "PTZ",
    "Hotkeys",
    "UI3 Camera Labels",
    "Digital Zoom",
    "MQTT Remote Control",
    "Extra",
];

/// A saved Blue Iris server (the password lives in Windows Credential Manager).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ServerEntry {
    pub name: String,
    pub url: String,
    pub user: String,
    #[serde(default)]
    pub auto_login: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub values: BTreeMap<String, Value>,
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
    #[serde(default)]
    pub last_server: Option<usize>,
    #[serde(default)]
    pub streaming_profiles: Vec<bi_proto::profile::StreamingProfile>,
    /// Native-only options.
    #[serde(default)]
    pub native: NativeOptions,
    #[serde(skip)]
    dirty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeOptions {
    /// "Auto" | "D3D11VA" | "Software"
    pub hw_decode: String,
    /// Open one direct stream per camera in groups instead of the server-composited group image.
    pub client_side_grid: bool,
    /// Sub-stream (decode=-1) for client-side grid tiles.
    pub grid_use_substream: bool,
    /// Cache this many scrub JPEGs per clip for instant re-scrubbing.
    pub scrub_cache_frames: usize,
    pub show_stats_overlay: bool,
    /// Render hardware-decoded frames straight from the decoder's GPU textures (no copy through system memory).
    pub zero_copy: bool,
}

impl Default for NativeOptions {
    fn default() -> Self {
        NativeOptions { hw_decode: "Auto".into(), client_side_grid: false, grid_use_substream: true, scrub_cache_frames: 400, show_stats_overlay: false, zero_copy: true }
    }
}

pub fn def(key: &str) -> Option<&'static SettingDef> {
    DEFS.iter().find(|d| d.key == key)
}

pub fn config_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "UI3Native").map(|d| d.config_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

impl Settings {
    pub fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    pub fn load() -> Self {
        let mut s: Settings = std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        if s.streaming_profiles.is_empty() {
            s.streaming_profiles = default_profiles();
        }
        s
    }

    pub fn save(&mut self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(self) {
            Ok(t) => {
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, t).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
                self.dirty = false;
            }
            Err(e) => tracing::error!("saving settings: {e}"),
        }
    }

    pub fn save_if_dirty(&mut self) {
        if self.dirty {
            self.save();
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn default_value(key: &str) -> Value {
        match def(key).map(|d| d.default) {
            Some(Dv::Str(s)) => Value::String(s.to_string()),
            Some(Dv::Num(n)) => serde_json::json!(n),
            None => Value::Null,
        }
    }

    pub fn raw(&self, key: &str) -> Value {
        self.values.get(key).cloned().unwrap_or_else(|| Self::default_value(key))
    }

    pub fn str(&self, key: &str) -> String {
        match self.raw(key) {
            Value::String(s) => s,
            Value::Null => String::new(),
            v => v.to_string(),
        }
    }

    /// UI3 stores booleans as "1"/"0".
    pub fn bool(&self, key: &str) -> bool {
        debug_assert!(Self::is_bool_key(key), "Settings::bool called on non-boolean setting {key}");
        bi_proto::lenient::value_to_bool(&self.raw(key))
    }

    /// True for settings that hold "0"/"1" (checkboxes and hidden flags); anything else must not be read via `bool()`.
    fn is_bool_key(key: &str) -> bool {
        match DEFS.iter().find(|d| d.key == key) {
            Some(d) => matches!(d.input, Input::Checkbox) || matches!(d.default, Dv::Str("0" | "1")),
            None => true,
        }
    }

    /// Deleting recordings needs both the setting and an administrator session (the server answers "Access denied"
    /// otherwise; UI3 would offer to upgrade the session, which this app does not support).
    pub fn can_delete_clips(&self, admin: bool) -> bool {
        admin && self.bool("ui3_allow_clip_deletion")
    }

    /// `ui3_askForDelete` is a select ("All" | "Multiple" | "None"): whether deleting `n` items needs confirmation.
    pub fn delete_needs_confirm(&self, n: usize) -> bool {
        match self.str("ui3_askForDelete").as_str() {
            "All" => true,
            "Multiple" => n > 1,
            _ => false,
        }
    }

    pub fn f64(&self, key: &str) -> f64 {
        bi_proto::lenient::value_to_f64(&self.raw(key)).unwrap_or(0.0)
    }

    pub fn set(&mut self, key: &str, v: impl Into<Value>) {
        let v = v.into();
        if self.values.get(key) != Some(&v) {
            self.values.insert(key.to_string(), v);
            self.dirty = true;
        }
    }

    pub fn set_bool(&mut self, key: &str, b: bool) {
        self.set(key, if b { "1" } else { "0" });
    }

    pub fn reset(&mut self, key: &str) {
        if self.values.remove(key).is_some() {
            self.dirty = true;
        }
    }

    pub fn hotkey(&self, key: &str) -> Option<Hotkey> {
        Hotkey::parse(&self.str(key))
    }

    fn selected_profile(&self) -> bi_proto::profile::StreamingProfile {
        let name = self.str("ui3_streamingQuality");
        self.streaming_profiles.iter().find(|p| p.name == name).cloned().or_else(|| self.streaming_profiles.first().cloned()).unwrap_or_else(bi_proto::profile::native_passthrough_profile)
    }

    pub fn current_profile(&self) -> bi_proto::profile::StreamingProfile {
        let mut p = self.selected_profile();
        if p.q6 < 0 {
            p.q6 = (self.f64("ui3_streamingQualityDefault") as i32).clamp(1, 100);
        }
        p
    }

    /// Profile for recorded playback (clips, timeline). Blue Iris always transcodes recordings, so direct-to-wire
    /// does not apply; a direct-to-wire profile keeps its size but leaves the quality to Blue Iris instead of the
    /// Default Video Quality, which at high values makes Blue Iris drop reference frames at full resolution.
    pub fn playback_profile(&self) -> bi_proto::profile::StreamingProfile {
        let p = self.selected_profile();
        if p.direct_to_wire == 2 { p } else { self.current_profile() }
    }
}

/// Default profile list: UI3's defaults with the native passthrough profile first.
pub fn default_profiles() -> Vec<bi_proto::profile::StreamingProfile> {
    let mut v = vec![bi_proto::profile::native_passthrough_profile()];
    v.extend(bi_proto::profile::default_profiles(false));
    v
}

/// UI3 hotkey string "ctrl|alt|shift|keyCode" (JavaScript keyCode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: egui::Key,
}

impl Hotkey {
    pub fn parse(s: &str) -> Option<Hotkey> {
        let p: Vec<&str> = s.split('|').collect();
        if p.len() != 4 {
            return None;
        }
        let code: u32 = p[3].trim().parse().ok()?;
        Some(Hotkey { ctrl: p[0] == "1", alt: p[1] == "1", shift: p[2] == "1", key: keycode_to_key(code)? })
    }

    pub fn to_ui3_string(&self) -> String {
        format!("{}|{}|{}|{}", self.ctrl as u8, self.alt as u8, self.shift as u8, key_to_keycode(self.key).unwrap_or(0))
    }

    pub fn matches(&self, key: egui::Key, m: egui::Modifiers) -> bool {
        self.key == key && self.ctrl == m.ctrl && self.alt == m.alt && self.shift == m.shift
    }

    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl + ");
        }
        if self.alt {
            s.push_str("Alt + ");
        }
        if self.shift {
            s.push_str("Shift + ");
        }
        s.push_str(self.key.name());
        s
    }
}

const KEYMAP: &[(u32, egui::Key)] = {
    use egui::Key::*;
    &[
        (8, Backspace),
        (9, Tab),
        (13, Enter),
        (27, Escape),
        (32, Space),
        (33, PageUp),
        (34, PageDown),
        (35, End),
        (36, Home),
        (37, ArrowLeft),
        (38, ArrowUp),
        (39, ArrowRight),
        (40, ArrowDown),
        (45, Insert),
        (46, Delete),
        (48, Num0),
        (49, Num1),
        (50, Num2),
        (51, Num3),
        (52, Num4),
        (53, Num5),
        (54, Num6),
        (55, Num7),
        (56, Num8),
        (57, Num9),
        (65, A),
        (66, B),
        (67, C),
        (68, D),
        (69, E),
        (70, F),
        (71, G),
        (72, H),
        (73, I),
        (74, J),
        (75, K),
        (76, L),
        (77, M),
        (78, N),
        (79, O),
        (80, P),
        (81, Q),
        (82, R),
        (83, S),
        (84, T),
        (85, U),
        (86, V),
        (87, W),
        (88, X),
        (89, Y),
        (90, Z),
        (96, Num0),
        (97, Num1),
        (98, Num2),
        (99, Num3),
        (100, Num4),
        (101, Num5),
        (102, Num6),
        (103, Num7),
        (104, Num8),
        (105, Num9),
        (107, Plus),
        (109, Minus),
        (112, F1),
        (113, F2),
        (114, F3),
        (115, F4),
        (116, F5),
        (117, F6),
        (118, F7),
        (119, F8),
        (120, F9),
        (121, F10),
        (122, F11),
        (123, F12),
        (186, Semicolon),
        (187, Equals),
        (188, Comma),
        (189, Minus),
        (190, Period),
        (191, Slash),
        (192, Backtick),
        (219, OpenBracket),
        (220, Backslash),
        (221, CloseBracket),
        (222, Quote),
    ]
};

pub fn keycode_to_key(code: u32) -> Option<egui::Key> {
    KEYMAP.iter().find(|(c, _)| *c == code).map(|(_, k)| *k)
}

pub fn key_to_keycode(key: egui::Key) -> Option<u32> {
    KEYMAP.iter().find(|(_, k)| *k == key).map(|(c, _)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkeys_parse() {
        let h = Hotkey::parse("1|0|0|192").unwrap();
        assert!(h.ctrl && !h.alt && !h.shift);
        assert_eq!(h.key, egui::Key::Backtick);
        assert_eq!(h.to_ui3_string(), "1|0|0|192");
        assert!(Hotkey::parse("").is_none());
    }

    #[test]
    fn default_profile_is_native_passthrough() {
        let s = Settings::default();
        let p = s.current_profile();
        assert_eq!(p.name, "Native (direct H.264/H.265)^");
        assert!(p.url_args((1920, 1080), bi_proto::profile::SupportedCodecs::default(), None).contains("&d2w=1"));
    }

    #[test]
    fn native_playback_leaves_quality_to_blue_iris() {
        let mut s = Settings::default();
        s.set("ui3_streamingQualityDefault", 80.0);
        let codecs = bi_proto::profile::SupportedCodecs::default();
        let live = s.current_profile().url_args((3072, 1728), codecs, None);
        let playback = s.playback_profile().url_args((3072, 1728), codecs, None);
        assert!(live.contains("&q=80"), "{live}");
        assert!(!playback.contains("&q="), "{playback}");
        assert!(playback.contains("&w=3072&h=1728"), "{playback}");
    }

    #[test]
    fn defaults_resolve() {
        let s = Settings::default();
        assert_eq!(s.str("ui3_hotkey_playpause"), "0|0|0|32");
        assert_eq!(s.f64("ui3_skipAmount"), 10.0);
        assert_eq!(s.str("ui3_cameraLabels_position"), "Top");
        // Every hotkey default parses (or is intentionally unbound).
        for d in DEFS.iter().filter(|d| matches!(d.input, Input::Hotkey)) {
            let v = s.str(d.key);
            assert!(v.is_empty() || Hotkey::parse(&v).is_some(), "{} = {v}", d.key);
        }
    }
}
