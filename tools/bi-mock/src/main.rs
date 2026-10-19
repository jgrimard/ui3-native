//! bi-mock: a small offline imitation of the Blue Iris web server, for developing and testing UI3 Native without a
//! real server. It implements the subset of the protocol the app uses, with the same wire formats:
//!
//! * `POST /json` — login (MD5 challenge), camlist, status, cliplist/alertlist, clipstats, timeline, ptz, camconfig,
//!   trigger, update, delclip/delalert, export, sysconfig, log/users/devices, logout
//! * `GET /video/<id>/2.0` — live stream (H.265 camera, H.264 camera, and a group composed of both)
//! * `GET /file/clips/<path>` — recorded clip stream (`time`/`pos`, `speed`, reverse); `speed=0` returns one JPEG
//! * `GET /time/<id>` — timeline stream / `?jpeg` single frame with `X-UTC`
//! * `GET /thumbs/@id`, `/alerts/@id`, `/image/<id>`, `/image/<id>/preset_N.jpg`, `/clips/<path>`
//!
//! Usage: `bi-mock [port] [--showcase <dir>]` (default 8181). Login: user `admin`, password `admin`.
//! Media comes from `temp/testmedia` (H.265 1440p + H.264 1080p); JPEG frames are generated with the bundled ffmpeg.
//!
//! `--showcase <dir>` replaces the test cameras with one camera per still image `NN-name.png` in `dir` (a 3×3 group
//! for nine images), each encoded once into `temp/showcase-media` with a burned-in timestamp, alternating H.265 and
//! H.264. An image `NNb-*.png` becomes the footage of camera `NN`'s alerts. Used for the README screenshots.

use anyhow::{Context, Result};
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bi_media::elementary::{Elementary, read_elementary};
use bi_proto::VideoCodec;
use bi_proto::stream_parser::StatusBlock;
use bi_proto::stream_writer::StreamWriter;
use bytes::Bytes;
use md5::{Digest, Md5};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Media {
    el: Elementary,
    /// JPEG frames at the stream frame rate (same count as access units).
    jpegs: Vec<Bytes>,
    duration_ms: i64,
}

struct CamDef {
    id: String,
    name: String,
    color: i64,
    ptz: bool,
    newalerts: i64,
    /// Fixed motion state on the live stream; `None` toggles every 10 s.
    motion: Option<bool>,
    /// Media key for this camera's alerts (defaults to the camera's own media).
    alert_media: Option<String>,
}

struct Clip {
    path: String,
    camera: String,
    /// Key into `AppState::cams` for the clip's footage.
    media: String,
    date: i64,
    msec: i64,
    flags: i64,
    is_alert: bool,
    clip_path: String,
    offset: i64,
    memo: String,
}

struct AppState {
    defs: Vec<CamDef>,
    system_name: String,
    /// The "index" group's camlist entry and its stream's `X-CAMLIST` / `X-RECLIST` headers.
    group: Value,
    camlist_header: String,
    reclist_header: String,
    cams: HashMap<String, Arc<Media>>,
    sessions: Mutex<HashSet<String>>,
    pending: Mutex<HashSet<String>>,
    clips: Mutex<Vec<Clip>>,
    ulaw: Bytes,
    start_ms: i64,
    profile: Mutex<i64>,
    signal: Mutex<i64>,
    triggered_until: Mutex<HashMap<String, i64>>,
}

type St = Arc<AppState>;

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

fn md5hex(s: &str) -> String {
    let mut h = Md5::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn project_root() -> PathBuf {
    let mut p = std::env::current_dir().unwrap_or_default();
    loop {
        if p.join("Cargo.toml").exists() && p.join("crates").exists() {
            return p;
        }
        if !p.pop() {
            return std::env::current_dir().unwrap_or_default();
        }
    }
}

fn load_media(root: &std::path::Path, src: &std::path::Path) -> Result<Media> {
    let el = read_elementary(src).with_context(|| format!("reading {}", src.display()))?;
    let dir = src.with_file_name(format!("jpg_{}", src.file_name().unwrap_or_default().to_string_lossy()));
    if !dir.exists() || std::fs::read_dir(&dir)?.count() < el.units.len() {
        std::fs::create_dir_all(&dir)?;
        let ff = root.join("third_party/ffmpeg/bin/ffmpeg.exe");
        let st = std::process::Command::new(ff)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(src)
            .args(["-vf", "scale=1280:-2", "-q:v", "5"])
            .arg(dir.join("f_%04d.jpg"))
            .status()?;
        anyhow::ensure!(st.success(), "ffmpeg failed generating JPEG frames");
    }
    let mut jpegs = Vec::new();
    for i in 1..=el.units.len() {
        jpegs.push(Bytes::from(std::fs::read(dir.join(format!("f_{i:04}.jpg")))?));
    }
    let duration_ms = (el.units.len() as f64 * 1000.0 / el.fps) as i64;
    Ok(Media { el, jpegs, duration_ms })
}

/// The server's UTC offset in minutes (the machine's local zone, so clip times match the burned-in timestamps).
fn tzone_minutes() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60
}

