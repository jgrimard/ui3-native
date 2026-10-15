//! Async streaming reader for `/video/`, `/file/clips/` and `/time/`. Port of the fetch/pump logic of
//! `FetchVideoH264Streamer` (ui3.js:36929-37323) including the JPEG short-circuit and error mapping.

use crate::client::BiClient;
use crate::stream_parser::{StreamEvent, StreamParser};
use bytes::Bytes;
use futures_util::StreamExt;
use std::time::Duration;
use tokio::task::JoinHandle;

/// Response headers relevant to the player.
#[derive(Debug, Clone, Default)]
pub struct StreamHeaders {
    pub content_type: String,
    /// Camera ids within a group image, `X-CAMLIST` (ui3.js:19370).
    pub camlist: Vec<String>,
    /// Rectangles `[l,t,r,b]` per camera, `X-RECLIST`.
    pub reclist: Vec<[i32; 4]>,
    /// Frame time of a single-JPEG response (`/time/?jpeg`), `X-UTC`.
    pub utc: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum StreamMessage {
    Headers(StreamHeaders),
    Event(StreamEvent),
    /// The server answered with a single JPEG (e.g. `speed=0`).
    Jpeg { data: Bytes, utc: Option<i64> },
    Ended(EndReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// Block type 4 or a completed JPEG.
    Natural,
    /// Body ended without a type-4 block.
    Eof,
    SessionLost,
    Unavailable,
    Http(u16),
    Protocol(String),
    Network(String),
    IdleTimeout,
}

/// Dropping or calling [`StreamHandle::stop`] cancels the stream.
pub struct StreamHandle {
    task: Option<JoinHandle<()>>,
}

impl StreamHandle {
    pub fn stop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(|t| t.is_finished())
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct StreamOptions {
    pub timestamp_scale: f64,
    pub idle_timeout: Duration,
}

impl Default for StreamOptions {
    fn default() -> Self {
        StreamOptions { timestamp_scale: 1.0, idle_timeout: Duration::from_secs(20) }
    }
}

enum Sink<F> {
    Callback(F),
    Channel(tokio::sync::mpsc::Sender<StreamMessage>),
}

impl<F: FnMut(StreamMessage) -> bool> Sink<F> {
    /// Returns false when the consumer is gone.
    async fn send(&mut self, m: StreamMessage) -> bool {
        match self {
            Sink::Callback(f) => f(m),
            Sink::Channel(tx) => tx.send(m).await.is_ok(),
        }
    }
}

/// Spawns the stream on the given runtime. `sink` returns `false` to stop reading.
pub fn spawn_stream<F>(rt: &tokio::runtime::Handle, client: BiClient, path: String, opts: StreamOptions, sink: F) -> StreamHandle
where
    F: FnMut(StreamMessage) -> bool + Send + 'static,
{
    let task = rt.spawn(async move {
        let mut sink = Sink::Callback(sink);
        if let Some(r) = run(&client, &path, &opts, &mut sink).await {
            sink.send(StreamMessage::Ended(r)).await;
        }
    });
    StreamHandle { task: Some(task) }
}

/// Spawns the stream and returns a bounded channel of messages. The reader awaits channel capacity, so a slow
/// consumer (e.g. a decoder pacing recorded playback) applies backpressure all the way to the TCP socket.
pub fn spawn_stream_channel(
    rt: &tokio::runtime::Handle,
    client: BiClient,
    path: String,
    opts: StreamOptions,
    capacity: usize,
) -> (StreamHandle, tokio::sync::mpsc::Receiver<StreamMessage>) {
    let (tx, rx) = tokio::sync::mpsc::channel(capacity.max(1));
    let task = rt.spawn(async move {
        let mut sink: Sink<fn(StreamMessage) -> bool> = Sink::Channel(tx);
        if let Some(r) = run(&client, &path, &opts, &mut sink).await {
            sink.send(StreamMessage::Ended(r)).await;
        }
    });
    (StreamHandle { task: Some(task) }, rx)
}

/// `X-CAMLIST` is a JSON list body without brackets: `"cam1","cam2"` (ui3.js:19377).
fn parse_list(v: &str) -> Vec<String> {
    let inner = v.trim().trim_start_matches('[').trim_end_matches(']');
    serde_json::from_str::<Vec<String>>(&format!("[{inner}]")).unwrap_or_else(|_| {
        inner.split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect()
    })
}

/// `X-RECLIST` is `[l,t,r,b],[l,t,r,b]`.
fn parse_rects(v: &str) -> Vec<[i32; 4]> {
    let nums: Vec<i32> = v.split(|c: char| !(c.is_ascii_digit() || c == '-')).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect();
    nums.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect()
}

async fn run<F>(client: &BiClient, path: &str, opts: &StreamOptions, sink: &mut Sink<F>) -> Option<EndReason>
where
    F: FnMut(StreamMessage) -> bool,
{
    let resp = match client.open_stream(path).await {
        Ok(r) => r,
        Err(e) => return Some(EndReason::Network(format!("{e:#}"))),
    };
    let status = resp.status().as_u16();
    match status {
        403 => return Some(EndReason::SessionLost),
        503 => return Some(EndReason::Unavailable),
        _ => {}
    }
    let h = resp.headers();
    let get = |n: &str| h.get(n).map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
    let headers = StreamHeaders {
        content_type: get("content-type").unwrap_or_default(),
        camlist: get("x-camlist").map(|v| parse_list(&v)).unwrap_or_default(),
        reclist: get("x-reclist").map(|v| parse_rects(&v)).unwrap_or_default(),
        utc: get("x-utc").and_then(|v| v.trim().parse().ok()),
    };
    let is_jpeg = headers.content_type.starts_with("image/jpeg");
    let utc = headers.utc;
    if !sink.send(StreamMessage::Headers(headers)).await {
        return None;
    }
    if is_jpeg {
        return match resp.bytes().await {
            Ok(b) => {
                sink.send(StreamMessage::Jpeg { data: b, utc }).await;
                Some(EndReason::Natural)
            }
            Err(e) => Some(EndReason::Network(e.to_string())),
        };
    }
    if !(200..300).contains(&status) {
        return Some(EndReason::Http(status));
    }
    let mut parser = StreamParser::new(opts.timestamp_scale);
    let mut body = resp.bytes_stream();
    loop {
        let chunk = match tokio::time::timeout(opts.idle_timeout, body.next()).await {
            Err(_) => return Some(EndReason::IdleTimeout),
            Ok(None) => return Some(EndReason::Eof),
            Ok(Some(Err(e))) => return Some(EndReason::Network(e.to_string())),
            Ok(Some(Ok(c))) => c,
        };
        parser.push(&chunk);
        loop {
            match parser.next_event() {
                Ok(Some(ev)) => {
                    let end = matches!(ev, StreamEvent::End);
                    if !sink.send(StreamMessage::Event(ev)).await {
                        return None;
                    }
                    if end {
                        return Some(EndReason::Natural);
                    }
                }
                Ok(None) => break,
                Err(e) => return Some(EndReason::Protocol(e.to_string())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_lists() {
        assert_eq!(parse_list(r#""cam1","cam2","cam 3""#), vec!["cam1", "cam2", "cam 3"]);
        assert_eq!(parse_rects("[0,0,640,360],[640,0,1280,360]"), vec![[0, 0, 640, 360], [640, 0, 1280, 360]]);
    }
}
