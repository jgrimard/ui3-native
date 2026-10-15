//! MQTT remote control. Port of `MqttClient` (ui3.js:41483).
//!
//! Subscribes to `ui3/<instance>/#` and `ui3/global/#`:
//! * `ui3/<id>/state/vid` (camera id), `state/maximize` ("1"/"0"), `state/volume` (0-100)
//! * `ui3/<id|global>/playaudio[/<player>]` (sound file name), `playtts` (text), `toast[/<id>]` (JSON or text)
//!
//! Publishes `ui3/<id>/state/{vid,maximize,volume}` when they change. UI3 used a WebSocket URL
//! (`ws://host:port/path`); plain `mqtt://host:1883` / `tcp://` URLs also work natively.

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, Transport};
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
}

pub struct Mqtt {
    client: AsyncClient,
    rx: crossbeam_channel::Receiver<MqttCommand>,
    instance: String,
    qos: QoS,
    retain: bool,
    publish_enabled: bool,
    published: HashMap<String, String>,
    #[allow(dead_code)]
    pub config_key: String,
}

fn qos(n: f64) -> QoS {
    match n as i32 {
        1 => QoS::AtLeastOnce,
        2 => QoS::ExactlyOnce,
        _ => QoS::AtMostOnce,
    }
}

impl Mqtt {
    /// Settings fingerprint so the client restarts when any MQTT setting changes.
    pub fn config_key(s: &crate::settings::Settings) -> String {
        [
            "ui3_mqttClientEnabled",
            "ui3_mqttBrokerUrl",
            "ui3_mqttUser",
            "ui3_mqttPass",
            "ui3_mqttInstanceId",
            "ui3_mqttSubscribeEnabled",
            "ui3_mqttSubscribeQOS",
            "ui3_mqttPublishEnabled",
            "ui3_mqttPublishQOS",
            "ui3_mqttPublishRetain",
            "ui3_mqttAudioEvents",
            "ui3_mqttToastEvents",
        ]
        .iter()
        .map(|k| s.str(k))
        .collect::<Vec<_>>()
        .join("|")
    }

    pub fn start(s: &crate::settings::Settings, ctx: &egui::Context) -> anyhow::Result<Option<Mqtt>> {
        if !s.bool("ui3_mqttClientEnabled") {
            return Ok(None);
        }
        let url = s.str("ui3_mqttBrokerUrl");
        anyhow::ensure!(!url.trim().is_empty(), "MQTT broker URL is not set");
        let instance = {
            let i = s.str("ui3_mqttInstanceId");
            if i.trim().is_empty() { "ui3native".to_string() } else { i }
        };
        let client_id = format!("ui3n{:08x}", rand_id());
        let lower = url.to_ascii_lowercase();
        let mut opts = if lower.starts_with("ws://") || lower.starts_with("wss://") {
            let mut o = MqttOptions::new(client_id, url.clone(), 0);
            o.set_transport(Transport::Ws);
            o
        } else {
            let rest = url.split("://").nth(1).unwrap_or(&url);
            let (host, port) = rest.trim_end_matches('/').rsplit_once(':').map(|(h, p)| (h.to_string(), p.parse().unwrap_or(1883))).unwrap_or((rest.trim_end_matches('/').to_string(), 1883));
            MqttOptions::new(client_id, host, port)
        };
        opts.set_keep_alive(Duration::from_secs(30));
        let sub_qos = qos(s.f64("ui3_mqttSubscribeQOS"));
        let pub_qos = qos(s.f64("ui3_mqttPublishQOS"));
        opts.set_clean_session(sub_qos == QoS::AtMostOnce && pub_qos == QoS::AtMostOnce);
        let user = s.str("ui3_mqttUser");
        if !user.is_empty() {
            opts.set_credentials(user, s.str("ui3_mqttPass"));
        }
        let (client, mut eventloop) = AsyncClient::new(opts, 32);
        let (tx, rx) = crossbeam_channel::unbounded();
        let subscribe = s.bool("ui3_mqttSubscribeEnabled");
        let audio_events = s.bool("ui3_mqttAudioEvents");
        let toast_events = s.bool("ui3_mqttToastEvents");
        let inst = instance.clone();
        let c2 = client.clone();
        let ctx = ctx.clone();
        crate::runtime::handle().spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Packet::ConnAck(_))) => {
                        tracing::info!("MQTT connected");
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
                        tracing::warn!("MQTT: {e}");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });
        Ok(Some(Mqtt {
            client,
            rx,
            instance,
            qos: pub_qos,
            retain: s.bool("ui3_mqttPublishRetain"),
            publish_enabled: s.bool("ui3_mqttPublishEnabled"),
            published: HashMap::new(),
            config_key: Self::config_key(s),
        }))
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
        crate::runtime::spawn_detached(async move {
            let _ = c.disconnect().await;
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
}