/// Blue Iris camera colors are 0xBBGGRR.
fn bgr(rgb: i64) -> i64 {
    ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF)
}

/// Encodes a still image into a 6 s 1080p15 elementary stream with a ticking Blue Iris-style timestamp. Like Blue
/// Iris recordings, every keyframe is an IDR preceded by parameter sets, so playback can start at any keyframe.
fn encode_still(root: &std::path::Path, img: &std::path::Path, out: &std::path::Path, hevc: bool) -> Result<()> {
    let dir = out.parent().context("no parent")?;
    std::fs::create_dir_all(dir)?;
    // drawtext reads the expansion from a file, avoiding a layer of filtergraph escaping.
    let ts_name = format!("{}.ts.txt", out.file_name().unwrap_or_default().to_string_lossy());
    std::fs::write(dir.join(&ts_name), format!("%{{pts:localtime:{}:%m-%d-%Y %I\\:%M\\:%S %p}}", now_ms() / 1000))?;
    let vf = format!(
        "scale=1920:1080,drawtext=fontfile='C\\:/Windows/Fonts/arialbd.ttf':fontsize=34:fontcolor=white:borderw=2:bordercolor=black@0.6:x=w-tw-28:y=24:textfile={ts_name},format=yuv420p"
    );
    let codec: &[&str] = if hevc { &["-c:v", "libkvazaar", "-b:v", "4M", "-kvazaar-params", "preset=ultrafast,period=32,vps-period=1,open-gop=0"] } else { &["-c:v", "libopenh264", "-b:v", "4M", "-g", "30"] };
    let st = std::process::Command::new(root.join("third_party/ffmpeg/bin/ffmpeg.exe"))
        .current_dir(dir)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-loop", "1", "-framerate", "15", "-t", "6", "-i"])
        .arg(std::path::absolute(img)?)
        .args(["-vf", &vf])
        .args(codec)
        .arg(out.file_name().unwrap_or_default())
        .status()?;
    anyhow::ensure!(st.success(), "ffmpeg failed encoding {}", img.display());
    Ok(())
}

type Showcase = (Vec<CamDef>, HashMap<String, Arc<Media>>);

