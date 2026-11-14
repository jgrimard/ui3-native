//! Client-side export: stream H.264/H.265 video from Blue Iris and remux it to MP4 (video only, no audio).

use anyhow::{Result, bail};
use bi_media::mux::Mp4Muxer;
use bi_proto::BiClient;
use bi_proto::stream_parser::{StreamEvent, StreamParser};
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

/// Longest gap between chunks before the export gives up on the server.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

struct Frame {
    data: bytes::Bytes,
    time_ms: i64,
    keyframe: bool,
    codec: bi_proto::VideoCodec,
    w: i32,
    h: i32,
}

/// Streams `path` into an MP4 at `dest`. `progress` is 0..=1000; setting `cancel` stops the export.
/// A failed or cancelled export removes the partial file.
pub async fn export_stream_to_mp4(client: BiClient, path: String, dest: PathBuf, duration_ms: i64, progress: Arc<AtomicU32>, cancel: Arc<AtomicBool>) -> Result<String> {
    let r = run(&client, &path, &dest, duration_ms, &progress, &cancel).await;
    match r {
        Ok(frames) => {
            progress.store(1000, Ordering::Relaxed);
            Ok(format!("Exported {frames} frames to {}", dest.display()))
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&dest).await;
            Err(e)
        }
    }
}

async fn run(client: &BiClient, path: &str, dest: &Path, duration_ms: i64, progress: &AtomicU32, cancel: &AtomicBool) -> Result<u64> {
    let resp = client.open_stream(path).await?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status());
    }
    // The muxer blocks on file I/O, so it runs on a blocking thread fed through a bounded channel.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Frame>(64);
    let dest2 = dest.to_path_buf();
    let writer = tokio::task::spawn_blocking(move || -> Result<u64> {
        let mut mux: Option<Mp4Muxer> = None;
        let mut n = 0u64;
        while let Some(f) = rx.blocking_recv() {
            if mux.is_none() {
                if !f.keyframe {
                    continue;
                }
                mux = Some(Mp4Muxer::create(&dest2, f.codec, f.w, f.h)?);
            }
            mux.as_mut().unwrap().write(&f.data, f.time_ms, f.keyframe, f.codec)?;
            n += 1;
        }
        match mux {
            Some(m) => {
                m.finish()?;
                Ok(n)
            }
            None => bail!("no video received"),
        }
    });
    let read = read_stream(resp, tx, duration_ms, progress, cancel).await;
    // The sender is gone now, so the writer finishes (or has already failed) and closes the file.
    let written = writer.await?;
    read?;
    written
}

async fn read_stream(resp: bi_proto::reqwest::Response, tx: tokio::sync::mpsc::Sender<Frame>, duration_ms: i64, progress: &AtomicU32, cancel: &AtomicBool) -> Result<()> {
    let mut parser = StreamParser::new(1.0);
    let mut body = resp.bytes_stream();
    let (mut w, mut h) = (0, 0);
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("Export cancelled");
        }
        let chunk = match tokio::time::timeout(IDLE_TIMEOUT, body.next()).await {
            Err(_) => bail!("the server stopped sending video"),
            Ok(None) => return Ok(()),
            Ok(Some(c)) => c.map_err(bi_proto::client::without_url)?,
        };
        parser.push(&chunk);
        while let Some(ev) = parser.next_event()? {
            match ev {
                StreamEvent::Header { video: Some(v), .. } => {
                    w = v.width;
                    h = v.height;
                }
                StreamEvent::Video(f) => {
                    if f.time_ms > duration_ms {
                        return Ok(());
                    }
                    progress.store(((f.time_ms as f64 / duration_ms.max(1) as f64) * 1000.0).clamp(0.0, 1000.0) as u32, Ordering::Relaxed);
                    // A closed channel means the writer failed; its error is reported by the caller.
                    if tx.send(Frame { data: f.data, time_ms: f.time_ms, keyframe: f.keyframe, codec: f.codec, w, h }).await.is_err() {
                        return Ok(());
                    }
                }
                StreamEvent::End => return Ok(()),
                _ => {}
            }
        }
    }
}
