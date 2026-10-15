//! Login screen (equivalent of UI3's login.htm + `SessionManager.LogInWithCredentials`).

use crate::creds;
use crate::runtime::{self, Pending};
use crate::settings::{ServerEntry, Settings};
use crate::toasts::Toasts;
use crate::views::main_view::MainView;
use crate::views::{style, w};
use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use anyhow::Result;
use bi_proto::api::CameraInfo;
use bi_proto::{BiClient, Credentials, Session};

type LoginJob = Pending<Result<(BiClient, Session, Vec<CameraInfo>)>>;

pub struct LoginView {
    server_idx: Option<usize>,
    name: String,
    url: String,
    user: String,
    pass: String,
    remember: bool,
    auto_login: bool,
    job: Option<LoginJob>,
    error: Option<String>,
    tried_auto: bool,
    /// The login in progress was started automatically (not by the user pressing Log In).
    was_automatic: bool,
    /// Logged in from UI3N_* environment variables (testing): do not save the server entry.
    from_env: bool,
}

impl LoginView {
    pub fn new(settings: &Settings) -> Self {
        let mut v = LoginView {
            server_idx: None,
            name: String::new(),
            url: String::new(),
            user: String::new(),
            pass: String::new(),
            remember: true,
            auto_login: false,
            job: None,
            error: None,
            tried_auto: false,
            was_automatic: false,
            from_env: false,
        };
        // Development/testing: UI3N_URL / UI3N_USER / UI3N_PASS log in automatically.
        if let Ok(url) = std::env::var("UI3N_URL") {
            v.url = url;
            v.user = std::env::var("UI3N_USER").unwrap_or_default();
            v.pass = std::env::var("UI3N_PASS").unwrap_or_default();
            v.name = "Test server".into();
            v.auto_login = true;
            v.remember = false;
            v.from_env = true;
            return v;
        }
        if let Some(i) = settings.last_server.filter(|i| *i < settings.servers.len()) {
            v.select(settings, i);
        } else if settings.servers.is_empty() {
            // First run: offer the project-local test config if present (development convenience).
            if let Some(cfg) = dev_server_config() {
                v.url = cfg.0;
                v.user = cfg.1;
                v.pass = cfg.2;
                v.name = "Blue Iris".into();
            }
        }
        v
    }

    fn select(&mut self, settings: &Settings, i: usize) {
        let s = &settings.servers[i];
        self.server_idx = Some(i);
        self.name = s.name.clone();
        self.url = s.url.clone();
        self.user = s.user.clone();
        self.auto_login = s.auto_login;
        self.pass = creds::load_password(&s.url, &s.user).unwrap_or_default();
        self.remember = !self.pass.is_empty();
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.was_automatic = false;
        self.error = None;
        let url = self.url.trim().to_string();
        let c = Credentials { user: self.user.trim().to_string(), pass: self.pass.clone() };
        self.job = Some(runtime::spawn(ctx, async move {
            let client = BiClient::new(&url, c)?;
            let session = client.login().await?;
            let cams = client.camlist(false).await?;
            Ok((client, session, cams))
        }));
    }

    /// The login screen shown after an explicit log out: never logs in automatically.
    pub fn after_logout(settings: &Settings) -> Self {
        let mut v = Self::new(settings);
        v.tried_auto = true;
        v
    }