/// One camera per `NN-name.png|jpg` in `dir`; `NNb-*.png` is camera `NN`'s alert footage.
fn load_showcase(root: &std::path::Path, dir: &std::path::Path) -> Result<Showcase> {
    const PALETTE: [i64; 9] = [0x2E86DE, 0xE67E22, 0x27AE60, 0x8E44AD, 0xC0392B, 0x16A085, 0xF1C40F, 0xD35400, 0x95A5A6];
    let mut images: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("png") || x.eq_ignore_ascii_case("jpg")))
        .collect();
    images.sort();
    // Cached per day so the burned-in date matches the clip list (bump the "v" when the encoding changes).
    let out_dir = root.join("temp/showcase-media").join(format!("v2-{}", now_ms() / 86_400_000));
    let (mut defs, mut cams) = (Vec::<CamDef>::new(), HashMap::new());
    // (camera NN prefix, camera index) and (NN prefix of the camera it belongs to, media key).
    let (mut prefixes, mut alts) = (Vec::new(), Vec::new());
    for (i, img) in images.iter().enumerate() {
        let stem = img.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let (prefix, rest) = stem.split_once('-').unwrap_or(("", &stem));
        let hevc = i % 2 == 0;
        let out = out_dir.join(format!("{stem}.{}", if hevc { "hevc" } else { "h264" }));
        if !out.exists() {
            println!("[{}/{}] encoding {stem}", i + 1, images.len());
            encode_still(root, img, &out, hevc)?;
        }
        let media = Arc::new(load_media(root, &out)?);
        if let Some(base) = prefix.strip_suffix('b') {
            alts.push((base.to_string(), stem.clone()));
            cams.insert(stem, media);
            continue;
        }
        // "back-fence-night" -> id "backfence", name "Back Fence" (the IR image speaks for itself).
        let words: Vec<&str> = rest.split('-').filter(|w| *w != "night").collect();
        let name = words.iter().map(|w| w[..1].to_ascii_uppercase() + &w[1..]).collect::<Vec<_>>().join(" ");
        let id = words.concat();
        prefixes.push((prefix.to_string(), defs.len()));
        defs.push(CamDef { id: id.clone(), name, color: bgr(PALETTE[defs.len() % PALETTE.len()]), ptz: id == "street", newalerts: 0, alert_media: None, motion: Some(id == "backfence") });
        cams.insert(id, media);
    }
    for (base, key) in alts {
        if let Some((_, i)) = prefixes.iter().find(|(p, _)| *p == base) {
            defs[*i].alert_media = Some(key);
            defs[*i].newalerts = 2;
        }
    }
    anyhow::ensure!(!defs.is_empty(), "no NN-name images in {}", dir.display());
    Ok((defs, cams))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let port: u16 = args.first().and_then(|p| p.parse().ok()).unwrap_or(8181);
    let showcase = args.iter().position(|a| a == "--showcase").and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let root = project_root();
    let ulaw = Bytes::from(std::fs::read(root.join("temp/testmedia/tone8k.ulaw")).unwrap_or_else(|_| vec![0xFF; 48000]));
    let (defs, mut cams, system_name) = match &showcase {
        Some(dir) => {
            let (defs, cams) = load_showcase(&root, dir)?;
            (defs, cams, "Home NVR".to_string())
        }
        None => {
            let h265 = Arc::new(load_media(&root, &root.join("temp/testmedia/test1440.hevc"))?);
            let h264 = Arc::new(load_media(&root, &root.join("temp/testmedia/test1080.h264"))?);
            let defs = vec![
                CamDef { id: "cam265".into(), name: "Driveway (H.265)".into(), color: 0x0000FF, ptz: true, newalerts: 2, alert_media: None, motion: None },
                CamDef { id: "cam264".into(), name: "Porch (H.264)".into(), color: 0x00FF00, ptz: false, newalerts: 2, alert_media: None, motion: None },
            ];
            (defs, HashMap::from([("cam265".to_string(), h265), ("cam264".to_string(), h264)]), "Mock Blue Iris".to_string())
        }
    };
    // The group's server-composited stream just reuses the last camera's media.
    let last = cams[&defs.last().context("no cameras")?.id].clone();
    cams.insert("index".to_string(), last);
    let members: Vec<&str> = defs.iter().map(|d| d.id.as_str()).collect();
    let (group_rects, group_w, group_h, reclist) = if showcase.is_some() {
        // Square-ish grid of 640x360 tiles.
        let cols = (defs.len() as f64).sqrt().ceil() as i32;
        let rows = (defs.len() as i32 + cols - 1) / cols;
        let rects: Vec<[i32; 4]> = (0..defs.len() as i32).map(|i| [i % cols * 640, i / cols * 360, (i % cols + 1) * 640, (i / cols + 1) * 360]).collect();
        let reclist = rects.iter().map(|r| format!("[{},{},{},{}]", r[0], r[1], r[2], r[3])).collect::<Vec<_>>().join(",");
        (rects, cols * 640, rows * 360, reclist)
    } else {
        (vec![[0, 0, 1920, 1080], [1920, 0, 3840, 2160]], 1920, 1080, "[0,0,960,1080],[960,0,1920,1080]".to_string())
    };
    let group = json!({"optionValue": "index", "optionDisplay": "All cameras", "group": members, "rects": group_rects, "width": group_w, "height": group_h, "isEnabled": true});
    let camlist_header = members.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(",");
    // Clip database: one clip every 10 minutes for the last 24 h per camera, alerts in every third.
    let start = now_ms();
    let mut clips = Vec::new();
    let mut id = 1000;
    for k in 0..144 {
        for (ci, cam) in defs.iter().enumerate() {
            id += 1;
            let date = (start - k as i64 * 600_000) / 1000;
            let m = &cams[&cam.id];
            let cpath = format!("@{id}.bvr");
            let flags = if k % 7 == 0 { 2 } else { 0 } | ((1 << 8) * (k == 0) as i64);
            clips.push(Clip { path: cpath.clone(), camera: cam.id.clone(), media: cam.id.clone(), date, msec: m.duration_ms, flags, is_alert: false, clip_path: cpath.clone(), offset: 0, memo: String::new() });
            // Showcase: staggered across cameras, mostly on the cameras that have alert footage.
            let alert = match showcase {
                Some(_) => (k + ci) % 3 == 0 && (cam.alert_media.is_some() || (k + ci) % 9 == 0),
                None => k % 3 == 0,
            };
            if alert {
                id += 1;
                // Bit 26 is the person icon, bit 27 the vehicle icon.
                let (media, memo, ai) = match (&cam.alert_media, &showcase) {
                    (Some(alt), _) if cam.id.contains("door") => (alt.clone(), "person".to_string(), 1 << 26),
                    (Some(alt), _) => (alt.clone(), "car".to_string(), 1 << 27),
                    (None, Some(_)) => (cam.id.clone(), String::new(), 0),
                    (None, None) => (cam.id.clone(), if k % 9 == 0 { "delivery".into() } else { String::new() }, if k % 6 == 0 { 1 << 26 } else { 1 << 27 }),
                };
                let aflags = (1 << 16) | (1 << 17) | ai;
                // The recording the alert points into contains the event too.
                if let Some(parent) = clips.last_mut() {
                    parent.media = media.clone();
                }
                clips.push(Clip { path: format!("@{id}.bvr"), camera: cam.id.clone(), media, date: date + 2, msec: 3000, flags: aflags, is_alert: true, clip_path: cpath, offset: 2000, memo });
            }
        }
    }
    let state = Arc::new(AppState {
        defs,
        system_name,
        group,
        camlist_header,
        reclist_header: reclist,
        cams,
        sessions: Mutex::new(HashSet::new()),
        pending: Mutex::new(HashSet::new()),
        clips: Mutex::new(clips),
        ulaw,
        start_ms: start,
        profile: Mutex::new(1),
        signal: Mutex::new(1),
        triggered_until: Mutex::new(HashMap::new()),
    });
    let app = Router::new()
        .route("/json", post(json_handler))
        .route("/video/{id}/{ver}", get(live_handler))
        .route("/file/clips/{path}", get(clip_handler))
        .route("/time/{id}", get(time_handler))
        .route("/thumbs/{id}", get(thumb_handler))
        .route("/alerts/{id}", get(thumb_handler))
        .route("/image/{id}", get(image_handler))
        .route("/image/{id}/{preset}", get(image_handler2))
        .route("/clips/{path}", get(download_handler))
        .with_state(state);
    let addr = format!("127.0.0.1:{port}");
    println!("bi-mock listening on http://{addr}/  (user admin / pass admin)");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn session_ok(st: &St, q: &HashMap<String, String>) -> bool {
    q.get("session").is_some_and(|s| st.sessions.lock().unwrap().contains(s))
}

