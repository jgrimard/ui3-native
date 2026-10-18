//! HTTP client and session handling. Port of `SessionManager` login logic (ui3.js:17032, 17361) and `ExecJSON` (ui3.js:35555).

use crate::api::{JsonResponse, LoginData};
use anyhow::{Context, Result, anyhow, bail};
use md5::{Digest, Md5};
use parking_lot::RwLock;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct Credentials {
    pub user: String,
    pub pass: String,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub login: LoginData,
}

struct Inner {
    base: String,
    http: reqwest::Client,
    /// Separate client without any timeout for long-lived video streams.
    stream_http: reqwest::Client,
    creds: RwLock<Credentials>,
    session: RwLock<Option<Session>>,
    /// Distinguishes this app instance's /time/ streams when sharing a session (UI3 `ui3InstanceId`).
    opaque: String,
    /// Opaque for timeline preview JPEGs. Blue Iris answers a JPEG request on an opaque that has a timeline video
    /// session with that session's current frame (ignoring `pos`, while `X-UTC` echoes the requested time).
    preview_opaque: String,
    /// Counter for [`BiClient::new_stream_opaque`].
    stream_seq: AtomicU32,
}

/// Cheap to clone, thread-safe Blue Iris client.
#[derive(Clone)]
pub struct BiClient {
    inner: Arc<Inner>,
}

