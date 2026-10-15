//! Event-driven behaviour:
//! * Event-triggered sounds (`biSoundPlayer`, ui3.js:25149): motion/trigger from status-block transitions,
//!   alert from camera-list transitions of the visible cameras; files come from the server's `sounds/` folder.
//! * UI status sounds and speech (`ProgrammaticSoundPlayer`, ui3.js:41173): disconnect/reconnect tones, periodic
//!   beeps while disconnected, SAPI text-to-speech.
//! * Auto-maximize (`PrioritizeTriggered`, ui3.js:18398).

use crate::runtime::{self, Pending};
use crate::settings::Settings;
use anyhow::Result;
use bi_media::audio::AudioOutput;
use bi_proto::BiClient;
use bi_proto::api::CameraInfo;
use bi_proto::stream_parser::StatusBlock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Events {
    last_status: Option<StatusBlock>,
    prev_alerting: bool,
    prev_cam_motion: bool,
    prev_cam_triggered: bool,
    /// Camera auto-maximized by `ui3_prioritizeTriggered`.
    pub pt_current: Option<String>,
    disconnected_at: Option<Instant>,
    did_emit_disconnect: bool,
    next_beep: Option<Instant>,
    sounds: HashMap<String, Arc<Vec<f32>>>,
    sound_jobs: Vec<(String, Pending<Result<Vec<f32>>>)>,
    queued_play: Vec<String>,
    speech: Option<std::sync::mpsc::Sender<(String, String)>>,
}

impl Events {
    /// Status block from the current live stream.
    pub fn on_status(&mut self, ctx: &egui::Context, client: &BiClient, settings: &Settings, audio: &AudioOutput, st: &StatusBlock) {
        let prev = self.last_status.clone().unwrap_or_default();
        if st.motion && !prev.motion {
            self.play_event(ctx, client, settings, audio, "motion");
        }
        if st.triggered && !prev.triggered {
            self.play_event(ctx, client, settings, audio, "trigger");
        }
        self.last_status = Some(st.clone());
    }

    pub fn reset_status(&mut self) {
        self.last_status = None;
    }

    /// Camera list update. `visible` = cameras in the current live view.
    /// With no single stream delivering status blocks (the client-side grid), motion and trigger sounds come from the
    /// camera list instead (`PlayEventSounds`, ui3.js:25220); `from_camlist` selects that mode.
    pub fn on_camlist(&mut self, ctx: &egui::Context, client: &BiClient, settings: &Settings, audio: &AudioOutput, visible: &[&CameraInfo], from_camlist: bool) {
        let motion = visible.iter().any(|c| c.is_motion);
        let triggered = visible.iter().any(|c| c.is_triggered);
        if from_camlist {
            if motion && !self.prev_cam_motion {
                self.play_event(ctx, client, settings, audio, "motion");
            }
            if triggered && !self.prev_cam_triggered {
                self.play_event(ctx, client, settings, audio, "trigger");
            }
        }
        self.prev_cam_motion = motion;
        self.prev_cam_triggered = triggered;
        let alerting = visible.iter().any(|c| c.is_alerting);
        if alerting && !self.prev_alerting {
            self.play_event(ctx, client, settings, audio, "alert");
        }
        self.prev_alerting = alerting;
    }

    pub fn play_event(&mut self, ctx: &egui::Context, client: &BiClient, settings: &Settings, audio: &AudioOutput, event: &str) {
        let file = settings.str(&format!("ui3_sound_{event}"));
        self.play_file(ctx, client, settings, audio, &file);
    }

    /// Plays `sounds/<file>` from the server (cached after first download).
    pub fn play_file(&mut self, ctx: &egui::Context, client: &BiClient, settings: &Settings, audio: &AudioOutput, file: &str) {
        if file.is_empty() || file == "None" || file.contains("..") || file.contains(':') {
            return;
        }
        audio.set_effects_volume((settings.f64("ui3_eventSoundVolume") / 100.0) as f32);
        if let Some(s) = self.sounds.get(file) {
            audio.play_effect(s);
            return;
        }
        self.queued_play.push(file.to_string());
        if self.sound_jobs.iter().any(|(f, _)| f == file) {
            return;
        }
        let c = client.clone();
        let (rate, ch) = (audio.device_rate(), audio.channels());
        let f = file.to_string();
        self.sound_jobs.push((
            file.to_string(),
            runtime::spawn(ctx, async move {
                let r = c.get_bytes(&format!("sounds/{}", bi_proto::urls::encode(&f))).await?;
                anyhow::ensure!(r.status < 400 && !r.body.is_empty(), "HTTP {}", r.status);
                let dir = std::env::temp_dir().join("UI3Native").join("sounds");
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(f.replace(['/', '\\'], "_"));
                std::fs::write(&path, &r.body)?;
                tokio::task::spawn_blocking(move || bi_media::audio::decode_sound_file(&path, rate, ch)).await?
            }),
        ));
    }

    pub fn poll(&mut self, audio: &AudioOutput) {
        let mut done = Vec::new();
        self.sound_jobs.retain_mut(|(f, j)| match j.poll() {
            Some(Ok(s)) => {
                done.push((f.clone(), Arc::new(s)));
                false
            }
            Some(Err(e)) => {
                tracing::warn!("sound {f}: {e:#}");
                false
            }
            None => true,
        });
        for (f, s) in done {
            if let Some(i) = self.queued_play.iter().position(|q| *q == f) {
                self.queued_play.remove(i);
                audio.play_effect(&s);
            }
            self.sounds.insert(f, s);
        }
    }

