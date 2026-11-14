//! Fast scrubbing frame loader.
//!
//! Port of the seek-bar `loadSeekHintImg` / `queuedMsec` logic (ui3.js `SeekBar`, 12449) and the timeline's
//! `downloadSeekPreview` + `timelineSync` single lock (ui3.js:10456, 11130): exactly one JPEG request is in flight;
//! while it loads only the newest requested position is remembered, intermediate positions are skipped.
//! Native improvement: decoded frames are kept in an LRU cache (bounded by a memory budget) so re-scrubbing a
//! region is instant.

use crate::runtime::{self, Pending};
use anyhow::Result;
use bi_media::jpeg::{RgbaImage, decode_jpeg_rgba};
use bi_proto::BiClient;
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
    /// Bytes of decoded pixels currently cached, and the most that may be.
    cache_bytes: usize,
    budget_bytes: usize,
    pub last_error: Option<String>,
}

impl ScrubLoader {
    /// `budget_mb`: memory the cache of decoded frames may use. A 4K frame is about 33 MiB, so the cache is
    /// bounded by size rather than by frame count.
    pub fn new(budget_mb: usize) -> Self {
        ScrubLoader { inflight: None, queued: None, cache: lru::LruCache::unbounded(), cache_bytes: 0, budget_bytes: budget_mb.max(1) * 1024 * 1024, last_error: None }
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.cache_bytes = 0;
    }

    /// Caches a frame, evicting the least recently used ones while over budget (the newest frame always stays).
    fn cache_put(&mut self, key: String, img: Arc<RgbaImage>, utc: Option<i64>) {
        self.cache_bytes += img.rgba.len();
        if let Some((old, _)) = self.cache.put(key, (img, utc)) {
            self.cache_bytes -= old.rgba.len();
        }
        while self.cache_bytes > self.budget_bytes && self.cache.len() > 1 {
            match self.cache.pop_lru() {
                Some((_, (old, _))) => self.cache_bytes -= old.rgba.len(),
                None => break,
            }
        }
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
                self.cache_put(req.path.clone(), img.clone(), utc);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(bytes: usize) -> Arc<RgbaImage> {
        Arc::new(RgbaImage { width: 1, height: 1, rgba: vec![0; bytes] })
    }

    #[test]
    fn cache_is_bounded_by_bytes() {
        let mut l = ScrubLoader::new(1); // 1 MiB
        let third = 400 * 1024;
        l.cache_put("a".into(), frame(third), None);
        l.cache_put("b".into(), frame(third), None);
        assert_eq!(l.cache.len(), 2);
        // Touch "a" so "b" is the least recently used, then go over budget.
        assert!(l.cache.get("a").is_some());
        l.cache_put("c".into(), frame(third), None);
        assert!(l.cache.contains("a") && l.cache.contains("c") && !l.cache.contains("b"));
        assert_eq!(l.cache_bytes, 2 * third);
        // Replacing a key does not count its bytes twice.
        l.cache_put("c".into(), frame(third), None);
        assert_eq!(l.cache_bytes, 2 * third);
        // A single frame larger than the whole budget is still kept (it is the one on screen).
        l.cache_put("huge".into(), frame(4 * 1024 * 1024), None);
        assert_eq!(l.cache.len(), 1);
        assert_eq!(l.cache_bytes, 4 * 1024 * 1024);
        l.clear_cache();
        assert_eq!((l.cache.len(), l.cache_bytes), (0, 0));
    }
}