pub fn login_response_hash(user: &str, session: &str, pass: &str) -> String {
    let mut h = Md5::new();
    h.update(format!("{user}:{session}:{pass}").as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Normalizes "192.168.1.5:81" / "http://host:81" / "http://host:81/ui3.htm" into "http://host:81/".
pub fn normalize_base_url(s: &str) -> String {
    let mut s = s.trim().to_string();
    if !s.contains("://") {
        s = format!("http://{s}");
    }
    if let Some(i) = s.find("://").map(|i| i + 3)
        && let Some(slash) = s[i..].find('/')
    {
        let path = &s[i + slash..];
        if path.ends_with(".htm") || path.ends_with(".html") {
            s.truncate(i + slash + 1);
        }
    }
    if !s.ends_with('/') {
        s.push('/');
    }
    s
}

impl BiClient {
    pub fn new(base_url: &str, creds: Credentials) -> Result<Self> {
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()?;
        let stream_http = reqwest::Client::builder().cookie_store(true).connect_timeout(Duration::from_secs(10)).tcp_nodelay(true).build()?;
        let opaque = format!("{:08x}", rand_u32());
        let preview_opaque = format!("{opaque}p");
        Ok(BiClient {
            inner: Arc::new(Inner {
                base: normalize_base_url(base_url),
                http,
                stream_http,
                creds: RwLock::new(creds),
                session: RwLock::new(None),
                opaque,
                preview_opaque,
                stream_seq: AtomicU32::new(0),
            }),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.inner.base
    }
    pub fn opaque(&self) -> &str {
        &self.inner.opaque
    }
    pub fn preview_opaque(&self) -> &str {
        &self.inner.preview_opaque
    }
    /// A new opaque for each timeline video stream. Reopening on an opaque Blue Iris has used makes it send a burst
    /// of frames from that session's previous position first, stamped with the new time.
    pub fn new_stream_opaque(&self) -> String {
        format!("{}s{}", self.inner.opaque, self.inner.stream_seq.fetch_add(1, Ordering::Relaxed))
    }
    pub fn session(&self) -> Option<Session> {
        self.inner.session.read().clone()
    }
    pub fn session_id(&self) -> Option<String> {
        self.inner.session.read().as_ref().map(|s| s.id.clone())
    }
    pub fn set_credentials(&self, c: Credentials) {
        *self.inner.creds.write() = c;
    }

    /// Absolute URL for a relative request path, with `session=` appended (UI3 `GetAPISessionArg(.., true)`).
    pub fn url(&self, path: &str) -> String {
        let mut u = format!("{}{}", self.inner.base, path.trim_start_matches('/'));
        if let Some(id) = self.session_id() {
            u.push(if u.contains('?') { '&' } else { '?' });
            u.push_str("session=");
            u.push_str(&id);
        }
        u
    }

    async fn post_json(&self, args: &Value) -> Result<JsonResponse> {
        let cmd = args.get("cmd").and_then(|c| c.as_str()).unwrap_or("");
        let url = format!("{}json?_{}", self.inner.base, cmd);
        let resp = self
            .inner
            .http
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .body(serde_json::to_string(args)?)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            bail!("HTTP {status} from /json ({cmd}): {}", truncate(&text, 200));
        }
        serde_json::from_str(&text).with_context(|| format!("bad JSON from /json ({cmd}): {}", truncate(&text, 200)))
    }

    /// Two-step challenge/response login.
    pub async fn login(&self) -> Result<Session> {
        let creds = self.inner.creds.read().clone();
        let r1 = self.post_json(&json!({"cmd": "login"})).await?;
        let sid = r1.session.clone().ok_or_else(|| anyhow!("login: server did not return a session id"))?;
        if r1.is_success() {
            // Already authenticated (e.g. auth-exempt LAN); use as-is.
            let login: LoginData = serde_json::from_value(r1.data.clone()).unwrap_or_default();
            let s = Session { id: sid, login };
            *self.inner.session.write() = Some(s.clone());
            return Ok(s);
        }
        if r1.fail_reason() != Some("missing response") {
            bail!("login: unexpected challenge response: {}", serde_json::to_string(&r1)?);
        }
        let auth_exempt = r1.data.get("auth-exempt").is_some_and(crate::lenient::value_to_bool);
        let response = if !creds.user.is_empty() {
            login_response_hash(&creds.user, &sid, &creds.pass)
        } else if auth_exempt {
            login_response_hash("Anonymous", &sid, "")
        } else {
            bail!("login: credentials required");
        };
        let r2 = self.post_json(&json!({"cmd": "login", "session": sid, "response": response})).await?;
        if !r2.is_success() {
            bail!("Failed to log in. {}", r2.fail_reason().unwrap_or("unknown reason"));
        }
        let login: LoginData = serde_json::from_value(r2.data.clone()).context("parsing login data")?;
        let s = Session { id: r2.session.unwrap_or(sid), login };
        *self.inner.session.write() = Some(s.clone());
        Ok(s)
    }

    pub async fn logout(&self) -> Result<()> {
        if self.session_id().is_some() {
            let _ = self.json(json!({"cmd": "logout"})).await;
        }
        *self.inner.session.write() = None;
        Ok(())
    }

    /// Executes a JSON command with the session attached; on "Invalid session" logs in again once and retries.
    pub async fn json(&self, mut args: Value) -> Result<JsonResponse> {
        for attempt in 0..2 {
            if self.session_id().is_none() {
                self.login().await?;
            }
            if let Some(obj) = args.as_object_mut() {
                obj.insert("session".into(), Value::String(self.session_id().unwrap_or_default()));
            }
            let r = self.post_json(&args).await?;
            if r.is_invalid_session() && attempt == 0 {
                tracing::warn!("Blue Iris session expired; re-establishing");
                *self.inner.session.write() = None;
                continue;
            }
            return Ok(r);
        }
        unreachable!()
    }

    /// Like [`Self::json`] but fails unless `result == "success"`, returning `data`.
    pub async fn json_ok(&self, args: Value) -> Result<Value> {
        let cmd = args.get("cmd").and_then(|c| c.as_str()).unwrap_or("").to_string();
        let r = self.json(args).await?;
        if !r.is_success() {
            bail!("{cmd} failed: {}", r.fail_reason().map(str::to_string).unwrap_or_else(|| r.data.to_string()));
        }
        Ok(r.data)
    }

    /// GET returning bytes plus selected headers (used for JPEG frames, thumbnails, /time/set).
    pub async fn get_bytes(&self, path: &str) -> Result<HttpBytes> {
        let url = self.url(path);
        let resp = self.inner.http.get(&url).send().await.with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if status.as_u16() == 403 {
            *self.inner.session.write() = None;
        }
        let headers = resp.headers().clone();
        let body = resp.bytes().await?;
        Ok(HttpBytes { status: status.as_u16(), headers, body })
    }

    /// Opens a long-lived streaming GET (no total timeout).
    pub async fn open_stream(&self, path: &str) -> Result<reqwest::Response> {
        // The request path without the session argument, so stream parameters can be inspected in the log.
        tracing::info!("open stream: {path}");
        let url = self.url(path);
        let resp = self.inner.stream_http.get(&url).send().await.with_context(|| format!("GET {url}"))?;
        if resp.status().as_u16() == 403 {
            *self.inner.session.write() = None;
        }
        Ok(resp)
    }
}

pub struct HttpBytes {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub body: bytes::Bytes,
}

impl HttpBytes {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
    pub fn is_jpeg(&self) -> bool {
        self.header("content-type").is_some_and(|c| c.starts_with("image/jpeg")) || self.body.starts_with(&[0xFF, 0xD8])
    }
}

/// True when `e` came from failing to reach the server (connect, reset, timeout) rather than from a reply it sent.
pub fn is_unreachable(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<reqwest::Error>().is_some_and(|r| r.is_connect() || r.is_request() || r.is_timeout() || r.is_body()))
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn rand_u32() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.finish() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_hash() {
        assert_eq!(login_response_hash("admin", "ABC", "pass"), "8e4b3dc21a937804eeca74d807d7174b");
        assert_eq!(login_response_hash("Anonymous", "XYZ", ""), "2471bc1f6bc9f87744cfb39da96af584");
    }

    #[test]
    fn base_urls() {
        assert_eq!(normalize_base_url("192.168.1.5:81"), "http://192.168.1.5:81/");
        assert_eq!(normalize_base_url("http://host:81/ui3.htm"), "http://host:81/");
        assert_eq!(normalize_base_url("https://bi.example.com/"), "https://bi.example.com/");
    }
}