    /// Returns the main view once logged in.
    pub fn ui(&mut self, ui: &mut egui::Ui, settings: &mut Settings, toasts: &mut Toasts) -> Option<MainView> {
        let ctx = ui.ctx().clone();
        if !self.tried_auto {
            self.tried_auto = true;
            // Anonymous servers (empty user) need no password.
            if self.auto_login && !self.url.is_empty() && (!self.pass.is_empty() || self.user.is_empty()) {
                self.start(&ctx);
                self.was_automatic = true;
            }
        }
        if let Some(job) = &mut self.job
            && let Some(res) = job.poll()
        {
            self.job = None;
            match res {
                Ok((client, session, cams)) => {
                    let entry = ServerEntry {
                        name: if self.name.trim().is_empty() { session.login.system_name.clone() } else { self.name.trim().to_string() },
                        url: client.base_url().to_string(),
                        user: self.user.trim().to_string(),
                        auto_login: self.auto_login,
                    };
                    if self.from_env {
                        return Some(MainView::new(client, session, cams, &ctx, settings));
                    }
                    let idx = match settings.servers.iter().position(|s| s.url == entry.url && s.user == entry.user) {
                        Some(i) => {
                            settings.servers[i] = entry.clone();
                            i
                        }
                        None => {
                            settings.servers.push(entry.clone());
                            settings.servers.len() - 1
                        }
                    };
                    settings.last_server = Some(idx);
                    settings.mark_dirty();
                    if self.remember {
                        if let Err(e) = creds::save_password(&entry.url, &entry.user, &self.pass) {
                            toasts.warn(format!("Could not save password: {e}"));
                        }
                    } else {
                        creds::delete_password(&entry.url, &entry.user);
                    }
                    let l = &session.login;
                    // UI3 hides the session toast on automatic logins unless asked (ui3.js:17149).
                    let show_toast = !self.was_automatic || settings.bool("ui3_show_session_success");
                    if show_toast {
                    toasts.success(format!(
                        "Logged in as {} ({})\nServer \"{}\"\nBlue Iris version: {}",
                        if l.user.is_empty() { if l.admin { "administrator" } else { "user" } } else { &l.user },
                        if l.admin { "Administrator" } else { "Limited User" },
                        l.system_name,
                        l.version
                    ));
                    }
                    return Some(MainView::new(client, session, cams, &ctx, settings));
                }
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::BLACK)).show(ui, |ui| {
            let full = ui.max_rect();
            // body { background: radial-gradient(ellipse at center, #45484d 0%, #000000 100%) }
            radial_background(ui.painter(), full);
            let form_w = 360.0f32.min(full.width() - 32.0);
            let form_h = 470.0;
            let top = full.center().y - form_h / 2.0;
            let form = Rect::from_min_size(pos2(full.center().x - form_w / 2.0, top.max(full.top() + 16.0)), vec2(form_w, form_h));
            let mut fui = ui.new_child(egui::UiBuilder::new().max_rect(form).layout(egui::Layout::top_down(egui::Align::Center)));
            let ui = &mut fui;
            ui.spacing_mut().item_spacing = vec2(0.0, 10.0);
            ui.add(egui::Image::new(egui::include_image!("../../assets/icon256.png")).fit_to_exact_size(vec2(72.0, 72.0)));
            // h1
            let (r, _) = ui.allocate_exact_size(vec2(form_w, 40.0), Sense::hover());
            ui.painter().text(r.center() + vec2(0.0, 1.0), Align2::CENTER_CENTER, "UI3 Native", style::bold(32.0), Color32::from_black_alpha(80));
            ui.painter().text(r.center(), Align2::CENTER_CENTER, "UI3 Native", style::bold(32.0), Color32::WHITE);
            ui.add_space(4.0);

            // Saved servers.
            if !settings.servers.is_empty() {
                let sel = self.server_idx.map(|i| format!("{}  —  {}", settings.servers[i].name, settings.servers[i].url)).unwrap_or_else(|| "New server…".into());
                let mut choose = None;
                let servers = settings.servers.clone();
                w::dropdown_box(ui, "servers", &sel, form_w, 42.0, true, |ui| {
                    for (i, s) in servers.iter().enumerate() {
                        if w::list_item(ui, None, &format!("{}  —  {}", s.name, s.url), self.server_idx == Some(i), w::ItemKind::Normal).clicked() {
                            choose = Some(Some(i));
                            ui.close();
                        }
                    }
                    if w::list_item(ui, None, "New server…", self.server_idx.is_none(), w::ItemKind::Normal).clicked() {
                        choose = Some(None);
                        ui.close();
                    }
                });
                match choose {
                    Some(Some(i)) => self.select(settings, i),
                    Some(None) => {
                        self.server_idx = None;
                        self.name.clear();
                        self.url.clear();
                        self.user.clear();
                        self.pass.clear();
                    }
                    None => {}
                }
            }
            ltext(ui, &mut self.url, "Server address, e.g. http://192.168.1.10:81", false, form_w);
            ltext(ui, &mut self.name, "Name (optional)", false, form_w);
            ltext(ui, &mut self.user, "Username", false, form_w);
            let r = ltext(ui, &mut self.pass, "Password", true, form_w);
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && self.job.is_none() {
                self.start(&ctx);
            }
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                ui.visuals_mut().override_text_color = Some(Color32::from_rgb(0xDD, 0xDD, 0xDD));
                ui.set_width(form_w);
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.checkbox(&mut self.auto_login, egui::RichText::new("Log in automatically").size(15.0));
                    ui.checkbox(&mut self.remember, egui::RichText::new("Remember password").size(15.0));
                });
            });
            // .btn
            let busy = self.job.is_some();
            let enabled = !busy && !self.url.trim().is_empty();
            let (br, bresp) = ui.allocate_exact_size(vec2(form_w, 44.0), if enabled { Sense::click() } else { Sense::hover() });
            let fill = if !enabled {
                Color32::from_rgb(0x1D, 0x4D, 0x7B).gamma_multiply(0.6)
            } else if bresp.hovered() {
                Color32::from_rgb(0x22, 0x5B, 0x91)
            } else {
                Color32::from_rgb(0x1D, 0x4D, 0x7B)
            };
            ui.painter().rect_filled(br, 4.0, fill);
            ui.painter().rect_stroke(br, 4.0, Stroke::new(1.0, Color32::from_black_alpha(77)), StrokeKind::Inside);
            ui.painter().text(br.center(), Align2::CENTER_CENTER, if busy { "Logging in…" } else { "Log in" }, FontId::proportional(18.0), Color32::WHITE);
            if bresp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                self.start(&ctx);
            }
            if busy {
                ui.spinner();
            }
            if let Some(e) = &self.error {
                ui.label(egui::RichText::new(e).color(Color32::from_rgb(0xFF, 0x70, 0x70)).size(15.0));
            }
            if let Some(i) = self.server_idx
                && !busy
            {
                let r = ui.add(egui::Label::new(egui::RichText::new("Forget this server").size(13.0).color(Color32::from_gray(0x99)).underline()).sense(Sense::click()));
                if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    let s = settings.servers.remove(i);
                    creds::delete_password(&s.url, &s.user);
                    settings.last_server = None;
                    settings.mark_dirty();
                    self.server_idx = None;
                }
            }
        });
        None
    }
}