fn forbidden() -> Response {
    (StatusCode::FORBIDDEN, "invalid session").into_response()
}

async fn json_handler(State(st): State<St>, body: String) -> Response {
    let args: Value = serde_json::from_str(&body).unwrap_or_default();
    let cmd = args["cmd"].as_str().unwrap_or("");
    let session = args["session"].as_str().unwrap_or("").to_string();
    let ok = |data: Value| axum::Json(json!({"result": "success", "session": session, "data": data})).into_response();
    if cmd == "login" {
        if let Some(resp) = args["response"].as_str() {
            let expected = md5hex(&format!("admin:{session}:admin"));
            if st.pending.lock().unwrap().remove(&session) && resp == expected {
                st.sessions.lock().unwrap().insert(session.clone());
                return ok(json!({
                    "user": "admin", "admin": true, "system name": st.system_name, "version": "6.0.1.1",
                    "profiles": ["Inactive", "Away", "Home", "Night", "Vacation", "Profile 5", "Profile 6", "Profile 7"],
                    "schedules": ["Default", "Weekend"], "tzone": tzone_minutes(), "lan": true,
                    "streams": ["VBR q=70% 2048kbps gop=1000 1080p *", "VBR q=60% 512kbps gop=1000 720p *", "CBR 256kbps gop=1000 480p"],
                    "www_sounds": ["alarm.wav", "chime.mp3"]
                }));
            }
            return axum::Json(json!({"result": "fail", "data": {"reason": "Invalid credentials"}})).into_response();
        }
        if st.sessions.lock().unwrap().contains(&session) {
            return ok(json!({"user": "admin", "admin": true, "system name": st.system_name, "version": "6.0.1.1"}));
        }
        let sid = format!("{:08X}", (now_ms() as u64).wrapping_mul(2654435761) as u32);
        st.pending.lock().unwrap().insert(sid.clone());
        return axum::Json(json!({"result": "fail", "session": sid, "data": {"reason": "missing response"}})).into_response();
    }
    if !st.sessions.lock().unwrap().contains(&session) {
        return axum::Json(json!({"result": "fail", "data": {"reason": "Invalid session"}})).into_response();
    }
    let trig = st.triggered_until.lock().unwrap().clone();
    match cmd {
        "logout" => {
            st.sessions.lock().unwrap().remove(&session);
            ok(Value::Null)
        }
        "camlist" => {
            let cam = |d: &CamDef| {
                let t = trig.get(&d.id).is_some_and(|u| *u > now_ms());
                let m = &st.cams[&d.id];
                json!({"optionValue": d.id, "optionDisplay": d.name, "isEnabled": true, "webcast": true, "ptz": d.ptz, "audio": true,
                       "width": m.el.width, "height": m.el.height, "FPS": 15, "BPS": 2_000_000, "isOnline": true, "isRecording": true, "isTriggered": t,
                       "isAlerting": t, "isMotion": t, "newalerts": d.newalerts, "nAlerts": 48, "nClips": 144, "color": d.color, "ip": "10.0.0.5"})
            };
            ok(Value::Array(std::iter::once(st.group.clone()).chain(st.defs.iter().map(cam)).collect()))
        }
        "status" => {
            if let Some(p) = args.get("profile").and_then(|p| p.as_i64())
                && p >= 0
            {
                *st.profile.lock().unwrap() = p;
            }
            if let Some(s) = args.get("signal").and_then(|p| p.as_i64()) {
                *st.signal.lock().unwrap() = s;
            }
            let up = (now_ms() - st.start_ms) / 1000;
            ok(json!({"signal": *st.signal.lock().unwrap(), "cpu": "7", "ram": "1.2G", "mem": "1.2G", "memphys": "16G", "memload": "38%",
                "disks": [{"disk": "D:", "allocated": 2_000_000, "used": 1_234_567, "free": 800_000, "total": 4_000_000}],
                "clips": "288 (12 GB)", "uptime": format!("0:{:02}:{:02}:{:02}", up / 3600, (up / 60) % 60, up % 60), "profile": *st.profile.lock().unwrap(),
                "lock": 0, "schedule": "Default", "warnings": 1, "alerts": 2, "tzone": tzone_minutes(), "time": now_ms()}))
        }
        "cliplist" | "alertlist" => {
            let want_alerts = cmd == "alertlist";
            let camera = args["camera"].as_str().unwrap_or("index").to_string();
            let view = args["view"].as_str().unwrap_or("all");
            let end = args["enddate"].as_i64().unwrap_or(i64::MAX);
            let start = args["startdate"].as_i64().unwrap_or(0);
            let search = args["search"].as_str().unwrap_or("").to_ascii_lowercase();
            let clips = st.clips.lock().unwrap();
            let items: Vec<Value> = clips
                .iter()
                .filter(|c| c.is_alert == want_alerts || view == "flagged")
                .filter(|c| camera == "index" || c.camera.eq_ignore_ascii_case(&camera))
                .filter(|c| view != "flagged" || c.flags & 2 != 0)
                .filter(|c| c.date <= end && c.date >= start)
                .filter(|c| search.is_empty() || c.memo.contains(&search))
                .take(1000)
                .map(|c| {
                    let color = st.defs.iter().find(|d| d.id == c.camera).map_or(0, |d| d.color);
                    let m = &st.cams[&c.media];
                    let filetype = if m.el.codec == VideoCodec::H265 { "bvr H265" } else { "bvr H264" };
                    let mut v = json!({"camera": c.camera, "path": c.path, "offset": c.offset, "date": c.date, "color": color,
                        "flags": c.flags, "msec": c.msec, "filesize": format!("{}sec (2.1M)", c.msec / 1000), "filetype": filetype, "res": format!("{}x{}", m.el.width, m.el.height), "memo": c.memo});
                    if c.is_alert {
                        v["clip"] = c.clip_path.clone().into();
                    }
                    v
                })
                .collect();
            ok(Value::Array(items))
        }
        "clipstats" => {
            let path = args["path"].as_str().unwrap_or("");
            let clips = st.clips.lock().unwrap();
            let m = clips.iter().find(|c| c.path == path).map(|c| c.msec).unwrap_or(6000);
            ok(json!({"msec": m, "filesize": "6sec (2.1M)"}))
        }
        "timeline" => {
            let sd = args["startdate"].as_i64().unwrap_or(0);
            let ed = args["enddate"].as_i64().unwrap_or(0);
            let msecpp = args["msecpp"].as_i64().unwrap_or(1000).max(1);
            let camera = args["camera"].as_str().unwrap_or("index");
            let tracks: Vec<&str> = if camera == "index" { st.defs.iter().map(|d| d.id.as_str()).collect() } else { vec![camera] };
            let clips = st.clips.lock().unwrap();
            let mut tclips = Vec::new();
            let mut talerts = Vec::new();
            for c in clips.iter() {
                let Some(track) = tracks.iter().position(|t| *t == c.camera) else { continue };
                let t0 = c.date * 1000;
                if t0 / 1000 > ed || t0 / 1000 + 600 < sd {
                    continue;
                }
                let x1 = (t0 - sd * 1000) / msecpp;
                if c.is_alert {
                    let x2 = x1 + (c.msec / msecpp).max(1);
                    talerts.push(json!({"x1": x1, "x2": x2, "type": c.flags, "tracks": 1 << track, "record": c.path}));
                } else {
                    // Continuous recording: each clip covers 10 minutes, ending no later than now.
                    let x2 = (x1 + (600_000 / msecpp).max(1)).min((now_ms() - sd * 1000) / msecpp).max(x1 + 1);
                    tclips.push(json!({"x1": x1, "x2": x2, "track": track}));
                }
            }
            let colors: Vec<i64> = tracks.iter().map(|t| st.defs.iter().find(|d| d.id == *t).map_or(0, |d| d.color)).collect();
            ok(json!({"colors": colors, "alerts": talerts, "clips": tclips}))
        }
        "ptz" => {
            if args.get("button").is_some() {
                eprintln!("ptz {}", args);
                ok(Value::Null)
            } else {
                ok(json!({"brightness": 8, "contrast": 3, "irmode": 2, "powermode": -1, "presetnum": 5, "presets": ["Gate", "Street", {"num": 3, "desc": "Garage"}], "talksamplerate": 8000}))
            }
        }
        "camconfig" => ok(json!({"motion": true, "schedule": false, "ptzcycle": false, "ptzevents": false, "alerts": -1, "record": 2, "push": true, "output": false, "manrec": false,
            "setmotion": {"audio_trigger": false, "sense": 8000, "contrast": 67}})),
        "trigger" => {
            let cam = args["camera"].as_str().unwrap_or("").to_string();
            st.triggered_until.lock().unwrap().insert(cam, now_ms() + 10_000);
            ok(Value::Null)
        }
        "update" => {
            let path = args["path"].as_str().unwrap_or("");
            let mut clips = st.clips.lock().unwrap();
            if let Some(c) = clips.iter_mut().find(|c| c.path == path) {
                if let (Some(f), Some(m)) = (args["flags"].as_i64(), args["mask"].as_i64()) {
                    c.flags = (c.flags & !m) | (f & m);
                }
                if let Some(memo) = args["memo"].as_str() {
                    c.memo = memo.to_string();
                }
            }
            ok(Value::Null)
        }
        "delclip" | "delalert" => {
            let path = args["path"].as_str().unwrap_or("").to_string();
            st.clips.lock().unwrap().retain(|c| c.path != path);
            ok(Value::Null)
        }
        "export" => ok(json!([{"path": "@9001.mp4", "status": "done", "progress": 100, "msec": 6000, "utc": now_ms() - 60_000, "uri": "Clipboard\\cam265.20261014_120000.mp4", "filesize": "2.1M"}])),
        "sysconfig" => ok(json!({"archive": false, "schedule": true, "manrecsec": 7200})),
        "log" => ok(json!([{"level": 0, "date": now_ms() / 1000, "obj": "cam265", "msg": "Signal: restored"}, {"level": 1, "date": now_ms() / 1000 - 60, "obj": "Server", "msg": "Mock warning"}])),
        "users" => ok(json!([{"user": "admin", "admin": true, "lan": true, "object": "Console"}])),
        "devices" => ok(json!([{"name": "Pixel", "type": "Android", "lastseen": now_ms() / 1000}])),
        "updatecheck" => ok(json!({"version": "6.0.1.1", "current": true})),
        _ => axum::Json(json!({"result": "fail", "data": {"reason": format!("mock: unsupported cmd {cmd}")}})).into_response(),
    }
}

