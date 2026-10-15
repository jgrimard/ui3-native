//! Fast scrubbing frame loader.
//!
//! Port of the seek-bar `loadSeekHintImg` / `queuedMsec` logic (ui3.js `SeekBar`, 12449) and the timeline's
//! `downloadSeekPreview` + `timelineSync` single lock (ui3.js:10456, 11130): exactly one JPEG request is in flight;
//! while it loads only the newest requested position is remembered, intermediate positions are skipped.
//! Native improvement: decoded frames are kept in an LRU cache so re-scrubbing a region is instant.

use crate::runtime::{self, Pending};
use anyhow::Result;
use bi_media::jpeg::{RgbaImage, decode_jpeg_rgba};
use bi_proto::BiClient;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
pub struct ScrubRequest {
    /// Relative request path (built with `bi_proto::urls`).
    pub path: String,
    /// Caller-defined position tag (clip ms or timeline utc).
    pub tag: i64,
}

#[derive(Clone)]
pub struct ScrubFrame {
    pub tag: i64,
    pub image: Arc<RgbaImage>,
    /// `X-UTC` header (timeline frames).
    pub utc: Option<i64>,
    pub latency_ms: f64,
}

type Job = Pending<Result<(Arc<RgbaImage>, Option<i64>)>>;

pub struct ScrubLoader {
    inflight: Option<(ScrubRequest, Job, Instant)>,
    queued: Option<ScrubRequest>,
    cache: lru::LruCache<String, (Arc<RgbaImage>, Option<i64>)>,
    pub last_error: Option<String>,
}

impl ScrubLoader {
    pub fn new(cache_frames: usize) -> Self {
        ScrubLoader { inflight: None, queued: None, cache: lru::LruCache::new(NonZeroUsize::new(cache_frames.max(1)).unwrap()), last_error: None }
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// Requests a frame. Returns it immediately if cached.
    pub fn request(&mut self, ctx: &egui::Context, client: &BiClient, req: ScrubRequest) -> Option<ScrubFrame> {
        if let Some((img, utc)) = self.cache.get(&req.path) {
            return Some(ScrubFrame { tag: req.tag, image: img.clone(), utc: *utc, latency_ms: 0.0 });
        }
        if self.inflight.as_ref().is_some_and(|(r, _, _)| r.path == req.path) {
            self.queued = None;
            return None;
        }
        if self.inflight.is_some() {
            self.queued = Some(req);
        } else {
            self.start(ctx, client, req);
        }
        None
    }

    fn start(&mut self, ctx: &egui::Context, client: &BiClient, req: ScrubRequest) {
        let c = client.clone();
        let path = req.path.clone();
        let job = runtime::spawn(ctx, async move {
            let r = c.get_bytes(&path).await?;
            anyhow::ensure!(r.status < 400, "HTTP {}", r.status);
            anyhow::ensure!(r.is_jpeg(), "server did not return a JPEG ({} bytes)", r.body.len());
            let utc = r.header("x-utc").and_then(|v| v.trim().parse().ok());
            let body = r.body;
            let img = tokio::task::spawn_blocking(move || decode_jpeg_rgba(&body)).await??;
            Ok((Arc::new(img), utc))
        });
        self.inflight = Some((req, job, Instant::now()));
    }

    /// Call every frame. Returns a newly loaded frame, and starts the queued request if any.
    pub fn poll(&mut self, ctx: &egui::Context, client: &BiClient) -> Option<ScrubFrame> {
        let (req, job, started) = self.inflight.as_mut()?;
        let res = job.poll()?;
        let req = req.clone();
        let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.inflight = None;
        let out = match res {
            Ok((img, utc)) => {
                self.last_error = None;
                self.cache.put(req.path.clone(), (img.clone(), utc));
                Some(ScrubFrame { tag: req.tag, image: img, utc, latency_ms })
            }
            Err(e) => {
                self.last_error = Some(format!("{e:#}"));
                None
            }
        };
        if let Some(q) = self.queued.take() {
            if let Some(hit) = self.cache.get(&q.path).cloned() {
                return Some(ScrubFrame { tag: q.tag, image: hit.0, utc: hit.1, latency_ms: 0.0 });
            }
            self.start(ctx, client, q);
        }
        out
    }

    /// Drops any queued request (e.g. on mouse release).
    pub fn cancel_queued(&mut self) {
        self.queued = None;
    }
}