/// `.ltext`: 18px text field with a translucent dark fill, rounded corners and a placeholder.
fn ltext(ui: &mut egui::Ui, text: &mut String, hint: &str, password: bool, width: f32) -> egui::Response {
    let frame = egui::Frame::new().fill(Color32::from_black_alpha(77)).stroke(Stroke::new(1.0, Color32::from_black_alpha(77))).corner_radius(4).inner_margin(egui::Margin::symmetric(10, 9));
    frame
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(text)
                    .hint_text(egui::RichText::new(hint).size(18.0).color(Color32::from_gray(0x99)))
                    .password(password)
                    .frame(egui::Frame::NONE)
                    .font(FontId::proportional(18.0))
                    .text_color(Color32::WHITE)
                    .desired_width(width - 22.0),
            )
        })
        .inner
}

/// Radial gradient from #45484d at the center to black at the edges.
fn radial_background(p: &egui::Painter, r: Rect) {
    let mut m = egui::Mesh::default();
    let c = r.center();
    let center = Color32::from_rgb(0x45, 0x48, 0x4D);
    m.colored_vertex(c, center);
    let n = 48;
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        m.colored_vertex(pos2(c.x + a.cos() * r.width() * 0.75, c.y + a.sin() * r.height() * 0.75), Color32::BLACK);
    }
    for i in 1..=n as u32 {
        m.add_triangle(0, i, i + 1);
    }
    p.add(egui::Shape::mesh(m));
}

/// `temp/bi-server.json` next to the workspace (development only; never shipped).
fn dev_server_config() -> Option<(String, String, String)> {
    let mut dir = std::env::current_exe().ok()?;
    for _ in 0..5 {
        dir.pop();
        let p = dir.join("temp").join("bi-server.json");
        if p.exists() {
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
            return Some((v["url"].as_str()?.to_string(), v["user"].as_str().unwrap_or("").to_string(), v["pass"].as_str().unwrap_or("").to_string()));
        }
    }
    None
}