fn video_headers(ct: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(ct).unwrap());
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h
}

fn jpeg_response(data: Bytes, utc: Option<i64>) -> Response {
    let mut h = video_headers("image/jpeg");
    if let Some(u) = utc {
        h.insert("X-UTC", HeaderValue::from_str(&u.to_string()).unwrap());
    }
    (h, data).into_response()
}

fn fourcc(m: &Media) -> &'static [u8; 4] {
    if m.el.codec == VideoCodec::H265 { b"H265" } else { b"H264" }
}

/// Infinite live stream paced in real time, looping the media; first AU of each loop is a keyframe.
fn live_stream(m: Arc<Media>, ulaw: Bytes, audio: bool, motion: Option<bool>) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    async_stream(move |tx| async move {
        let w = StreamWriter::new(fourcc(&m), m.el.width, m.el.height, audio.then_some(8000));
        if tx.send(Bytes::from(w.out)).await.is_err() {
            return;
        }
        let frame_ms = 1000.0 / m.el.fps;
        let start = tokio::time::Instant::now();
        let mut n: u64 = 0;
        let mut apos = 0usize;
        loop {
            let i = (n % m.el.units.len() as u64) as usize;
            let t = (n as f64 * frame_ms) as u32;
            let mut w = StreamWriter { out: Vec::new() };
            w.video(0, t, now_ms() as u64, &m.el.units[i].data);
            if audio {
                let want = (frame_ms * 8.0) as usize;
                let mut chunk = Vec::with_capacity(want);
                for _ in 0..want {
                    chunk.push(ulaw[apos % ulaw.len()]);
                    apos += 1;
                }
                w.audio(&chunk);
            }
            if n.is_multiple_of(15) {
                w.status(&StatusBlock { rec: true, motion: motion.unwrap_or((n / 150) % 2 == 1), fps_x100: (m.el.fps * 100.0) as i32, apeak: 9000, ..Default::default() });
            }
            if tx.send(Bytes::from(w.out)).await.is_err() {
                return;
            }
            n += 1;
            tokio::time::sleep_until(start + Duration::from_micros((n as f64 * frame_ms * 1000.0) as u64)).await;
        }
    })
}

