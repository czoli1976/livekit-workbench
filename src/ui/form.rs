//! The publish settings in the left panel, and how edits reach a live track.

use eframe::egui::{self, RichText};
use livekit::options::{DegradationPreference, VideoCodec};

use super::widgets::{self, live_badge, republish_badge, section};
use super::App;
use crate::capabilities::{codec_name, codec_supports_svc, EncoderChoice, ALL_CODECS, SVC_MODES};
use crate::capture::SourceKind;
use crate::session::{Layering, Phase, PublishConfig};

/// What the content hint does, read from WebRTC m150 and LiveKit's fork.
/// The SDK has no public contentHint setter, so the app sets the source's
/// screencast flag: WebRTC's sender maps contentHint "detail" and "text" to
/// that same flag ("motion" clears it), and nothing else reads the hint once
/// an explicit degradation preference is set, which the SDK always does.
const CONTENT_HINT_HELP: &str = "Equivalent to the W3C contentHint. The LiveKit Rust SDK has no contentHint setter, so the app sets the video source's screencast flag, which is exactly what WebRTC turns contentHint \"detail\" and \"text\" into (\"motion\" clears it). Detail and text therefore behave the same.\n\n\
With Detail / text:\n\
• When constrained = Balanced becomes Keep resolution: WebRTC drops frames instead of lowering the size.\n\
• VideoToolbox (H.264, H.265): QP capped at 39, and H.264 uses the Baseline profile at an automatic level.\n\
• OpenH264: switches to its screen-content mode.\n\
• All encoders: frames are labelled as screen content for receivers, and the bitrate cap is raised to at least 1.2 Mbps.\n\
NVENC, VAAPI and Jetson encoders do nothing extra with it.";

pub const SIZES: &[(&str, u32, u32)] = &[
    ("360p", 640, 360),
    ("540p", 960, 540),
    ("720p", 1280, 720),
    ("1080p", 1920, 1080),
    ("1440p", 2560, 1440),
    ("4K", 3840, 2160),
    ("5K", 5120, 2880),
    ("8K", 7680, 4320),
];
const RATES: [u32; 5] = [15, 24, 30, 50, 60];
const BITRATE_PRESETS: [f32; 6] = [1.0, 2.5, 5.0, 10.0, 20.0, 50.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    Single,
    Simulcast,
    Svc,
}

/// What the user wants to publish. The engine gets a `PublishConfig` built from it.
#[derive(Clone, Debug, PartialEq)]
pub struct PublishForm {
    pub source: SourceKind,
    pub capture: (u32, u32),
    pub fps: u32,
    pub publish_size: Option<(u32, u32)>,
    pub codec: VideoCodec,
    pub encoder: EncoderChoice,
    pub layers: LayerKind,
    pub svc_mode: String,
    pub bitrate_mbps: f32,
    pub max_fps: u32,
    pub degradation: DegradationPreference,
    pub screen_share: bool,
    pub detail_hint: bool,
}

impl Default for PublishForm {
    fn default() -> Self {
        Self {
            source: SourceKind::Detail,
            capture: (1920, 1080),
            fps: 30,
            publish_size: None,
            codec: VideoCodec::H264,
            encoder: EncoderChoice::Auto,
            layers: LayerKind::Single,
            svc_mode: "L3T3_KEY".into(),
            bitrate_mbps: 5.0,
            max_fps: 30,
            degradation: DegradationPreference::Balanced,
            screen_share: false,
            detail_hint: false,
        }
    }
}

impl PublishForm {
    pub fn to_config(&self) -> PublishConfig {
        PublishConfig {
            source: self.source.clone(),
            capture_w: self.capture.0,
            capture_h: self.capture.1,
            fps: self.fps,
            max_fps: self.max_fps,
            publish_size: self.publish_size,
            codec: self.codec,
            encoder: self.encoder,
            bitrate_bps: (self.bitrate_mbps as f64 * 1e6).round() as u64,
            degradation: self.degradation,
            layering: match self.layers {
                LayerKind::Single => Layering::Single,
                LayerKind::Simulcast => Layering::Simulcast,
                LayerKind::Svc => Layering::Svc(self.svc_mode.clone()),
            },
            screen_share: self.screen_share,
            detail_hint: self.detail_hint,
        }
    }
}

/// How the wanted settings differ from what is published.
#[derive(Debug, Default, PartialEq)]
pub struct Pending {
    /// Input or capture format changed and can be switched live.
    pub live: bool,
    /// Names of settings that need a republish.
    pub republish: Vec<&'static str>,
}

