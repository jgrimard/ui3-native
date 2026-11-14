//! MQTT remote control. Port of `MqttClient` (ui3.js:41483).
//!
//! Subscribes to `ui3/<instance>/#` and `ui3/global/#`:
//! * `ui3/<id>/state/vid` (camera id), `state/maximize` ("1"/"0"), `state/volume` (0-100)
//! * `ui3/<id|global>/playaudio[/<player>]` (sound file name), `playtts` (text), `toast[/<id>]` (JSON or text)
//!
//! Publishes `ui3/<id>/state/{vid,maximize,volume}` when they change. UI3 used a WebSocket URL
//! (`ws://host:port/path`); natively `mqtt://host:1883` / `tcp://` also work, and `wss://` / `mqtts://` use TLS.

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, TlsConfiguration, Transport};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum MqttCommand {
    Video(String),
    Maximize(bool),
    Volume(f32),
    PlayAudio(String),
    Speak(String),
    Toast { kind: String, msg: String, timeout_ms: Option<u64> },
    /// Connection state changes, for `ui3_mqttStatusToasts`.
    Status { connected: bool, msg: String },
}

pub struct Mqtt {
    client: AsyncClient,
    rx: crossbeam_channel::Receiver<MqttCommand>,
    /// The event loop task; aborted on drop so an old client never reconnects in the background.
    task: tokio::task::JoinHandle<()>,
    instance: String,
    qos: QoS,
    retain: bool,
    publish_enabled: bool,
    published: HashMap<String, String>,
}

fn qos(n: f64) -> QoS {
    match n as i32 {
        1 => QoS::AtLeastOnce,
        2 => QoS::ExactlyOnce,
        _ => QoS::AtMostOnce,
    }
}

/// Where and how to connect, parsed from the broker URL setting.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Broker {
    Tcp { host: String, port: u16, tls: bool },
    /// WebSocket: rumqttc takes the whole URL.
    Ws { url: String, tls: bool },
}

fn parse_broker(url: &str) -> anyhow::Result<Broker> {
    let url = url.trim();
    anyhow::ensure!(!url.is_empty(), "MQTT broker URL is not set");
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => ("mqtt".to_string(), url),
    };
    let tls = match scheme.as_str() {
        "ws" => return Ok(Broker::Ws { url: url.to_string(), tls: false }),
        "wss" => return Ok(Broker::Ws { url: url.to_string(), tls: true }),
        "mqtt" | "tcp" => false,
        "mqtts" | "ssl" | "tls" => true,
        other => anyhow::bail!("unsupported MQTT URL scheme \"{other}://\" (use mqtt://, mqtts://, ws:// or wss://)"),
    };
    let default_port = if tls { 8883 } else { 1883 };
    // host[:port], ignoring any path. IPv6 literals are written [addr] or [addr]:port.
    let authority = rest.split('/').next().unwrap_or("");
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !authority.ends_with(']') => (h, p.parse::<u16>().map_err(|_| anyhow::anyhow!("invalid MQTT port \"{p}\""))?),
        _ => (authority, default_port),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    anyhow::ensure!(!host.is_empty(), "MQTT broker URL has no host");
    Ok(Broker::Tcp { host: host.to_string(), port, tls })
}

/// TLS settings for `mqtts://` and `wss://`: the broker's certificate is checked against the Windows root store.
fn tls_config() -> anyhow::Result<TlsConfiguration> {
    // rustls needs a process-wide crypto provider; installing it twice is harmless.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    anyhow::ensure!(!roots.is_empty(), "no trusted root certificates were found for the MQTT TLS connection");
    Ok(rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth().into())
}

impl Mqtt {
    /// Settings fingerprint so the client restarts when any MQTT setting changes.
    pub fn config_key(s: &crate::settings::Settings) -> String {
        let mut parts: Vec<String> = [
            "ui3_mqttClientEnabled",
            "ui3_mqttBrokerUrl",
            "ui3_mqttUser",
            "ui3_mqttInstanceId",
            "ui3_mqttSubscribeEnabled",
            "ui3_mqttSubscribeQOS",
            "ui3_mqttPublishEnabled",
            "ui3_mqttPublishQOS",
            "ui3_mqttPublishRetain",
            "ui3_mqttAudioEvents",
            "ui3_mqttToastEvents",
            "ui3_mqttStatusToasts",
        ]
        .iter()
        .map(|k| s.str(k))
        .collect();
        parts.push(s.secret("ui3_mqttPass"));
        parts.join("|")
    }