/// Recorded playback from `start_ms` at `speed` (100 = 1x, negative = reverse), ending with a type-4 block.
fn clip_stream(m: Arc<Media>, start_ms: i64, speed: i32, ulaw: Bytes, audio: bool, utc_base: i64) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    async_stream(move |tx| async move {
        let audio = audio && speed == 100;
        let w = StreamWriter::new(fourcc(&m), m.el.width, m.el.height, audio.then_some(8000));
        if tx.send(Bytes::from(w.out)).await.is_err() {
            return;
        }
        let n = m.el.units.len();
        let frame_ms = 1000.0 / m.el.fps;
        let target = ((start_ms as f64 / frame_ms) as usize).min(n - 1);
        let rate = (speed.unsigned_abs() as f64 / 100.0).max(0.01);
        let wall = tokio::time::Instant::now();
        let mut sent = 0u64;
        let send = |i: usize, w: &mut StreamWriter| {
            let t = (i as f64 * frame_ms) as u32;
            w.video(((i as f64 / n as f64) * 10000.0) as u16, t, (utc_base + t as i64) as u64, &m.el.units[i].data);
        };
        if speed >= 0 {
            // Start at the keyframe at or before the target (Blue Iris does the same).
            let mut i = (0..=target).rev().find(|k| m.el.units[*k].keyframe).unwrap_or(0);
            while i < n {
                let mut w = StreamWriter { out: Vec::new() };
                send(i, &mut w);
                if audio {
                    let a0 = (i as f64 * frame_ms * 8.0) as usize % ulaw.len();
                    let a1 = (a0 + (frame_ms * 8.0) as usize).min(ulaw.len());
                    w.audio(&ulaw[a0..a1]);
                }
                if tx.send(Bytes::from(w.out)).await.is_err() {
                    return;
                }
                sent += 1;
                if i >= target {
                    tokio::time::sleep_until(wall + Duration::from_micros(((sent as f64 * frame_ms / rate) * 1000.0) as u64)).await;
                }
                i += 1;
            }
        } else {
            // Reverse: keyframes only, walking backwards.
            let keys: Vec<usize> = (0..=target).rev().filter(|k| m.el.units[*k].keyframe).collect();
            for i in keys {
                let mut w = StreamWriter { out: Vec::new() };
                send(i, &mut w);
                if tx.send(Bytes::from(w.out)).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis((frame_ms * 30.0 / rate) as u64)).await;
            }
        }
        let mut w = StreamWriter { out: Vec::new() };
        w.end();
        let _ = tx.send(Bytes::from(w.out)).await;
    })
}

