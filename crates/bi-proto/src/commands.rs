//! Typed wrappers for every `/json` command UI3 issues.

use crate::api::*;
use crate::client::BiClient;
use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

/// Clip list views (dbView dropdown, ui3.js:6360).
pub const DB_VIEWS: &[(&str, &str)] = &[
    ("all", "All clips"),
    ("alerts", "Alerts"),
    ("new", "New"),
    ("stored", "Stored"),
    ("flagged", "Flagged"),
    ("new.clipboard", "Clipboard"),
    ("cancelled", "Cancelled alerts"),
    ("confirmed", "Confirmed alerts"),
    ("people", "Person alerts"),
    ("vehicles", "Vehicle alerts"),
    ("wildlife", "Wildlife alerts"),
    ("zonea", "Zone A alerts"),
    ("zoneb", "Zone B alerts"),
    ("zonec", "Zone C alerts"),
    ("zoned", "Zone D alerts"),
    ("zonee", "Zone E alerts"),
    ("zonef", "Zone F alerts"),
    ("zoneg", "Zone G alerts"),
    ("zoneh", "Hotspot alerts"),
    ("dio", "DIO alerts"),
    ("onvif", "ONVIF alerts"),
    ("audio", "Audio alerts"),
    ("external", "External alerts"),
    ("memos", "Memos"),
    ("protected", "Protected"),
    ("archive", "FTP backup queue"),
    ("export", "Convert/Export queue"),
];

/// `DbViewIsAlerts` (ui3.js:15567). Note UI3 does not list "wildlife" here; kept identical for parity.
pub fn db_view_is_alerts(v: &str) -> bool {
    matches!(v, "alerts" | "cancelled" | "confirmed" | "people" | "vehicles" | "dio" | "onvif" | "audio" | "external")
        || (v.len() == 5 && v.starts_with("zone") && ('a'..='h').contains(&v.chars().last().unwrap_or(' ')))
}

#[derive(Debug, Clone, Default)]
pub struct ClipQuery {
    /// Camera or group id ("index" = all cameras).
    pub camera: String,
    /// unix seconds, 0 = unbounded
    pub start: i64,
    pub end: i64,
    pub view: String,
    pub search: String,
    /// Calendar mode: request per-day tile counts.
    pub tiles: bool,
}

/// Camera config toggles for `camconfig` (ui3.js:28684).
#[derive(Debug, Clone)]
pub enum CamConfig {
    ManualRecord(bool),
    Reset,
    Reboot,
    Enable(bool),
    /// -1 = indefinitely, 0 = resume, else seconds (UI3 pause dialog values).
    Pause(i64),
    Motion(bool),
    Schedule(bool),
    PtzCycle(bool),
    PtzEvents(bool),
    Alerts(i64),
    Record(i64),
    Push(bool),
    Output(bool),
    SetMotion(Value),
    Raw(Map<String, Value>),
}

#[derive(Debug, Clone, Default)]
pub struct ExportRequest {
    /// Clip path (`@123.bvr`) or camera id for a timeline range.
    pub path: String,
    pub start_ms: Option<i64>,
    pub msec: Option<i64>,
    /// 0 AVI, 1 MP4, 2 WMV, 3 BVR (`ClipExportPanel` format options).
    pub format: Option<i64>,
    pub profile: Option<i64>,
    /// 0..=2
    pub substream: Option<i64>,
    pub audio: Option<bool>,
    /// tenths of a dB
    pub gain: Option<i64>,
    pub reencode: Option<bool>,
    pub overlay: Option<bool>,
    pub motion: Option<bool>,
    /// `"mult@fps"`
    pub timelapse: Option<String>,
}

impl BiClient {
    pub async fn status(&self, changes: Option<Map<String, Value>>) -> Result<StatusData> {
        let mut args = json!({"cmd": "status"});
        if let Some(c) = changes {
            args.as_object_mut().unwrap().extend(c);
        }
        let data = self.json_ok(args).await?;
        serde_json::from_value(data).context("parsing status")
    }