pub fn diff(active: &PublishConfig, want: &PublishConfig) -> Pending {
    let mut p = Pending::default();
    let mut r = |cond: bool, name: &'static str| {
        if cond {
            p.republish.push(name);
        }
    };
    r(active.publish_dims() != want.publish_dims(), "publish size");
    r(active.codec != want.codec, "codec");
    r(active.encoder != want.encoder, "encoder");
    r(active.layering != want.layering, "layers");
    r(active.bitrate_bps != want.bitrate_bps, "bitrate cap");
    r(active.max_fps != want.max_fps, "max frame rate");
    r(active.degradation != want.degradation, "when constrained");
    r(active.screen_share != want.screen_share, "track source");
    r(active.detail_hint != want.detail_hint, "content hint");
    p.live = (active.source != want.source
        || (active.capture_w, active.capture_h) != (want.capture_w, want.capture_h)
        || active.fps != want.fps)
        && active.publish_dims() == want.publish_dims();
    p
}

pub fn size_name(p: (u32, u32)) -> String {
    match SIZES.iter().find(|s| (s.1, s.2) == p) {
        Some(s) => format!("{}  {}×{}", s.0, s.1, s.2),
        None => format!("{}×{}", p.0, p.1),
    }
}

pub fn degradation_name(d: DegradationPreference) -> &'static str {
    match d {
        DegradationPreference::MaintainResolution => "Keep resolution, drop frames",
        DegradationPreference::MaintainFramerate => "Keep frame rate, lower resolution",
        DegradationPreference::Balanced => "Balanced",
        DegradationPreference::MaintainFramerateAndResolution => "Keep both (may stall)",
        #[allow(unreachable_patterns)]
        _ => "Default",
    }
}

pub fn svc_mode_text(mode: &str) -> String {
    let b = mode.as_bytes();
    if b.len() < 4 {
        return String::new();
    }
    let s = (b[1] - b'0') as u32;
    let t = (b[3] - b'0') as u32;
    let key = if mode.ends_with("_KEY") {
        " Keyframes only on the base layer."
    } else {
        ""
    };
    match (s, t) {
        (1, t) => format!("1 spatial × {t} temporal layers: frame rate can drop for a subscriber, size cannot.{key}"),
        (s, t) => format!("{s} spatial × {t} temporal layers in one stream.{key}"),
    }
}