/// Tiny channel-backed stream helper (avoids pulling in async-stream).
fn async_stream<F, Fut>(f: F) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>>
where
    F: FnOnce(tokio::sync::mpsc::Sender<Bytes>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(4);
    tokio::spawn(f(tx));
    futures_util::StreamExt::map(tokio_stream::wrappers::ReceiverStream::new(rx), Ok)
}

async fn live_handler(State(st): State<St>, Path((id, _ver)): Path<(String, String)>, Query(q): Query<HashMap<String, String>>) -> Response {
    if !session_ok(&st, &q) {
        return forbidden();
    }
    let Some(m) = st.cams.get(&id).cloned() else { return (StatusCode::NOT_FOUND, "no such camera").into_response() };
    let audio = q.get("audio").is_some_and(|a| a != "0");
    let motion = st.defs.iter().find(|d| d.id == id).and_then(|d| d.motion);
    let mut h = video_headers("video/x-bi");
    if id == "index" {
        h.insert("X-CAMLIST", HeaderValue::from_str(&st.camlist_header).unwrap());
        h.insert("X-RECLIST", HeaderValue::from_str(&st.reclist_header).unwrap());
    }
    (h, Body::from_stream(live_stream(m, st.ulaw.clone(), audio, motion))).into_response()
}

fn find_clip(st: &St, path: &str) -> Option<(Arc<Media>, i64, i64)> {
    let clips = st.clips.lock().unwrap();
    let p = if path.starts_with('@') { path.to_string() } else { format!("@{path}") };
    let id = bi_proto::api::rec_id_from_path(&p).to_string();
    let c = clips.iter().find(|c| bi_proto::api::rec_id_from_path(&c.path) == id)?;
    let m = st.cams.get(&c.media)?.clone();
    // Alerts start at their offset within the clip.
    Some((m, if c.is_alert { c.offset } else { 0 }, c.date * 1000))
}

