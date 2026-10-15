//! Client-side export: stream the original (direct-to-wire) H.264/H.265 from Blue Iris and remux to MP4.

use anyhow::{Result, bail};
use bi_media::mux::Mp4Muxer;
use bi_proto::BiClient;
use bi_proto::stream_parser::{StreamEvent, StreamParser};
use futures_util::StreamExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

enum Msg {
    Frame { data: bytes::Bytes, time_ms: i64, keyframe: bool, codec: bi_proto::VideoCodec, w: i32, h: i32 },
}

pub async fn export_stream_to_mp4(client: BiClient, path: String, dest: PathBuf, duration_ms: i64, progress: Arc<AtomicU32>) -> Result<String> {
    let resp = client.open_stream(&path).await?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status());
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<Msg>(64);
    let dest2 = dest.clone();
    let writer = tokio::task::spawn_blocking(move || -> Result<u64> {
        let mut mux: Option<Mp4Muxer> = None;
        let mut n = 0u64;
        while let Ok(Msg::Frame { data, time_ms, keyframe, codec, w, h }) = rx.recv() {
            if mux.is_none() {
                if !keyframe {
                    continue;
                }
                mux = Some(Mp4Muxer::create(&dest2, codec, w, h)?);
            }
            mux.as_mut().unwrap().write(&data, time_ms, keyframe, codec)?;
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
    let mut parser = StreamParser::new(1.0);
    let mut body = resp.bytes_stream();
    let (mut w, mut h) = (0, 0);
    'outer: while let Some(chunk) = body.next().await {
        parser.push(&chunk?);
        while let Some(ev) = parser.next_event()? {
            match ev {
                StreamEvent::Header { video: Some(v), .. } => {
                    w = v.width;
                    h = v.height;
                }
                StreamEvent::Video(f) => {
                    if f.time_ms > duration_ms {
                        break 'outer;
                    }
                    progress.store(((f.time_ms as f64 / duration_ms.max(1) as f64) * 1000.0).clamp(0.0, 1000.0) as u32, Ordering::Relaxed);
                    if tx.send(Msg::Frame { data: f.data, time_ms: f.time_ms, keyframe: f.keyframe, codec: f.codec, w, h }).is_err() {
                        break 'outer;
                    }
                }
                StreamEvent::End => break 'outer,
                _ => {}
            }
        }
    }
    drop(tx);
    let frames = writer.await??;
    progress.store(1000, Ordering::Relaxed);
    Ok(format!("Exported {frames} frames to {}", dest.display()))
}