    /// `NotifyDisconnected` / `NotifyReconnected`: tones after 1 s of disconnection, periodic beeps every 2 s.
    pub fn connection_state(&mut self, settings: &Settings, audio: &AudioOutput, connected: bool) {
        let sound = settings.bool("ui3_uiStatusSounds");
        let speech = settings.bool("ui3_uiStatusSpeech");
        if connected {
            if self.disconnected_at.take().is_some() && self.did_emit_disconnect {
                if sound {
                    audio.play_notes(&[(0.2, 86.0, 0.15), (0.2, 114.0, 0.15), (0.2, 171.0, 0.15)]);
                }
                if speech {
                    self.speak(settings, "reconnected");
                }
            }
            self.did_emit_disconnect = false;
            self.next_beep = None;
            return;
        }
        let since = *self.disconnected_at.get_or_insert_with(Instant::now);
        if !self.did_emit_disconnect && since.elapsed() > Duration::from_secs(1) {
            self.did_emit_disconnect = true;
            if sound {
                audio.play_notes(&[(0.2, 171.0, 0.15), (0.2, 114.0, 0.15), (0.2, 86.0, 0.15)]);
            }
            if speech {
                self.speak(settings, "disconnected");
            }
            self.next_beep = Some(Instant::now() + Duration::from_secs(2));
        } else if let Some(t) = self.next_beep
            && Instant::now() >= t
        {
            if sound {
                audio.play_notes(&[(0.2, 86.0, 0.15)]);
            }
            self.next_beep = Some(Instant::now() + Duration::from_secs(2));
        }
    }

    /// Text-to-speech through Windows SAPI (UI3 used the browser's speechSynthesis).
    pub fn speak(&mut self, settings: &Settings, text: &str) {
        if self.speech.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<(String, String)>();
            std::thread::Builder::new().name("tts".into()).spawn(move || tts_thread(rx)).ok();
            self.speech = Some(tx);
        }
        if let Some(tx) = &self.speech {
            let _ = tx.send((text.to_string(), settings.str("ui3_speechVoice")));
        }
    }
}

/// Installed SAPI voice names (UI3 `GetSpeechVoiceOptions`), enumerated once.
pub fn speech_voices() -> &'static [String] {
    static VOICES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    VOICES.get_or_init(|| sapi_voice_tokens().into_iter().map(|(n, _)| n).collect())
}

/// `(display name, token)` for every installed voice.
fn sapi_voice_tokens() -> Vec<(String, windows::Win32::Media::Speech::ISpObjectToken)> {
    use windows::Win32::Media::Speech::{ISpObjectTokenCategory, SpObjectTokenCategory};
    use windows::Win32::System::Com::{CLSCTX_ALL, CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
    use windows::core::{PCWSTR, w};
    let mut out = Vec::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED); // may already be initialized with another model
        let Ok(cat) = windows::Win32::System::Com::CoCreateInstance::<_, ISpObjectTokenCategory>(&SpObjectTokenCategory, None, CLSCTX_ALL) else { return out };
        if cat.SetId(w!(r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\Voices"), false).is_err() {
            return out;
        }
        let Ok(tokens) = cat.EnumTokens(PCWSTR::null(), PCWSTR::null()) else { return out };
        let mut n = 0u32;
        if tokens.GetCount(&mut n).is_err() {
            return out;
        }
        for i in 0..n {
            let Ok(tok) = tokens.Item(i) else { continue };
            if let Ok(name) = tok.GetStringValue(PCWSTR::null()) {
                let s = name.to_string().unwrap_or_default();
                CoTaskMemFree(Some(name.as_ptr() as *const _));
                if !s.is_empty() {
                    out.push((s, tok));
                }
            }
        }
    }
    out
}

fn tts_thread(rx: std::sync::mpsc::Receiver<(String, String)>) {
    use windows::Win32::Media::Speech::{ISpVoice, SPF_ASYNC, SPF_PURGEBEFORESPEAK, SpVoice};
    use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let voice: ISpVoice = match CoCreateInstance(&SpVoice, None, CLSCTX_ALL) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("SAPI unavailable: {e}");
                return;
            }
        };
        let mut current_voice = String::new();
        while let Ok((text, voice_name)) = rx.recv() {
            if voice_name != current_voice {
                current_voice = voice_name.clone();
                if let Some((_, tok)) = sapi_voice_tokens().into_iter().find(|(n, _)| *n == voice_name) {
                    let _ = voice.SetVoice(&tok);
                }
            }
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = voice.Speak(windows::core::PCWSTR(wide.as_ptr()), (SPF_ASYNC.0 | SPF_PURGEBEFORESPEAK.0) as u32, None);
        }
    }
}

/// `CamIsConsideredTriggered` (ui3.js:18465).
pub fn cam_is_considered_triggered(c: &CameraInfo, settings: &Settings) -> bool {
    let active = c.extra.get("active").is_none_or(bi_proto::lenient::value_to_bool);
    if !(c.is_enabled && active && c.webcast()) {
        return false;
    }
    match settings.str("ui3_prioritizeTriggered_triggerMode").as_str() {
        "Trigger" => c.is_triggered,
        "Motion" => c.is_motion,
        _ => c.is_alerting,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_sapi_voices() {
        // Windows ships at least one SAPI voice; an empty list means the enumeration is broken.
        let v = super::speech_voices();
        println!("voices: {v:?}");
        assert!(!v.is_empty());
    }
}