async fn clip_handler(State(st): State<St>, Path(path): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    if !session_ok(&st, &q) {
        return forbidden();
    }
    let Some((m, base_off, utc)) = find_clip(&st, &path) else { return (StatusCode::NOT_FOUND, "no such clip").into_response() };
    let speed: i32 = q.get("speed").and_then(|s| s.parse().ok()).unwrap_or(100);
    let time_ms = q
        .get("time")
        .and_then(|t| t.parse::<f64>().ok())
        .map(|t| t as i64)
        .or_else(|| q.get("pos").and_then(|p| p.parse::<i64>().ok()).map(|p| p * m.duration_ms / 10000))
        .unwrap_or(0)
        + if q.contains_key("time") { 0 } else { base_off };
    let time_ms = time_ms.clamp(0, m.duration_ms - 1);
    if speed == 0 || q.contains_key("cache") || q.contains_key("decode") && !q.contains_key("speed") {
        let i = ((time_ms as f64 / m.duration_ms as f64) * m.jpegs.len() as f64) as usize;
        return jpeg_response(m.jpegs[i.min(m.jpegs.len() - 1)].clone(), Some(utc + time_ms));
    }
    let audio = q.get("audio").is_some_and(|a| a != "0");
    (video_headers("video/x-bi"), Body::from_stream(clip_stream(m, time_ms, speed, st.ulaw.clone(), audio, utc))).into_response()
}

async fn time_handler(State(st): State<St>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    if !session_ok(&st, &q) {
        return forbidden();
    }
    if id == "set" {
        return (StatusCode::OK, "ok").into_response();
    }
    let Some(m) = st.cams.get(&id).cloned() else { return (StatusCode::NOT_FOUND, "no such camera").into_response() };
    let mut pos: i64 = q.get("pos").and_then(|p| p.parse().ok()).unwrap_or(now_ms());
    let jump: i32 = q.get("jump").and_then(|j| j.parse().ok()).unwrap_or(0);
    if jump != 0 {
        // Jump to the previous/next alert start.
        let clips = st.clips.lock().unwrap();
        let mut times: Vec<i64> = clips.iter().filter(|c| c.is_alert && (id == "index" || c.camera == id)).map(|c| c.date * 1000).collect();
        times.sort();
        pos = if jump > 0 { times.iter().copied().find(|t| *t > pos + 1000).unwrap_or(pos) } else { times.iter().rev().copied().find(|t| *t < pos - 1000).unwrap_or(pos) };
    }
    // Map the absolute time onto the looping media.
    let off = pos.rem_euclid(m.duration_ms);
    if q.contains_key("jpeg") {
        let i = ((off as f64 / m.duration_ms as f64) * m.jpegs.len() as f64) as usize;
        return jpeg_response(m.jpegs[i.min(m.jpegs.len() - 1)].clone(), Some(pos));
    }
    let speed: i32 = q.get("speed").and_then(|s| s.parse().ok()).unwrap_or(100);
    let audio = q.get("audio").is_some_and(|a| a != "0");
    (video_headers("video/x-bi"), Body::from_stream(clip_stream(m, off, speed, st.ulaw.clone(), audio, pos - off))).into_response()
}

async fn thumb_handler(State(st): State<St>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    if !session_ok(&st, &q) {
        return forbidden();
    }
    match find_clip(&st, &id) {
        Some((m, off, _)) => {
            let i = ((off as f64 / m.duration_ms as f64) * m.jpegs.len() as f64) as usize;
            jpeg_response(m.jpegs[i.min(m.jpegs.len() - 1)].clone(), None)
        }
        None => (StatusCode::NOT_FOUND, "").into_response(),
    }
}

async fn image_handler(State(st): State<St>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    if !session_ok(&st, &q) {
        return forbidden();
    }
    let Some(m) = st.cams.get(&id) else { return (StatusCode::NOT_FOUND, "").into_response() };
    let i = ((now_ms() % m.duration_ms) as f64 / m.duration_ms as f64 * m.jpegs.len() as f64) as usize;
    jpeg_response(m.jpegs[i.min(m.jpegs.len() - 1)].clone(), Some(now_ms()))
}

async fn image_handler2(State(st): State<St>, Path((id, preset)): Path<(String, String)>, Query(q): Query<HashMap<String, String>>) -> Response {
    if !session_ok(&st, &q) {
        return forbidden();
    }
    let Some(m) = st.cams.get(&id) else { return (StatusCode::NOT_FOUND, "").into_response() };
    let n: usize = preset.trim_start_matches("preset_").trim_end_matches(".jpg").parse().unwrap_or(1);
    jpeg_response(m.jpegs[(n * 7) % m.jpegs.len()].clone(), None)
}

async fn download_handler(State(st): State<St>, Path(path): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    if !session_ok(&st, &q) {
        return forbidden();
    }
    let Some((m, _, utc)) = find_clip(&st, &path) else { return (StatusCode::NOT_FOUND, "").into_response() };
    let data = bi_media::elementary::synth_bistream(&m.el, utc as u64, None);
    let mut h = video_headers("application/octet-stream");
    h.insert(header::CONTENT_DISPOSITION, HeaderValue::from_str(&format!("attachment; filename=\"{}.bvr\"", bi_proto::api::rec_id_from_path(&path))).unwrap());
    (h, data).into_response()
}
