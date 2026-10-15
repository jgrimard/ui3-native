//! bi-capture: protocol probe for Blue Iris.
//!
//! ```text
//! bi-capture info                         login + camlist summary
//! bi-capture live <cam> [secs] [mode]     record /video/<cam> to temp/captures, print codec stats
//! bi-capture clip <path> [secs] [mode]    record /file/clips/<path>
//! bi-capture raw <relative-path> [secs]   record any stream path as-is
//! bi-capture timeline <cam> <start> <end> [msecpp]  print recorded ranges (start/end: unix seconds)
//! bi-capture cliplist <cam> <start> <end>           print recording files (start/end: unix seconds)
//! ```
//! mode: `native` (d2w=1, vcs=3; default), `h264` (vcs=1), `h265pref` (vc=2,vcs=3), `default` (UI3's 1080p VBR)
//!
//! Server settings come from `temp/bi-server.json` (`{"url": "...", "user": "...", "pass": "..."}`) or the
//! BI_URL / BI_USER / BI_PASS environment variables. `temp/` is gitignored.

use anyhow::{Context, Result, bail};
use bi_proto::profile::{self, SupportedCodecs};
use bi_proto::stream_parser::{StreamEvent, StreamParser};
use bi_proto::urls::{self, AudioRequest, ClipStart};
use bi_proto::{BiClient, Credentials};
use futures_util::StreamExt;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(serde::Deserialize, Default)]
struct ServerCfg {
    #[serde(default)]
    url: String,
    #[serde(default)]
    user: String,
    #[serde(default)]
    pass: String,
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

fn load_cfg() -> Result<ServerCfg> {
    let path = project_root().join("temp").join("bi-server.json");
    let mut cfg: ServerCfg = if path.exists() { serde_json::from_str(&std::fs::read_to_string(&path)?).with_context(|| format!("parsing {}", path.display()))? } else { ServerCfg::default() };
    if let Ok(v) = std::env::var("BI_URL") {
        cfg.url = v;
    }
    if let Ok(v) = std::env::var("BI_USER") {
        cfg.user = v;
    }
    if let Ok(v) = std::env::var("BI_PASS") {
        cfg.pass = v;
    }
    if cfg.url.is_empty() {
        bail!("No server configured. Create {} or set BI_URL/BI_USER/BI_PASS.", path.display());
    }
    Ok(cfg)
}

fn profile_args(mode: &str, native: (i32, i32)) -> String {
    let codecs = SupportedCodecs::default();
    match mode {
        "h264" => {
            let p = profile::native_passthrough_profile();
            p.url_args(p.size_to_request(native, None), SupportedCodecs { h264: true, h265: false }, None)
        }
        "h265pref" => {
            let mut p = profile::native_passthrough_profile();
            p.codec_pref = profile::CodecPref::H265;
            p.url_args(p.size_to_request(native, None), codecs, None)
        }
        "default" => {
            let p = profile::default_profiles(false).into_iter().find(|p| p.name == "1080p VBR^").unwrap();
            p.url_args(p.size_to_request(native, None), codecs, None)
        }
        _ => {
            let p = profile::native_passthrough_profile();
            p.url_args(p.size_to_request(native, None), codecs, None)
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("info");
    let cfg = load_cfg()?;
    let client = BiClient::new(&cfg.url, Credentials { user: cfg.user.clone(), pass: cfg.pass.clone() })?;
    let s = client.login().await?;
    println!(
        "Logged in to \"{}\" as {} (admin={}) — Blue Iris {} — tz offset {} ms",
        s.login.system_name,
        if s.login.user.is_empty() { "?" } else { &s.login.user },
        s.login.admin,
        s.login.version,
        s.login.tz_offset_ms()
    );
    for (i, st) in s.login.server_streams().iter().enumerate() {
        println!("  server stream {i}: {}", st.text);
    }
    let cams = client.camlist(false).await?;

    match cmd {
        "info" => {
            println!("{} camlist entries:", cams.len());
            for c in &cams {
                println!(
                    "  {:<16} {:<28} {}x{} sub {}x{} fps={:.1} {}{}{}{}",
                    c.id,
                    c.name,
                    c.width,
                    c.height,
                    c.width2,
                    c.height2,
                    c.fps,
                    if c.is_group() { "[group] " } else { "" },
                    if c.ptz { "[ptz] " } else { "" },
                    if c.audio { "[audio] " } else { "" },
                    if !c.is_enabled { "[disabled]" } else { "" },
                );
            }
            let st = client.status(None).await?;
            println!("status: cpu={} mem={} uptime={} profile={} signal={}", st.cpu, st.mem, st.uptime, st.profile, st.signal);
        }
        "timeline" => {
            let cam = args.get(1).context("missing camera")?;
            let start: i64 = args.get(2).context("missing start")?.parse()?;
            let end: i64 = args.get(3).context("missing end")?.parse()?;
            let msecpp: i64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(8192);
            let d = client.timeline(cam, start * 1000, end * 1000, msecpp).await?;
            let tz = s.login.tz_offset_ms();
            let fmt = |ms: i64| chrono::DateTime::from_timestamp_millis(ms + tz).map(|d| d.naive_utc().format("%m-%d %H:%M:%S").to_string()).unwrap_or_default();
            println!("{} clips, {} alerts, colors={:?}", d.clips.len(), d.alerts.len(), d.colors);
            for c in &d.clips {
                let (a, b) = (start * 1000 + c.x1 * msecpp, start * 1000 + c.x2 * msecpp);
                println!("  clip track {} x{}..{}  {} -> {}  extra={:?}", c.track, c.x1, c.x2, fmt(a), fmt(b), c.extra);
            }
            let extra: Vec<&String> = d.extra.keys().collect();
            println!("other keys: {extra:?}");
        }
        "cliplist" => {
            let cam = args.get(1).context("missing camera")?.clone();
            let start: i64 = args.get(2).context("missing start")?.parse()?;
            let end: i64 = args.get(3).context("missing end")?.parse()?;
            let q = bi_proto::commands::ClipQuery { camera: cam, start, end, ..Default::default() };
            let mut clips = client.clip_list(&q, |_| {}).await?;
            clips.sort_by_key(|c| c.date);
            let tz = s.login.tz_offset_ms();
            let fmt = |ms: i64| chrono::DateTime::from_timestamp_millis(ms + tz).map(|d| d.naive_utc().format("%m-%d %H:%M:%S").to_string()).unwrap_or_default();
            println!("{} items", clips.len());
            for c in &clips {
                let msec = c.msec.unwrap_or(0.0) as i64;
                println!("  {} -> {}  {:>7.1}s  {} {} flags={:#x} {}", fmt(c.date * 1000), fmt(c.date * 1000 + msec), msec as f64 / 1000.0, c.path, c.filetype, c.flags, c.camera);
            }
        }
        "live" | "clip" | "raw" => {
            let target = args.get(1).context("missing camera / clip path")?.clone();
            let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
            let mode = args.get(3).map(String::as_str).unwrap_or("native");
            let native = cams.iter().find(|c| c.id.eq_ignore_ascii_case(&target)).map(|c| (c.width as i32, c.height as i32)).unwrap_or((3840, 2160));
            let pargs = profile_args(mode, native);
            let path = match cmd {
                "live" => urls::live_video(&target, AudioRequest::MuLaw, &pargs, ""),
                "clip" => urls::clip_video(&target, ClipStart::Pos(0), 100, AudioRequest::MuLaw, &pargs, "", ""),
                _ => target.clone(),
            };
            capture(&client, &path, &target, mode, secs).await?;
        }
        other => bail!("unknown command {other}"),
    }
    let _ = client.logout().await;
    Ok(())
}

async fn capture(client: &BiClient, path: &str, label: &str, mode: &str, secs: u64) -> Result<()> {
    println!("GET {path}");
    let resp = client.open_stream(path).await?;
    println!("HTTP {} content-type={:?}", resp.status(), resp.headers().get("content-type"));
    for h in ["x-camlist", "x-reclist", "x-utc"] {
        if let Some(v) = resp.headers().get(h) {
            println!("  {h}: {}", String::from_utf8_lossy(v.as_bytes()));
        }
    }
    let dir = project_root().join("temp").join("captures");
    std::fs::create_dir_all(&dir)?;
    let safe: String = label.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let out_path = dir.join(format!("{safe}-{mode}.bistream"));
    let mut out = std::fs::File::create(&out_path)?;
    let mut parser = StreamParser::new(1.0);
    let mut body = resp.bytes_stream();
    let start = Instant::now();
    let (mut frames, mut keys, mut vbytes, mut audio_frames, mut status_blocks) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut last_report = Instant::now();
    let mut first_time = None;
    let mut last_time = 0i64;
    while start.elapsed() < Duration::from_secs(secs) {
        let chunk = match tokio::time::timeout(Duration::from_secs(15), body.next()).await {
            Err(_) => {
                println!("idle timeout");
                break;
            }
            Ok(None) => {
                println!("stream ended (EOF)");
                break;
            }
            Ok(Some(c)) => c?,
        };
        out.write_all(&chunk)?;
        parser.push(&chunk);
        while let Some(ev) = parser.next_event()? {
            match ev {
                StreamEvent::Header { streams, video, audio } => {
                    println!("stream header: streams={streams}");
                    if let Some(v) = video {
                        println!(
                            "  video: biCompression=\"{}\" -> {} {}x{}",
                            String::from_utf8_lossy(&v.compression),
                            v.codec().name(),
                            v.width,
                            v.height
                        );
                    }
                    if let Some(a) = audio {
                        println!("  audio: tag=0x{:04X} ch={} rate={} bits={}", a.format_tag, a.channels, a.samples_per_sec, a.bits_per_sample);
                    }
                }
                StreamEvent::Video(f) => {
                    frames += 1;
                    vbytes += f.data.len() as u64;
                    if f.keyframe {
                        keys += 1;
                        if keys <= 2 {
                            let nals: Vec<u8> = bi_proto::stream_parser::nal_types(&f.data, f.codec == bi_proto::VideoCodec::H265).collect();
                            println!("  keyframe #{keys}: {} bytes, NAL types {:?}, utc={}", f.data.len(), nals, f.utc_ms);
                        }
                    }
                    first_time.get_or_insert(f.time_ms);
                    last_time = f.time_ms;
                }
                StreamEvent::Audio(_) => audio_frames += 1,
                StreamEvent::Status(s) => {
                    status_blocks += 1;
                    if status_blocks == 1 {
                        println!("  status: {s:?}");
                    }
                }
                StreamEvent::End => println!("natural end of stream"),
            }
        }
        if last_report.elapsed() > Duration::from_secs(2) {
            last_report = Instant::now();
            println!("[{:>3}s/{secs}s] frames={frames} keyframes={keys} audio={audio_frames} {:.2} Mbps", start.elapsed().as_secs(), vbytes as f64 * 8.0 / start.elapsed().as_secs_f64() / 1e6);
        }
    }
    let span = (last_time - first_time.unwrap_or(0)) as f64 / 1000.0;
    println!(
        "done: {frames} video frames ({keys} keyframes) over {span:.1}s of stream time => {:.1} fps, {:.2} Mbps; {audio_frames} audio blocks; {status_blocks} status blocks",
        if span > 0.0 { frames as f64 / span } else { 0.0 },
        vbytes as f64 * 8.0 / start.elapsed().as_secs_f64() / 1e6
    );
    println!("saved {}", out_path.display());
    Ok(())
}