    pub fn start(s: &crate::settings::Settings, ctx: &egui::Context) -> anyhow::Result<Option<Mqtt>> {
        if !s.bool("ui3_mqttClientEnabled") {
            return Ok(None);
        }
        let broker = parse_broker(&s.str("ui3_mqttBrokerUrl"))?;
        let instance = {
            let i = s.str("ui3_mqttInstanceId");
            if i.trim().is_empty() { "ui3native".to_string() } else { i }
        };
        let client_id = format!("ui3n{:08x}", rand_id());
        let mut opts = match broker {
            Broker::Ws { url, tls } => {
                let mut o = MqttOptions::new(client_id, url, 0);
                o.set_transport(if tls { Transport::wss_with_config(tls_config()?) } else { Transport::Ws });
                o
            }
            Broker::Tcp { host, port, tls } => {
                let mut o = MqttOptions::new(client_id, host, port);
                if tls {
                    o.set_transport(Transport::tls_with_config(tls_config()?));
                }
                o
            }
        };
        opts.set_keep_alive(Duration::from_secs(30));
        let sub_qos = qos(s.f64("ui3_mqttSubscribeQOS"));
        let pub_qos = qos(s.f64("ui3_mqttPublishQOS"));
        // The client id is random per connection, so a persistent session could never be resumed.
        opts.set_clean_session(true);
        let user = s.str("ui3_mqttUser");
        if !user.is_empty() {
            opts.set_credentials(user, s.secret("ui3_mqttPass"));
        }
        let (client, mut eventloop) = AsyncClient::new(opts, 32);
        let (tx, rx) = crossbeam_channel::unbounded();
        let subscribe = s.bool("ui3_mqttSubscribeEnabled");
        let audio_events = s.bool("ui3_mqttAudioEvents");
        let toast_events = s.bool("ui3_mqttToastEvents");
        let status_toasts = s.bool("ui3_mqttStatusToasts");
        let inst = instance.clone();
        let c2 = client.clone();
        let ctx = ctx.clone();
        let task = crate::runtime::handle().spawn(async move {
            // Whether the last thing reported was a failure, so a broken connection is announced once.
            let mut failing = false;
            loop {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Packet::ConnAck(_))) => {
                        tracing::info!("MQTT connected");
                        failing = false;
                        if status_toasts {
                            let _ = tx.send(MqttCommand::Status { connected: true, msg: "MQTT connected".into() });
                            ctx.request_repaint();
                        }
                        if subscribe {
                            let _ = c2.subscribe(format!("ui3/{inst}/#"), sub_qos).await;
                            let _ = c2.subscribe("ui3/global/#", sub_qos).await;
                        }
                    }
                    Ok(Event::Incoming(Packet::Publish(p))) => {
                        if !subscribe {
                            continue;
                        }
                        let payload = String::from_utf8_lossy(&p.payload).to_string();
                        if let Some(cmd) = parse(&p.topic, &payload, &inst, audio_events, toast_events) {
                            let _ = tx.send(cmd);
                            ctx.request_repaint();
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        if !failing {
                            failing = true;
                            tracing::warn!("MQTT: {e}");
                            if status_toasts {
                                let _ = tx.send(MqttCommand::Status { connected: false, msg: format!("MQTT connection failed: {e}") });
                                ctx.request_repaint();
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });
        Ok(Some(Mqtt { client, rx, task, instance, qos: pub_qos, retain: s.bool("ui3_mqttPublishRetain"), publish_enabled: s.bool("ui3_mqttPublishEnabled"), published: HashMap::new() }))
    }

    pub fn try_recv(&self) -> Option<MqttCommand> {
        self.rx.try_recv().ok()
    }

    /// Publishes `ui3/<id>/state/<key>` if the value changed (`MqttTopicState`).
    pub fn publish_state(&mut self, key: &str, value: &str) {
        if !self.publish_enabled || self.published.get(key).is_some_and(|v| v == value) {
            return;
        }
        self.published.insert(key.to_string(), value.to_string());
        let (c, topic, v, q, r) = (self.client.clone(), format!("ui3/{}/state/{key}", self.instance), value.to_string(), self.qos, self.retain);
        crate::runtime::spawn_detached(async move {
            let _ = c.publish(topic, q, r, v).await;
        });
    }
}

impl Drop for Mqtt {
    fn drop(&mut self) {
        let c = self.client.clone();
        let task = self.task.abort_handle();
        crate::runtime::spawn_detached(async move {
            // Give the event loop a moment to send the DISCONNECT, then stop it for good: left running it
            // would reconnect and stay connected for the life of the process.
            let _ = tokio::time::timeout(Duration::from_millis(300), c.disconnect()).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            task.abort();
        });
    }
}

fn parse(topic: &str, payload: &str, inst: &str, audio: bool, toast: bool) -> Option<MqttCommand> {
    let parts: Vec<&str> = topic.split('/').collect();
    if parts.first() != Some(&"ui3") || !(parts.get(1) == Some(&inst) || parts.get(1) == Some(&"global")) {
        return None;
    }
    match (parts.len(), parts.get(2).copied()) {
        (4, Some("state")) if parts[1] == inst => match parts[3] {
            "vid" => Some(MqttCommand::Video(payload.to_string())),
            "maximize" => Some(MqttCommand::Maximize(payload == "1")),
            "volume" => payload.trim().parse::<f32>().ok().map(|v| MqttCommand::Volume((v / 100.0).clamp(0.0, 1.0))),
            _ => None,
        },
        (3 | 4, Some("playaudio")) if audio => Some(MqttCommand::PlayAudio(payload.to_string())),
        (3, Some("playtts")) => Some(MqttCommand::Speak(payload.to_string())),
        (3 | 4, Some("toast")) if toast => {
            let v: serde_json::Value = serde_json::from_str(payload).unwrap_or_else(|_| serde_json::Value::String(payload.to_string()));
            let (msg, kind, timeout) = match &v {
                serde_json::Value::String(s) => (s.clone(), "info".to_string(), None),
                o => (
                    o["msg"].as_str().unwrap_or("").to_string(),
                    o["type"].as_str().unwrap_or("info").to_ascii_lowercase(),
                    o["timeout"].as_f64().filter(|t| *t >= 0.0).map(|t| t as u64),
                ),
            };
            Some(MqttCommand::Toast { kind, msg, timeout_ms: timeout })
        }
        _ => None,
    }
}

fn rand_id() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0));
    h.finish() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics() {
        assert!(matches!(parse("ui3/abc/state/vid", "cam1", "abc", true, true), Some(MqttCommand::Video(v)) if v == "cam1"));
        assert!(parse("ui3/other/state/vid", "cam1", "abc", true, true).is_none());
        assert!(matches!(parse("ui3/global/toast", r#"{"msg":"hi","type":"warning"}"#, "abc", true, true), Some(MqttCommand::Toast { kind, .. }) if kind == "warning"));
        assert!(parse("ui3/global/playaudio", "x.wav", "abc", false, true).is_none());
    }

    #[test]
    fn broker_urls() {
        let tcp = |host: &str, port, tls| Broker::Tcp { host: host.into(), port, tls };
        assert_eq!(parse_broker("mqtt://broker.lan:1884").unwrap(), tcp("broker.lan", 1884, false));
        assert_eq!(parse_broker("tcp://broker.lan/").unwrap(), tcp("broker.lan", 1883, false));
        assert_eq!(parse_broker("broker.lan").unwrap(), tcp("broker.lan", 1883, false));
        assert_eq!(parse_broker("192.168.1.5:1883").unwrap(), tcp("192.168.1.5", 1883, false));
        // TLS schemes select TLS and its default port.
        assert_eq!(parse_broker("mqtts://broker.lan").unwrap(), tcp("broker.lan", 8883, true));
        assert_eq!(parse_broker("SSL://broker.lan:8884").unwrap(), tcp("broker.lan", 8884, true));
        assert_eq!(parse_broker("mqtt://[::1]:1883").unwrap(), tcp("::1", 1883, false));
        assert_eq!(parse_broker("mqtt://[::1]").unwrap(), tcp("::1", 1883, false));
        // WebSocket URLs are passed through whole; wss must be TLS.
        assert_eq!(parse_broker("ws://host:9001/mqtt").unwrap(), Broker::Ws { url: "ws://host:9001/mqtt".into(), tls: false });
        assert_eq!(parse_broker("wss://host/mqtt").unwrap(), Broker::Ws { url: "wss://host/mqtt".into(), tls: true });
        assert!(parse_broker("http://host").is_err());
        assert!(parse_broker("mqtt://host:notaport").is_err());
        assert!(parse_broker("  ").is_err());
    }
}