impl App {
    pub(super) fn source_label(&self, s: &SourceKind) -> String {
        match s {
            SourceKind::Camera(i) => self
                .cameras
                .iter()
                .find(|c| c.index == *i)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| format!("Camera {i}")),
            SourceKind::Pattern => "Synthetic: moving pattern".into(),
            SourceKind::Detail => "Synthetic: detailed motion".into(),
            SourceKind::Screen => "Screen".into(),
        }
    }

    /// Keep the form consistent with what this machine can do.
    pub(super) fn sanitize_form(&mut self) {
        let f = &mut self.form;
        if self.caps.encoder_allowed(f.codec, f.encoder).is_err() {
            f.encoder = EncoderChoice::Auto;
        }
        if f.layers == LayerKind::Simulcast
            && !crate::capabilities::codec_supports_simulcast(f.codec)
        {
            // Switching to VP9 or AV1 with simulcast on: SVC is what those codecs use.
            let svc_ok = self
                .caps
                .get(f.codec)
                .map(|c| c.svc_with(f.encoder))
                .unwrap_or(false);
            f.layers = if svc_ok {
                LayerKind::Svc
            } else {
                LayerKind::Single
            };
        }
        if f.layers == LayerKind::Svc {
            let ok = self
                .caps
                .get(f.codec)
                .map(|c| c.svc_with(f.encoder))
                .unwrap_or(false);
            if !ok {
                f.layers = LayerKind::Single;
            }
        }
    }

    pub(super) fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let phase = self.session.phase();
        let publishing = phase == Phase::Publishing;
        let active = self.session.shared.lock().active_publish.clone();
        let before = self.form.clone();

        // ---- Source
        section(ui, "Source");
        egui::Grid::new("source-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Input");
                live_badge(ui);
            });
            ui.horizontal(|ui| {
                let label = self.source_label(&self.form.source);
                egui::ComboBox::from_id_salt("source")
                    .selected_text(label)
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.form.source, SourceKind::Detail, "Synthetic: detailed motion")
                            .on_hover_text("A moving pattern plus fresh grain every frame: needs a realistic bitrate, like a camera.");
                        ui.selectable_value(&mut self.form.source, SourceKind::Pattern, "Synthetic: moving pattern")
                            .on_hover_text("A scrolling texture: cheap to encode, needs little bitrate.");
                        ui.selectable_value(&mut self.form.source, SourceKind::Screen, "Screen")
                            .on_hover_text(if cfg!(target_os = "macos") {
                                "A display or window, chosen in macOS's picker when capture starts (at Connect or Publish)."
                            } else {
                                "The main screen; on Linux with Wayland, the desktop picker asks which."
                            });
                        for c in self.cameras.clone() {
                            ui.selectable_value(&mut self.form.source, SourceKind::Camera(c.index), c.name);
                        }
                    });
                if ui.small_button("⟳").on_hover_text("Rescan cameras").clicked() {
                    self.refresh_cameras();
                }
            });
            ui.end_row();

            ui.horizontal(|ui| {
                ui.label("Capture");
                live_badge(ui);
            });
            match self.form.source.clone() {
                SourceKind::Camera(index) => {
                    self.load_formats(index);
                    let label = format!("{}×{} · {} fps", self.form.capture.0, self.form.capture.1, self.form.fps);
                    egui::ComboBox::from_id_salt("cam-format").selected_text(label).width(190.0).show_ui(ui, |ui| {
                        for f in self.camera_formats.clone() {
                            let sel = self.form.capture == (f.width, f.height) && self.form.fps == f.fps;
                            if ui.selectable_label(sel, format!("{}×{} · {} fps", f.width, f.height, f.fps)).clicked() {
                                self.form.capture = (f.width, f.height);
                                self.form.fps = f.fps;
                            }
                        }
                    });
                }
                SourceKind::Pattern | SourceKind::Detail | SourceKind::Screen => {
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("syn-size")
                            .selected_text(size_name(self.form.capture))
                            .width(124.0)
                            .show_ui(ui, |ui| {
                                for &(name, w, h) in SIZES {
                                    ui.selectable_value(&mut self.form.capture, (w, h), format!("{name}  {w}×{h}"));
                                }
                            });
                        egui::ComboBox::from_id_salt("syn-fps")
                            .selected_text(format!("{} fps", self.form.fps))
                            .width(60.0)
                            .show_ui(ui, |ui| {
                                for r in RATES {
                                    ui.selectable_value(&mut self.form.fps, r, format!("{r} fps"));
                                }
                            });
                    });
                }
            }
            ui.end_row();

            ui.horizontal(|ui| {
                ui.label("Publish size");
                republish_badge(ui);
            });
            let label = match self.form.publish_size {
                None => "Same as capture".to_string(),
                Some(p) => size_name(p),
            };
            egui::ComboBox::from_id_salt("pub-size").selected_text(label).width(190.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.form.publish_size, None, "Same as capture");
                for &(name, w, h) in SIZES {
                    ui.selectable_value(&mut self.form.publish_size, Some((w, h)), format!("{name}  {w}×{h}"));
                }
            })
            .response
            .on_hover_text("Frames are scaled to this size before the encoder. Pick a fixed size to switch inputs live.");
            ui.end_row();

            ui.horizontal(|ui| {
                ui.label("Track source");
                republish_badge(ui);
            });
            egui::ComboBox::from_id_salt("track-source")
                .selected_text(if self.form.screen_share { "Screen share (fast start)" } else { "Camera" })
                .width(190.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.form.screen_share, false, "Camera");
                    ui.selectable_value(&mut self.form.screen_share, true, "Screen share (fast start)").on_hover_text(
                        "The SDK seeds WebRTC's start bitrate at 90% of the cap for screen share, against at most 1 Mbps for camera, so the ramp-up is much shorter. The track is labelled a screen share in the room.",
                    );
                });
            ui.end_row();

            ui.horizontal(|ui| {
                ui.label("Content hint");
                republish_badge(ui);
            });
            egui::ComboBox::from_id_salt("hint")
                .selected_text(if self.form.detail_hint { "Detail / text" } else { "Motion" })
                .width(190.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.form.detail_hint, false, "Motion");
                    ui.selectable_value(&mut self.form.detail_hint, true, "Detail / text")
                        .on_hover_text(CONTENT_HINT_HELP);
                });
            ui.end_row();
        });
        if let Some(e) = &self.camera_error {
            ui.colored_label(widgets::red(ui), format!("Camera: {e}"));
        }

        ui.separator();
        // ---- Encode
        section(ui, "Encode");
        egui::Grid::new("encode-grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Codec");
                    republish_badge(ui);
                });
                egui::ComboBox::from_id_salt("codec")
                    .selected_text(codec_name(self.form.codec))
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        for codec in ALL_CODECS {
                            let caps = self.caps.get(codec).cloned();
                            let can = caps.as_ref().map(|c| c.can_encode()).unwrap_or(false);
                            let text = match &caps {
                                Some(c) => {
                                    let mut parts = Vec::new();
                                    if let Some(b) = c.hardware.first() {
                                        parts.push(format!(
                                            "HW · {}",
                                            crate::capabilities::backend_name(*b)
                                        ));
                                    }
                                    if c.software {
                                        parts.push("SW".into());
                                    }
                                    if codec_supports_svc(codec) && c.software {
                                        parts.push("SVC".into());
                                    }
                                    format!("{}   {}", codec_name(codec), parts.join(" · "))
                                }
                                None => codec_name(codec).to_string(),
                            };
                            let resp = ui.add_enabled(
                                can,
                                egui::Button::selectable(self.form.codec == codec, text),
                            );
                            if resp.clicked() {
                                self.form.codec = codec;
                            }
                            resp.on_disabled_hover_text(format!(
                                "No {} encoder in this SDK build on this machine.",
                                codec_name(codec)
                            ));
                        }
                    });
                ui.end_row();

                ui.horizontal(|ui| {
                    ui.label("Encoder");
                    republish_badge(ui);
                });
                ui.horizontal(|ui| {
                    for choice in [
                        EncoderChoice::Auto,
                        EncoderChoice::Hardware,
                        EncoderChoice::Software,
                    ] {
                        let allowed = self.caps.encoder_allowed(self.form.codec, choice);
                        let resp = ui.add_enabled(
                            allowed.is_ok(),
                            egui::Button::selectable(self.form.encoder == choice, choice.label()),
                        );
                        if resp.clicked() {
                            self.form.encoder = choice;
                        }
                        if let Err(why) = allowed {
                            resp.on_disabled_hover_text(why);
                        }
                    }
                });
                ui.end_row();
            });
        if self.form.encoder == EncoderChoice::Auto {
            ui.label(
                RichText::new(format!(
                    "Auto resolves to {}.",
                    self.caps.auto_resolves_to(self.form.codec)
                ))
                .small()
                .color(widgets::weak(ui)),
            );
        }
        // Desktop SDK builds decode H.264 and H.265 only through a platform
        // decoder (VideoToolbox, NVDEC); without one the app's own subscriber
        // cannot receive that codec, though browsers can.
        if !self
            .caps
            .get(self.form.codec)
            .map(|c| c.decode)
            .unwrap_or(true)
        {
            ui.label(
                RichText::new(format!(
                    "This machine can encode {} but the SDK cannot decode it here, so the far-end preview stays empty. Open a browser viewer to receive it.",
                    codec_name(self.form.codec)
                ))
                .small()
                .color(widgets::amber(ui)),
            );
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Layers");
            republish_badge(ui);
        });
        let svc_ok = self
            .caps
            .get(self.form.codec)
            .map(|c| c.svc_with(self.form.encoder))
            .unwrap_or(false);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.form.layers, LayerKind::Single, "Single");
            let sim_ok = crate::capabilities::codec_supports_simulcast(self.form.codec);
            let resp = ui
                .add_enabled(
                    sim_ok,
                    egui::Button::selectable(self.form.layers == LayerKind::Simulcast, "Simulcast"),
                )
                .on_hover_text("Up to three separate encodings (f, h, q) at full, half and quarter size.");
            if resp.clicked() {
                self.form.layers = LayerKind::Simulcast;
            }
            resp.on_disabled_hover_text(format!(
                "Not for {}: with this SDK, WebRTC sends VP9 as one stream with spatial layers and AV1 as a single layer when simulcast is requested. LiveKit uses SVC for VP9 and AV1; choose SVC.",
                codec_name(self.form.codec)
            ));
            let resp = ui.add_enabled(
                svc_ok,
                egui::Button::selectable(self.form.layers == LayerKind::Svc, "SVC"),
            );
            if resp.clicked() {
                self.form.layers = LayerKind::Svc;
            }
            let why = if !codec_supports_svc(self.form.codec) {
                format!(
                    "The SDK applies a scalability mode only for VP9 and AV1, not {}.",
                    codec_name(self.form.codec)
                )
            } else if self.form.encoder == EncoderChoice::Hardware {
                "Hardware encoders in this SDK advertise L1T1 only (no SVC). Choose Software."
                    .to_string()
            } else {
                "Auto would pick a hardware encoder here, which has no SVC. Choose Software."
                    .to_string()
            };
            resp.on_disabled_hover_text(why);
        });
        if self.form.layers == LayerKind::Svc {
            ui.horizontal(|ui| {
                ui.label("Scalability mode");
                egui::ComboBox::from_id_salt("svc-mode")
                    .selected_text(self.form.svc_mode.clone())
                    .show_ui(ui, |ui| {
                        for m in SVC_MODES {
                            ui.selectable_value(&mut self.form.svc_mode, m.to_string(), m);
                        }
                    });
            });
            ui.label(
                RichText::new(svc_mode_text(&self.form.svc_mode))
                    .small()
                    .color(widgets::weak(ui)),
            );
        }

        ui.separator();
        // ---- Rate control
        section(ui, "Rate control");
        egui::Grid::new("rate-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Bitrate cap");
                republish_badge(ui);
            });
            ui.add(
                egui::DragValue::new(&mut self.form.bitrate_mbps)
                    .range(0.1..=200.0)
                    .speed(0.1)
                    .max_decimals(1)
                    .suffix(" Mbps"),
            )
            .on_hover_text("The track's max_bitrate. With simulcast it is the top layer's cap; the SDK picks the lower layers' caps.");
            ui.end_row();
            ui.label("");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;

                for b in BITRATE_PRESETS {
                    if ui.small_button(format!("{b}")).clicked() {
                        self.form.bitrate_mbps = b;
                    }
                }
            });
            ui.end_row();

            ui.horizontal(|ui| {
                ui.label("Max frame rate");
                republish_badge(ui);
            });
            egui::ComboBox::from_id_salt("max-fps").selected_text(format!("{} fps", self.form.max_fps)).show_ui(ui, |ui| {
                for r in RATES {
                    ui.selectable_value(&mut self.form.max_fps, r, format!("{r} fps"));
                }
            });
            ui.end_row();

            ui.horizontal(|ui| {
                ui.label("When constrained");
                republish_badge(ui);
            });
            egui::ComboBox::from_id_salt("degrade")
                .selected_text(degradation_name(self.form.degradation))
                .width(190.0)
                .show_ui(ui, |ui| {
                    for d in [
                        DegradationPreference::MaintainResolution,
                        DegradationPreference::MaintainFramerate,
                        DegradationPreference::Balanced,
                        DegradationPreference::MaintainFramerateAndResolution,
                    ] {
                        ui.selectable_value(&mut self.form.degradation, d, degradation_name(d));
                    }
                });
            ui.end_row();
        });

        ui.separator();
        // ---- Resilience
        section(ui, "Resilience");
        let fec_list = self.session.shared.lock().snapshot.fec.clone();
        let fec = !fec_list.is_empty();
        egui::Grid::new("res-grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                widgets::kv(ui, "NACK and RTX", "Always on");
                let fec_text = if !phase.connected() {
                    RichText::new("Shown once publishing")
                } else if fec {
                    RichText::new(format!("{} negotiated", fec_list.join(", ")))
                        .color(widgets::green(ui))
                } else {
                    RichText::new("Not negotiated")
                };
                widgets::kv(ui, "Video FEC", fec_text);
            });
        ui.label(
            RichText::new(
                "Not available from this app's publisher. LiveKit 1.13.11 can accept FlexFEC-03 from publishers (rtc.flexfec.upstream_enabled), but WebRTC only offers it with the WebRTC-FlexFEC-03 field trials, and the Rust SDK compiles in a fixed set without them. WebRTC also offers ULPFEC with RED, and the LiveKit server's answer leaves them out. Recovery here is NACK and RTX.",
            )
            .small()
            .color(widgets::weak(ui)),
        );

        self.sanitize_form();

        // ---- Input changes apply to a live track at once; the rest wait in
        // the banner pinned above this panel (pending_banner).
        if publishing {
            if let Some(active) = &active {
                let want = self.form.to_config();
                if diff(active, &want).live && self.form != before {
                    self.session.switch_source(want);
                }
            }
        }
    }

    /// Pending republish changes, pinned above the scrolling settings so it is
    /// always in view.
    pub(super) fn pending_banner(&mut self, ui: &mut egui::Ui) {
        if self.session.phase() != Phase::Publishing {
            return;
        }
        let Some(active) = self.session.shared.lock().active_publish.clone() else {
            return;
        };
        let pending = diff(&active, &self.form.to_config());
        if pending.republish.is_empty() {
            return;
        }
        egui::Frame::new()
            .fill(super::theme::AMBER_BG)
            .stroke(egui::Stroke::new(1.0, widgets::amber(ui)))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let n = pending.republish.len();
                ui.label(
                    RichText::new(format!(
                        "{n} pending change{}: {}",
                        if n == 1 { "" } else { "s" },
                        pending.republish.join(", ")
                    ))
                    .strong()
                    .color(super::theme::AMBER_TEXT),
                );
                ui.label(
                    RichText::new("Applying republishes the track, about 1 s without video.")
                        .small()
                        .color(super::theme::AMBER_TEXT),
                );
                ui.horizontal(|ui| {
                    if ui.add(widgets::accent_button("Apply")).clicked() {
                        self.publish_or_republish(true);
                    }
                    if ui.button("Revert").clicked() {
                        self.revert_form(&active);
                    }
                });
            });
        ui.add_space(8.0);
    }

    fn revert_form(&mut self, a: &PublishConfig) {
        let f = &mut self.form;
        f.codec = a.codec;
        f.encoder = a.encoder;
        f.bitrate_mbps = a.bitrate_bps as f32 / 1e6;
        f.max_fps = a.max_fps;
        f.degradation = a.degradation;
        f.screen_share = a.screen_share;
        f.detail_hint = a.detail_hint;
        f.publish_size = a.publish_size;
        match &a.layering {
            Layering::Single => f.layers = LayerKind::Single,
            Layering::Simulcast => f.layers = LayerKind::Simulcast,
            Layering::Svc(m) => {
                f.layers = LayerKind::Svc;
                f.svc_mode = m.clone();
            }
        }
        if f.publish_size.is_none() {
            f.capture = (a.capture_w, a.capture_h);
        }
    }

    /// Check the form against this machine, returning a reason it cannot publish.
    pub(super) fn validate_form(&self) -> Result<PublishConfig, String> {
        let cfg = self.form.to_config();
        if matches!(cfg.source, SourceKind::Camera(_)) {
            crate::capture::camera_access().map_err(|e| format!("{e:#}"))?;
        }
        self.caps.encoder_allowed(cfg.codec, cfg.encoder)?;
        if cfg.layering == Layering::Simulcast
            && !crate::capabilities::codec_supports_simulcast(cfg.codec)
        {
            return Err(format!(
                "Simulcast is not available for {}; use SVC.",
                codec_name(cfg.codec)
            ));
        }
        if let Layering::Svc(_) = cfg.layering {
            if !self
                .caps
                .get(cfg.codec)
                .map(|c| c.svc_with(cfg.encoder))
                .unwrap_or(false)
            {
                return Err("SVC is not available for this codec and encoder.".into());
            }
        }
        if cfg.bitrate_bps < 50_000 {
            return Err("The bitrate cap is too low.".into());
        }
        Ok(cfg)
    }

    pub(super) fn publish_or_republish(&mut self, republish: bool) {
        match self.validate_form() {
            Ok(cfg) => {
                if republish {
                    self.session.republish(cfg);
                } else {
                    self.session.publish(cfg);
                }
            }
            Err(e) => self.notify(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_splits_live_and_republish() {
        let mut f = PublishForm {
            publish_size: Some((1280, 720)),
            ..Default::default()
        };
        let a = f.to_config();
        f.source = SourceKind::Pattern;
        f.capture = (3840, 2160);
        let d = diff(&a, &f.to_config());
        assert!(d.live);
        assert!(d.republish.is_empty());

        f.codec = VideoCodec::VP9;
        f.bitrate_mbps = 8.0;
        let d = diff(&a, &f.to_config());
        assert_eq!(d.republish, vec!["codec", "bitrate cap"]);
    }

    #[test]
    fn capture_change_without_fixed_size_needs_republish() {
        let mut f = PublishForm::default();
        let a = f.to_config();
        f.capture = (1280, 720);
        let d = diff(&a, &f.to_config());
        assert!(!d.live);
        assert_eq!(d.republish, vec!["publish size"]);
    }

    #[test]
    fn svc_text() {
        assert!(svc_mode_text("L3T3_KEY").contains("3 spatial × 3 temporal"));
        assert!(svc_mode_text("L3T3_KEY").contains("base layer"));
        assert!(svc_mode_text("L1T2").starts_with("1 spatial × 2"));
    }
}