    pub async fn set_profile(&self, profile: i64, lock: Option<i64>) -> Result<StatusData> {
        let mut m = Map::new();
        m.insert("profile".into(), profile.into());
        if let Some(l) = lock {
            m.insert("lock".into(), l.into());
        }
        self.status(Some(m)).await
    }

    /// Traffic signal: 0 red, 1 green, 2 yellow.
    pub async fn set_signal(&self, signal: i64) -> Result<StatusData> {
        let mut m = Map::new();
        m.insert("signal".into(), signal.into());
        self.status(Some(m)).await
    }

    pub async fn set_schedule(&self, schedule: &str) -> Result<StatusData> {
        let mut m = Map::new();
        m.insert("schedule".into(), schedule.into());
        self.status(Some(m)).await
    }

    pub async fn reboot_server(&self) -> Result<()> {
        self.json_ok(json!({"cmd": "status", "reboot": true})).await.map(|_| ())
    }

    pub async fn camlist(&self, reset_new_alerts: bool) -> Result<Vec<CameraInfo>> {
        let mut args = json!({"cmd": "camlist"});
        if reset_new_alerts {
            args["reset"] = 2.into();
        }
        let data = self.json_ok(args).await?;
        let arr = data.as_array().cloned().unwrap_or_default();
        Ok(arr.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
    }

    /// One page of `cliplist`/`alertlist` (max 1000 items).
    pub async fn clip_page(&self, q: &ClipQuery) -> Result<Vec<ClipItem>> {
        let cmd = if db_view_is_alerts(&q.view) { "alertlist" } else { "cliplist" };
        let mut args = json!({"cmd": cmd, "camera": q.camera});
        if q.start != 0 && q.end != 0 {
            args["startdate"] = q.start.into();
            args["enddate"] = q.end.into();
        }
        if !q.search.is_empty() {
            args["search"] = q.search.clone().into();
        }
        if !q.view.is_empty() && q.view != "all" {
            args["view"] = q.view.clone().into();
        }
        if q.tiles {
            args["tiles"] = true.into();
        }
        let data = self.json_ok(args).await?;
        let arr = data.as_array().cloned().unwrap_or_default();
        Ok(arr.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
    }

    /// Full clip list with UI3's 1000-item continuation paging. `progress` receives each page as it arrives.
    pub async fn clip_list(&self, q: &ClipQuery, mut progress: impl FnMut(&[ClipItem])) -> Result<Vec<ClipItem>> {
        let mut all = Vec::new();
        let mut q = q.clone();
        loop {
            let page = self.clip_page(&q).await?;
            progress(&page);
            let n = page.len();
            let cont = q.start != 0 && q.end != 0 && n >= 1000;
            let last_date = page.iter().rev().take(200).find(|c| !c.extra.contains_key("newalerts")).map(|c| c.date);
            all.extend(page);
            match (cont, last_date) {
                (true, Some(d)) if d < q.end => q.end = d,
                _ => break,
            }
        }
        // Continuation re-requests the boundary second; drop duplicates by path.
        let mut seen = std::collections::HashSet::new();
        all.retain(|c| seen.insert(c.path.clone()));
        Ok(all)
    }

    pub async fn clip_stats(&self, path: &str) -> Result<(i64, String)> {
        let d = self.json_ok(json!({"cmd": "clipstats", "path": path})).await?;
        Ok((d.get("msec").and_then(crate::lenient::value_to_f64).unwrap_or(0.0) as i64, d.get("filesize").and_then(|v| v.as_str()).unwrap_or("").to_string()))
    }

    pub async fn timeline(&self, camera: &str, start_ms: i64, end_ms: i64, msecpp: i64) -> Result<TimelineData> {
        let args = json!({
            "cmd": "timeline",
            "startdate": start_ms.div_euclid(1000),
            "enddate": (end_ms + 999).div_euclid(1000),
            "msecpp": msecpp,
            "camera": camera,
        });
        let data = self.json_ok(args).await?;
        serde_json::from_value(data).context("parsing timeline")
    }

    pub async fn camconfig_get(&self, camera: &str) -> Result<Value> {
        self.json_ok(json!({"cmd": "camconfig", "camera": camera})).await
    }

    pub async fn camconfig(&self, camera: &str, change: CamConfig) -> Result<Value> {
        let mut args = json!({"cmd": "camconfig", "camera": camera});
        let o = args.as_object_mut().unwrap();
        match change {
            CamConfig::ManualRecord(b) => o.insert("manrec".into(), b.into()),
            CamConfig::Reset => o.insert("reset".into(), true.into()),
            CamConfig::Reboot => o.insert("reboot".into(), true.into()),
            CamConfig::Enable(b) => o.insert("enable".into(), b.into()),
            CamConfig::Pause(v) => o.insert("pause".into(), v.into()),
            CamConfig::Motion(b) => o.insert("motion".into(), b.into()),
            CamConfig::Schedule(b) => o.insert("schedule".into(), b.into()),
            CamConfig::PtzCycle(b) => o.insert("ptzcycle".into(), b.into()),
            CamConfig::PtzEvents(b) => o.insert("ptzevents".into(), b.into()),
            CamConfig::Alerts(v) => o.insert("alerts".into(), v.into()),
            CamConfig::Record(v) => o.insert("record".into(), v.into()),
            CamConfig::Push(b) => o.insert("push".into(), b.into()),
            CamConfig::Output(b) => o.insert("output".into(), b.into()),
            CamConfig::SetMotion(v) => o.insert("setmotion".into(), v),
            CamConfig::Raw(m) => {
                o.extend(m);
                None
            }
        };
        self.json_ok(args).await
    }

    pub async fn ptz_info(&self, camera: &str) -> Result<PtzInfo> {
        let d = self.json_ok(json!({"cmd": "ptz", "camera": camera})).await?;
        serde_json::from_value(d).context("parsing ptz info")
    }

    /// Momentary PTZ command (presets, IR, home...).
    pub async fn ptz(&self, camera: &str, button: i32) -> Result<()> {
        self.json_ok(json!({"cmd": "ptz", "camera": camera, "button": button})).await.map(|_| ())
    }

    /// Press-and-hold PTZ: `start=true` begins motion, `false` stops.
    pub async fn ptz_hold(&self, camera: &str, button: i32, start: bool) -> Result<()> {
        let mut args = json!({"cmd": "ptz", "camera": camera, "button": button, "updown": if start {1} else {0}});
        if !start {
            // UI3 adds `stop: true` to stops (ui3.js:7815).
            args["stop"] = json!(true);
        }
        self.json_ok(args).await.map(|_| ())
    }

    pub async fn ptz_set_preset(&self, camera: &str, preset: i32, description: &str) -> Result<()> {
        self.json_ok(json!({"cmd": "ptz", "camera": camera, "button": 100 + preset, "description": description})).await.map(|_| ())
    }

    /// 3D positioning / click-to-center (`PTZ_relative`): x,y in 0..1 of the image, z in -1..1.
    pub async fn ptz_relative(&self, camera: &str, x: f64, y: f64, z: f64) -> Result<()> {
        self.json_ok(json!({"cmd": "ptz", "camera": camera, "button": 4, "xperc": x.clamp(0.0, 1.0), "yperc": y.clamp(0.0, 1.0), "zperc": z.clamp(-1.0, 1.0)})).await.map(|_| ())
    }

    pub async fn ptz_joystick(&self, camera: &str, value: i64) -> Result<()> {
        self.json_ok(json!({"cmd": "ptz", "camera": camera, "joystick": value})).await.map(|_| ())
    }

    pub async fn trigger(&self, camera: &str) -> Result<()> {
        self.json_ok(json!({"cmd": "trigger", "camera": camera})).await.map(|_| ())
    }

    pub async fn update_clip_flags(&self, path: &str, flags: i64, mask: i64) -> Result<()> {
        self.json_ok(json!({"cmd": "update", "path": path, "flags": flags, "mask": mask})).await.map(|_| ())
    }

    pub async fn update_clip_memo(&self, path: &str, memo: &str) -> Result<()> {
        self.json_ok(json!({"cmd": "update", "path": path, "memo": memo})).await.map(|_| ())
    }

    pub async fn delete_clip(&self, path: &str, is_alert: bool) -> Result<()> {
        let cmd = if is_alert { "delalert" } else { "delclip" };
        self.json_ok(json!({"cmd": cmd, "path": path})).await.map(|_| ())
    }

    pub async fn export(&self, r: &ExportRequest) -> Result<Value> {
        let mut args = json!({"cmd": "export", "path": r.path});
        let o = args.as_object_mut().unwrap();
        macro_rules! opt {
            ($k:literal, $v:expr) => {
                if let Some(v) = &$v {
                    o.insert($k.into(), json!(v));
                }
            };
        }
        opt!("startms", r.start_ms);
        opt!("msec", r.msec);
        opt!("format", r.format);
        opt!("profile", r.profile);
        opt!("substream", r.substream);
        opt!("audio", r.audio);
        opt!("gain", r.gain);
        opt!("reencode", r.reencode);
        opt!("overlay", r.overlay);
        opt!("motion", r.motion);
        opt!("timelapse", r.timelapse);
        self.json_ok(args).await
    }

    pub async fn export_list(&self) -> Result<Value> {
        self.json_ok(json!({"cmd": "export"})).await
    }

    pub async fn export_delete(&self, path: &str) -> Result<()> {
        self.json_ok(json!({"cmd": "export", "path": path, "delete": true})).await.map(|_| ())
    }

    pub async fn sysconfig(&self, changes: Option<Map<String, Value>>) -> Result<Value> {
        let mut args = json!({"cmd": "sysconfig"});
        if let Some(c) = changes {
            args.as_object_mut().unwrap().extend(c);
        }
        self.json_ok(args).await
    }

    pub async fn log(&self) -> Result<Value> {
        self.json_ok(json!({"cmd": "log"})).await
    }
    pub async fn users(&self) -> Result<Value> {
        self.json_ok(json!({"cmd": "users"})).await
    }
    pub async fn devices(&self) -> Result<Value> {
        self.json_ok(json!({"cmd": "devices"})).await
    }
    pub async fn update_check(&self) -> Result<Value> {
        self.json_ok(json!({"cmd": "updatecheck"})).await
    }

    /// Admin GET call: `admin?camera=<id>&<args>` (`ExecAdminCall`, ui3.js:35536).
    pub async fn admin_call(&self, camera: &str, args: &str) -> Result<String> {
        let r = self.get_bytes(&format!("admin?camera={}&{}", crate::urls::encode(camera), args.trim_start_matches('&'))).await?;
        Ok(String::from_utf8_lossy(&r.body).into_owned())
    }

    /// Controls a running `/time/` stream (`/time/set`).
    pub async fn timeline_set(&self, speed: Option<i32>, pos: Option<i64>, jump: Option<i32>) -> Result<()> {
        let r = self.get_bytes(&crate::urls::timeline_set(self.opaque(), speed, pos, jump)).await?;
        anyhow::ensure!(r.status < 400, "time/set failed: HTTP {}", r.status);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_views() {
        assert!(db_view_is_alerts("alerts"));
        assert!(db_view_is_alerts("zoneh"));
        assert!(!db_view_is_alerts("zonei"));
        assert!(!db_view_is_alerts("all"));
        assert!(!db_view_is_alerts("flagged"));
    }
}
