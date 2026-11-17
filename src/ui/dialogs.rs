//! Host capabilities, server profiles and About.

use std::sync::Arc;

use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use super::widgets::{self, kv};
use super::App;
use crate::capabilities::{backend_name, codec_name, codec_supports_svc, SVC_MODES};
use crate::profiles::{Profile, ProfileKind};
use crate::version;

/// Working copy of the profile being edited.
#[derive(Default)]
pub struct ProfileEditor {
    pub selected: String,
    pub draft: Option<Profile>,
    pub secret: String,
    pub secret_saved: bool,
    pub message: String,
    pub test: Arc<Mutex<Option<String>>>,
}

impl App {
    pub(super) fn capabilities_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_caps;
        egui::Window::new("Host capabilities")
            .open(&mut open)
            .default_width(820.0)
            .default_height(640.0)
            .vscroll(true)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} {} · LiveKit Rust SDK {} · backends: {}",
                            std::env::consts::OS,
                            std::env::consts::ARCH,
                            version::LIVEKIT_SDK,
                            self.caps.backends.iter().map(|b| backend_name(*b)).collect::<Vec<_>>().join(", ")
                        ))
                        .color(widgets::weak(ui)),
                    );
                    if ui.small_button("Re-probe").clicked() {
                        self.caps = crate::capabilities::HostCaps::probe();
                        self.sanitize_form();
                    }
                });
                ui.label(
                    RichText::new("From the SDK's encoder list per backend and the receiver's codec capabilities. The main window only offers what is available here, and explains anything greyed out.")
                        .small()
                        .color(widgets::weak(ui)),
                );
                ui.add_space(6.0);
                egui::Grid::new("caps").striped(true).spacing([16.0, 5.0]).show(ui, |ui| {
                    for h in ["Codec", "Hardware encode", "Software encode", "Decode", "SVC modes", "Simulcast"] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    for c in &self.caps.codecs {
                        ui.label(codec_name(c.codec));
                        if c.hardware.is_empty() {
                            ui.label(RichText::new("none").color(widgets::weak(ui)));
                        } else {
                            ui.colored_label(widgets::green(ui), c.hardware.iter().map(|b| backend_name(*b)).collect::<Vec<_>>().join(", "));
                        }
                        if c.software {
                            ui.colored_label(widgets::green(ui), "yes");
                        } else {
                            ui.label(RichText::new("not in this SDK build").color(widgets::weak(ui)));
                        }
                        if c.decode {
                            ui.colored_label(widgets::green(ui), "yes");
                        } else {
                            ui.label(RichText::new("no").color(widgets::weak(ui)));
                        }
                        if codec_supports_svc(c.codec) && c.software {
                            ui.label(format!("{} … {} (software)", SVC_MODES[0], SVC_MODES[SVC_MODES.len() - 1]));
                        } else {
                            ui.label(RichText::new("not applied by the SDK").color(widgets::weak(ui)));
                        }
                        if crate::capabilities::codec_supports_simulcast(c.codec) {
                            ui.label("yes");
                        } else {
                            ui.label(RichText::new("no, use SVC").color(widgets::weak(ui)))
                                .on_hover_text("Measured: WebRTC turns a VP9 simulcast request into one stream with spatial layers, and sends AV1 as a single layer.");
                        }
                        ui.end_row();
                    }
                });

                ui.add_space(10.0);
                self.frameworks_ui(ui);
                ui.add_space(10.0);
                ui.label(RichText::new("What can change while publishing").strong());
                ui.label(
                    RichText::new("The SDK keeps a published track's RTP sender private, so most encoder settings need a republish. This build uses the stock SDK.")
                        .small()
                        .color(widgets::weak(ui)),
                );
                egui::Grid::new("live").striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
                    for h in ["Setting", "Stock SDK", "How"] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    let rows = [
                        ("Input and capture format (fixed publish size)", true, "the capture thread restarts and feeds the same track"),
                        ("Subscriber layer and size", true, "set_video_quality · update_video_dimensions"),
                        ("Bitrate cap, max frame rate", false, "unpublish then publish, about 1 s gap"),
                        ("When constrained (degradation preference)", false, "unpublish then publish"),
                        ("Encoder (hardware or software)", false, "unpublish then publish"),
                        ("Codec, SVC mode, simulcast, track source, content hint", false, "unpublish then publish"),
                    ];
                    for (name, live, how) in rows {
                        ui.label(name);
                        if live {
                            widgets::live_badge(ui);
                        } else {
                            widgets::republish_badge(ui);
                        }
                        ui.label(RichText::new(how).small());
                        ui.end_row();
                    }
                });

                ui.add_space(10.0);
                ui.label(RichText::new("Forward error correction").strong());
                egui::Grid::new("fec").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                    kv(ui, "Client video FEC switch", "not in the SDK");
                    kv(ui, "RED in publish options", "audio only");
                    kv(ui, "Server FlexFEC-03, publisher to SFU", "rtc.flexfec.upstream_enabled (Local dev profile option)");
                    let fec = self.session.shared.lock().snapshot.fec_negotiated;
                    kv(ui, "Negotiated in this session", if fec { "yes" } else { "no" });
                });
                ui.add_space(6.0);
                ui.label(RichText::new("Decoders the receiver offers").strong());
                ui.label(RichText::new(self.caps.decoder_mimes.join("  ")).small().monospace());
            });
        self.show_caps = open;
    }

    /// Per-framework view, profiles, what this session saw, and notes.
    fn frameworks_ui(&mut self, ui: &mut egui::Ui) {
        let names = |v: &[livekit::options::VideoCodec]| {
            if v.is_empty() {
                "–".to_string()
            } else {
                v.iter()
                    .map(|c| codec_name(*c))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        };
        ui.label(RichText::new("Video frameworks this build uses").strong());
        egui::Grid::new("frameworks")
            .striped(true)
            .spacing([16.0, 5.0])
            .show(ui, |ui| {
                for h in ["Framework", "Kind", "Encodes", "Decodes", "Decode list"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for f in &self.caps.frameworks {
                    ui.label(&f.name);
                    if f.hardware {
                        ui.colored_label(widgets::green(ui), "hardware");
                    } else {
                        ui.label("software");
                    }
                    ui.label(names(&f.encodes));
                    ui.label(names(&f.decodes));
                    ui.label(
                        RichText::new(f.decode_basis)
                            .small()
                            .color(widgets::weak(ui)),
                    );
                    ui.end_row();
                }
            });
        ui.label(
            RichText::new("Encoders come from the SDK's per-backend lists. The SDK does not say which decoder handles a codec until it decodes one, so the decode column follows its source for this platform; the table below confirms it.")
                .small()
                .color(widgets::weak(ui)),
        );
        for h in &self.caps.hints {
            ui.colored_label(widgets::amber(ui), h);
        }

        ui.add_space(8.0);
        ui.label(RichText::new("Profiles").strong());
        egui::Grid::new("profiles")
            .striped(true)
            .spacing([16.0, 5.0])
            .show(ui, |ui| {
                for h in ["Codec", "Can send", "Can receive"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for codec in crate::capabilities::ALL_CODECS {
                    let find = |list: &Vec<(livekit::options::VideoCodec, Vec<String>)>| {
                        list.iter()
                            .find(|(c, _)| *c == codec)
                            .map(|(_, p)| p.join(", "))
                            .unwrap_or_else(|| "–".into())
                    };
                    ui.label(codec_name(codec));
                    ui.label(find(&self.caps.send_profiles));
                    ui.label(find(&self.caps.recv_profiles));
                    ui.end_row();
                }
            });
        ui.label(
            RichText::new("Read-only: the SDK offers no profile choice. For H.264 it prefers Constrained Baseline for browser compatibility, and the profile is settled with the server when the track is published; the one in use shows in Stats.")
                .small()
                .color(widgets::weak(ui)),
        );

        ui.add_space(8.0);
        ui.label(RichText::new("Seen in this session").strong());
        if self.seen.is_empty() {
            ui.label(RichText::new("Nothing published yet. Each codec you publish adds the encoder and decoder WebRTC actually used.").color(widgets::weak(ui)));
        } else {
            egui::Grid::new("seen")
                .striped(true)
                .spacing([16.0, 5.0])
                .show(ui, |ui| {
                    for h in ["Codec and profile", "Encoder used", "Decoder used"] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    for (codec, (enc, dec)) in &self.seen {
                        ui.label(codec);
                        ui.label(if enc.is_empty() {
                            "–".into()
                        } else {
                            enc.iter().cloned().collect::<Vec<_>>().join(", ")
                        });
                        ui.label(if dec.is_empty() {
                            "–".into()
                        } else {
                            dec.iter().cloned().collect::<Vec<_>>().join(", ")
                        });
                        ui.end_row();
                    }
                });
        }
    }

    pub(super) fn profiles_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_profiles;
        if self.profile_editor.draft.is_none() {
            let id = if self.profile_editor.selected.is_empty() {
                self.profile_id.clone()
            } else {
                self.profile_editor.selected.clone()
            };
            self.select_profile_for_edit(&id);
        }
        egui::Window::new("Server profiles")
            .open(&mut open)
            .default_width(720.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Switch profiles from the main window; switching disconnects first. Profiles are saved in the app's settings file; API secrets go to the system credential store, never the file.")
                        .small()
                        .color(widgets::weak(ui)),
                );
                ui.add_space(4.0);
                ui.columns(2, |c| {
                    // List.
                    let ui = &mut c[0];
                    let profiles = self.store.profiles.clone();
                    for p in &profiles {
                        let sel = self.profile_editor.selected == p.id;
                        let mut text = format!("{}\n{}", p.name, p.connect_url());
                        if p.id == self.store.default_id {
                            text.push_str("   · default");
                        }
                        if ui.selectable_label(sel, text).clicked() && !sel {
                            self.select_profile_for_edit(&p.id);
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Add").clicked() {
                            let id = self.store.add_remote();
                            self.save_store();
                            self.select_profile_for_edit(&id);
                        }
                        if ui.button("Duplicate").clicked() {
                            if let Some(id) = self.store.duplicate(&self.profile_editor.selected.clone()) {
                                self.save_store();
                                self.select_profile_for_edit(&id);
                            }
                        }
                        let selected = self.profile_editor.selected.clone();
                        let deletable = self.store.get(&selected).map(|p| !p.is_local()).unwrap_or(false) && selected != self.profile_id;
                        if ui
                            .add_enabled(deletable, egui::Button::new("Delete"))
                            .on_disabled_hover_text("The Local dev profile and the profile in use cannot be deleted.")
                            .clicked()
                        {
                            if let Some(p) = self.store.get(&selected).cloned() {
                                p.delete_api_secret();
                            }
                            self.store.remove(&selected);
                            self.save_store();
                            let first = self.profile_id.clone();
                            self.select_profile_for_edit(&first);
                        }
                    });

                    // Editor.
                    let ui = &mut c[1];
                    self.profile_form(ui);
                });
            });
        self.show_profiles = open;
        if !open {
            self.profile_editor.draft = None;
        }
    }

    fn select_profile_for_edit(&mut self, id: &str) {
        let e = &mut self.profile_editor;
        e.selected = id.to_string();
        e.draft = self.store.get(id).cloned();
        e.secret.clear();
        e.message.clear();
        *e.test.lock() = None;
        e.secret_saved = e
            .draft
            .as_ref()
            .map(|p| p.kind == ProfileKind::Remote && p.api_secret().is_ok())
            .unwrap_or(false);
    }

    fn profile_form(&mut self, ui: &mut egui::Ui) {
        let Some(mut d) = self.profile_editor.draft.clone() else {
            return;
        };
        let mut save = false;
        let mut test = false;
        egui::Grid::new("profile-form")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut d.name);
                ui.end_row();
                ui.label("Kind");
                ui.label(match d.kind {
                    ProfileKind::LocalDev => "Local dev server (managed by the app)",
                    ProfileKind::Remote => "LiveKit Cloud or self-hosted server",
                });
                ui.end_row();
                match d.kind {
                    ProfileKind::Remote => {
                        ui.label("Server URL");
                        ui.add(
                            egui::TextEdit::singleline(&mut d.url)
                                .hint_text("wss://your-project.livekit.cloud"),
                        );
                        ui.end_row();
                        ui.label("API key");
                        ui.text_edit_singleline(&mut d.api_key);
                        ui.end_row();
                        ui.label("API secret");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.profile_editor.secret)
                                .password(true)
                                .hint_text(if self.profile_editor.secret_saved {
                                    "saved · type to replace"
                                } else {
                                    "not saved yet"
                                }),
                        );
                        ui.end_row();
                    }
                    ProfileKind::LocalDev => {
                        ui.label("Port");
                        ui.add(egui::DragValue::new(&mut d.local.port).range(1024..=64000));
                        ui.end_row();
                        ui.label("Server binary");
                        ui.add(
                            egui::TextEdit::singleline(&mut d.local.binary_path)
                                .hint_text("found automatically"),
                        );
                        ui.end_row();
                        ui.label("Key and secret");
                        ui.label(format!(
                            "{} / {}  (dev mode)",
                            crate::profiles::LOCAL_DEV_KEY,
                            crate::profiles::LOCAL_DEV_SECRET
                        ));
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut d.local.start_with_app, "Start the server with the app");
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(
                            &mut d.local.flexfec_upstream,
                            "Enable FlexFEC upstream (restarts the server)",
                        );
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(
                            &mut d.local.bind_all,
                            "Bind to all interfaces (devices on your network can join)",
                        );
                        ui.end_row();
                    }
                }
                ui.label("Default room");
                ui.text_edit_singleline(&mut d.default_room);
                ui.end_row();
                ui.label("");
                let mut is_default = self.store.default_id == d.id;
                if ui
                    .checkbox(&mut is_default, "Use as default on launch")
                    .changed()
                    && is_default
                {
                    self.store.default_id = d.id.clone();
                    self.save_store();
                }
                ui.end_row();
            });
        if d.kind == ProfileKind::Remote {
            ui.label(
                RichText::new("LiveKit Cloud usage is billed: every test counts toward connection minutes and data transfer. A 50 Mbps test with two subscribers moves about 45 GB an hour.")
                    .small()
                    .color(widgets::amber(ui)),
            );
        }
        let problems = d.validate();
        for p in &problems {
            ui.colored_label(widgets::red(ui), p);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(problems.is_empty(), egui::Button::new("Save profile"))
                .clicked()
            {
                save = true;
            }
            if d.kind == ProfileKind::Remote && ui.button("Test connection").clicked() {
                test = true;
            }
        });
        if !self.profile_editor.message.is_empty() {
            ui.label(RichText::new(&self.profile_editor.message).small());
        }
        if let Some(t) = self.profile_editor.test.lock().clone() {
            ui.label(RichText::new(t).small());
        }

        if save {
            let mut msg = String::new();
            if d.kind == ProfileKind::Remote && !self.profile_editor.secret.is_empty() {
                match d.set_api_secret(self.profile_editor.secret.trim()) {
                    Ok(()) => {
                        self.profile_editor.secret.clear();
                        self.profile_editor.secret_saved = true;
                    }
                    Err(e) => msg = format!("Could not save the secret: {e:#}. "),
                }
            }
            let restart_server = d.is_local()
                && self
                    .store
                    .get(&d.id)
                    .map(|old| old.local != d.local)
                    .unwrap_or(false)
                && d.id == self.profile_id;
            if let Some(slot) = self.store.profiles.iter_mut().find(|p| p.id == d.id) {
                *slot = d.clone();
            }
            self.save_store();
            if restart_server {
                msg.push_str("Saved. Restarting the local server with the new settings.");
                self.restart_server();
            } else if msg.is_empty() {
                msg = "Saved.".into();
            }
            self.profile_editor.message = msg;
            if d.id == self.profile_id && !self.session.phase().connected() {
                self.room = d.default_room.clone();
            }
        }
        if test {
            let secret = if self.profile_editor.secret.is_empty() {
                d.api_secret().ok()
            } else {
                Some(self.profile_editor.secret.trim().to_string())
            };
            let slot = self.profile_editor.test.clone();
            match secret {
                None => *slot.lock() = Some("Enter the API secret first.".into()),
                Some(secret) => {
                    *slot.lock() = Some("Testing…".into());
                    let url = d.connect_url();
                    let key = d.api_key.trim().to_string();
                    self.rt.spawn(async move {
                        let started = std::time::Instant::now();
                        let host = url
                            .replacen("wss://", "https://", 1)
                            .replacen("ws://", "http://", 1);
                        let client = livekit_api::services::room::RoomClient::with_api_key(
                            &host, &key, &secret,
                        );
                        let r = tokio::time::timeout(
                            std::time::Duration::from_secs(10),
                            client.list_rooms(vec![]),
                        )
                        .await;
                        let text = match r {
                            Ok(Ok(rooms)) => format!(
                                "Connected · key and secret accepted · {} active room{} · {} ms",
                                rooms.len(),
                                if rooms.len() == 1 { "" } else { "s" },
                                started.elapsed().as_millis()
                            ),
                            Ok(Err(e)) => format!("Failed: {e}"),
                            Err(_) => "Failed: no answer within 10 s".into(),
                        };
                        *slot.lock() = Some(text);
                    });
                }
            }
        }
        self.profile_editor.draft = Some(d);
    }

    pub(super) fn about_window(&mut self, ctx: &egui::Context) {
        let modal = egui::Modal::new(egui::Id::unique("about")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(version::APP_NAME);
            ui.label(format!("Version {}", version::APP_VERSION));
            ui.add_space(4.0);
            ui.label("An open-source workbench for exploring LiveKit: codecs, encoders, layers, bitrate and what the SDK sends.");
            ui.add_space(8.0);
            egui::Grid::new("about-grid").num_columns(2).spacing([12.0, 5.0]).show(ui, |ui| {
                ui.label(RichText::new("Author").color(widgets::weak(ui)));
                ui.hyperlink_to(format!("{} on GitHub", version::AUTHOR_HANDLE), version::AUTHOR_GITHUB);
                ui.end_row();
                ui.label(RichText::new("LinkedIn").color(widgets::weak(ui)));
                ui.hyperlink_to("linkedin.com/in/ckristianzoli", version::AUTHOR_LINKEDIN);
                ui.end_row();
                ui.label(RichText::new("Source").color(widgets::weak(ui)));
                ui.hyperlink_to("github.com/czoli1976/livekit-workbench", version::REPO_URL);
                ui.end_row();
                ui.label(RichText::new("Built with").color(widgets::weak(ui)));
                ui.vertical(|ui| {
                    ui.label(RichText::new(format!("LiveKit Rust SDK {}", version::LIVEKIT_SDK)).strong());
                    ui.label(RichText::new(format!("libwebrtc {} · webrtc-sys {} · egui {}", version::LIBWEBRTC, version::WEBRTC_SYS, version::EGUI)).small());
                });
                ui.end_row();
                ui.label(RichText::new("Licence").color(widgets::weak(ui)));
                ui.label(version::LICENSE);
                ui.end_row();
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Copy version info").clicked() {
                    ui.ctx().copy_text(version::summary());
                }
                if ui.button("Close").clicked() {
                    self.show_about = false;
                }
            });
        });
        if modal.should_close() {
            self.show_about = false;
        }
    }
}
