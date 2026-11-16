//! The central area: pipeline cards with health outlines, the chart, previews
//! and the browser invite.

use std::time::Instant;

use eframe::egui::{self, RichText};

use super::widgets::{self, big, dims, live_badge, or_dash, stage_card, Series};
use super::App;
use crate::capabilities::codec_name;
use crate::session::{Layering, Phase, SharedPreview};
use crate::stats::{Snapshot, HISTORY};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChartMetric {
    Bitrate,
    Resolution,
    FrameRate,
}

/// What the app's subscriber asks the SFU to forward. Every choice is sent
/// as a size, which works for simulcast and SVC alike (the SDK's quality
/// call is simulcast-only), and the SFU picks the smallest layer covering it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubAsk {
    Top,
    FitTile,
    Half,
    Quarter,
}

impl SubAsk {
    pub fn label(self) -> &'static str {
        match self {
            SubAsk::Top => "Top layer",
            SubAsk::FitTile => "Fit the far-end tile",
            SubAsk::Half => "Half size",
            SubAsk::Quarter => "Quarter size",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PreviewLayout {
    SideBySide,
    LocalOnly,
    FarOnly,
}

/// One preview: the GPU texture made from the engine's shared thumbnail.
#[derive(Default)]
pub struct PreviewPane {
    texture: Option<egui::TextureHandle>,
    seen: u64,
}

impl PreviewPane {
    /// Draw the latest frame to fit `size`. Returns the rect used.
    fn paint(
        &mut self,
        ui: &mut egui::Ui,
        slot: &SharedPreview,
        name: &str,
        size: egui::Vec2,
        placeholder: &str,
    ) -> egui::Rect {
        {
            let p = slot.lock();
            if p.seq != self.seen {
                self.seen = p.seq;
                if p.w > 0 && p.rgba.len() == p.w * p.h * 4 {
                    let img = egui::ColorImage::from_rgba_unmultiplied([p.w, p.h], &p.rgba);
                    match &mut self.texture {
                        Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                        None => {
                            self.texture = Some(ui.ctx().load_texture(
                                name,
                                img,
                                egui::TextureOptions::LINEAR,
                            ))
                        }
                    }
                } else {
                    self.texture = None;
                }
            }
        }
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 4.0, egui::Color32::from_gray(12));
        match &self.texture {
            Some(t) => {
                let ts = t.size_vec2();
                let scale = (rect.width() / ts.x).min(rect.height() / ts.y);
                let fit = egui::Rect::from_center_size(rect.center(), ts * scale);
                ui.painter().image(
                    t.id(),
                    fit,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            None => {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    placeholder,
                    egui::FontId::proportional(13.0),
                    egui::Color32::from_gray(140),
                );
            }
        }
        rect
    }
}

fn layering_text(l: &Layering) -> String {
    match l {
        Layering::Single => "single layer".into(),
        Layering::Simulcast => "simulcast".into(),
        Layering::Svc(m) => format!("SVC {m}"),
    }
}

fn hw(b: bool) -> &'static str {
    if b {
        "HW"
    } else {
        "SW"
    }
}

impl App {
    pub(super) fn pipeline_ui(&mut self, ui: &mut egui::Ui) {
        let (phase, snap, active, error) = {
            let s = self.session.shared.lock();
            (
                s.phase,
                s.snapshot.clone(),
                s.active_publish.clone(),
                s.error.clone(),
            )
        };

        // ---- Header
        ui.horizontal_wrapped(|ui| {
            ui.heading("Pipeline");
            ui.add_space(8.0);
            match &active {
                Some(a) => {
                    let (w, h) = a.publish_dims();
                    let enc = snap
                        .top_layer()
                        .map(|t| t.encoder.clone())
                        .unwrap_or_default();
                    ui.label(
                        RichText::new(format!(
                            "{} · {} · {}{}×{} · cap {:.1} Mbps",
                            codec_name(a.codec),
                            layering_text(&a.layering),
                            if enc.is_empty() {
                                String::new()
                            } else {
                                format!("{enc} · ")
                            },
                            w,
                            h,
                            a.bitrate_bps as f64 / 1e6
                        ))
                        .color(widgets::weak(ui)),
                    );
                }
                None => {
                    ui.label(
                        RichText::new(match phase {
                            Phase::Connected => "Connected, not publishing",
                            _ => "Not publishing",
                        })
                        .color(widgets::weak(ui)),
                    );
                }
            }
        });
        if let Some(e) = &error {
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(e).color(widgets::red(ui))).wrap());
                if ui.small_button("Dismiss").clicked() {
                    self.session.shared.lock().error = None;
                }
            });
        }
        if let (Some(a), Some(top)) = (&active, snap.top_layer()) {
            // The SDK quietly uses another encoder when the requested one cannot
            // produce the codec.
            let mismatch = match a.encoder {
                crate::capabilities::EncoderChoice::Software if top.hardware => Some("software"),
                crate::capabilities::EncoderChoice::Hardware
                    if !top.hardware && !top.encoder.is_empty() =>
                {
                    Some("hardware")
                }
                _ => None,
            };
            if let Some(wanted) = mismatch {
                ui.colored_label(
                    widgets::amber(ui),
                    format!(
                        "You asked for a {wanted} encoder; WebRTC is using {} ({}).",
                        top.encoder,
                        hw(top.hardware)
                    ),
                );
            }
        }
        if active.is_some()
            && snap.total_target_mbps > 0.0
            && snap.total_max_mbps > 0.0
            && snap.total_target_mbps < snap.total_max_mbps * 0.9
        {
            ui.label(
                RichText::new(format!(
                    "Ramping: WebRTC allows {:.1} of {:.1} Mbps. It raises its estimate over about a minute; Screen share track source starts higher.",
                    snap.total_target_mbps, snap.total_max_mbps
                ))
                .small()
                .color(widgets::weak(ui)),
            );
        }

        // Health legend.
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Card outline:")
                    .small()
                    .color(widgets::weak(ui)),
            );
            for (name, c) in [
                ("healthy", widgets::green(ui)),
                ("degraded", widgets::amber(ui)),
                ("failing", widgets::red(ui)),
            ] {
                let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 8.0), egui::Sense::hover());
                ui.painter().rect_stroke(
                    r,
                    2.0,
                    egui::Stroke::new(2.0, c),
                    egui::StrokeKind::Inside,
                );
                ui.label(RichText::new(name).small());
            }
            ui.label(
                RichText::new("· hover a card for the reason")
                    .small()
                    .color(widgets::weak(ui)),
            );
        });
        ui.add_space(4.0);

        self.cards_ui(ui, &snap, active.as_ref().map(|a| &a.layering), phase);

        // ---- Chart
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.chart, ChartMetric::Bitrate, "Bitrate");
            ui.selectable_value(&mut self.chart, ChartMetric::Resolution, "Resolution");
            ui.selectable_value(&mut self.chart, ChartMetric::FrameRate, "Frame rate");
            ui.label(RichText::new("last 2 min").small().color(widgets::weak(ui)));
        });
        let h = &snap.history;
        let s = |i| widgets::series(ui, i);
        let (series, unit): (Vec<Series>, &str) = match self.chart {
            ChartMetric::Bitrate => (
                vec![
                    Series {
                        name: "cap",
                        data: &h.cap,
                        color: s(0),
                        dashed: true,
                    },
                    Series {
                        name: "WebRTC target",
                        data: &h.target,
                        color: s(1),
                        dashed: false,
                    },
                    Series {
                        name: "sent",
                        data: &h.sent,
                        color: s(2),
                        dashed: false,
                    },
                    Series {
                        name: "received",
                        data: &h.received,
                        color: s(3),
                        dashed: false,
                    },
                ],
                "Mbps",
            ),
            ChartMetric::Resolution => (
                vec![
                    Series {
                        name: "captured",
                        data: &h.capture_h,
                        color: s(0),
                        dashed: true,
                    },
                    Series {
                        name: "encoded (top layer)",
                        data: &h.encoded_h,
                        color: s(2),
                        dashed: false,
                    },
                    Series {
                        name: "decoded",
                        data: &h.decoded_h,
                        color: s(3),
                        dashed: false,
                    },
                ],
                "px high",
            ),
            ChartMetric::FrameRate => (
                vec![
                    Series {
                        name: "captured",
                        data: &h.capture_fps,
                        color: s(0),
                        dashed: true,
                    },
                    Series {
                        name: "encoded",
                        data: &h.encode_fps,
                        color: s(1),
                        dashed: false,
                    },
                    Series {
                        name: "sent",
                        data: &h.sent_fps,
                        color: s(2),
                        dashed: false,
                    },
                    Series {
                        name: "decoded",
                        data: &h.decode_fps,
                        color: s(3),
                        dashed: false,
                    },
                ],
                "fps",
            ),
        };
        widgets::legend(ui, &series);
        widgets::line_chart(ui, 130.0, unit, &series, HISTORY);

        // ---- Previews
        ui.add_space(10.0);
        self.previews_ui(ui, &snap);

        // ---- Invite
        ui.add_space(10.0);
        ui.separator();
        self.invite_ui(ui, &snap, phase);
    }

    fn cards_ui(
        &mut self,
        ui: &mut egui::Ui,
        snap: &Snapshot,
        layering: Option<&Layering>,
        phase: Phase,
    ) {
        let top = snap.top_layer().cloned();
        let me = snap.self_receive().cloned();
        let health = snap.health.clone();
        let weak = widgets::weak(ui);
        let layered = matches!(layering, Some(Layering::Simulcast) | Some(Layering::Svc(_)));
        let mut ask = self.sub_ask;
        ui.columns(5, |cols| {
            stage_card(&mut cols[0], "1 · Capture", &health.capture, |ui| {
                ui.label(big(dims(snap.capture.width, snap.capture.height)));
                ui.label(format!("{:.1} fps", snap.capture.fps));
                ui.label(RichText::new(or_dash(&snap.capture.note)).small().color(weak));
            });
            stage_card(&mut cols[1], "2 · Encode", &health.encode, |ui| {
                match &top {
                    Some(t) => {
                        ui.label(big(format!("{:.1} fps", t.fps)));
                        ui.label(format!("{} · {}", or_dash(&t.encoder), hw(t.hardware)));
                        let limit = if t.limit.is_empty() || t.limit == "None" { "Not limited".to_string() } else { format!("Limited by {}", t.limit.to_lowercase()) };
                        let time = if t.hardware {
                            format!("{:.0} ms latency", t.encode_ms)
                        } else {
                            format!("{:.1} ms/frame", t.encode_ms)
                        };
                        ui.label(RichText::new(format!("{limit} · {time}")).small().color(weak)).on_hover_text(
                            "WebRTC's encode time per frame. A hardware encoder pipelines several frames, so for it this is latency, not load.",
                        );
                        if t.qp > 0.0 {
                            ui.label(RichText::new(format!("QP {:.0} · {}", t.qp, dims(t.width, t.height))).small().color(weak));
                        }
                    }
                    None => {
                        ui.label(big("–"));
                    }
                }
            });
            stage_card(&mut cols[2], "3 · Send", &health.send, |ui| {
                ui.label(big(format!("{:.2} Mbps", snap.total_sent_mbps)));
                if snap.total_target_mbps > 0.0 {
                    ui.label(format!("target {:.2}", snap.total_target_mbps));
                } else if matches!(layering, Some(Layering::Svc(_))) {
                    ui.label(RichText::new("target not reported for SVC").small().color(weak));
                }
                let n = snap.layers.iter().filter(|l| l.active).count();
                let text = match layering {
                    Some(Layering::Svc(m)) => format!("{m}: all layers in 1 stream"),
                    Some(Layering::Simulcast) => format!("{n} simulcast layer{}", if n == 1 { "" } else { "s" }),
                    _ if n > 0 => "1 stream".to_string(),
                    _ => "–".to_string(),
                };
                ui.label(RichText::new(text).small().color(weak));
                if snap.retransmit_mbps > 0.005 {
                    ui.label(RichText::new(format!("retransmit {:.2} Mbps", snap.retransmit_mbps)).small().color(weak));
                }
            });
            stage_card(&mut cols[3], "4 · SFU forwards", &health.sfu, |ui| {
                match &me {
                    Some(r) => {
                        ui.label(big(dims(r.width, r.height)));
                    }
                    None => {
                        ui.label(big("–"));
                    }
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Subscriber asks for").small());
                    live_badge(ui);
                });
                ui.add_enabled_ui(layered && phase.connected(), |ui| {
                    egui::ComboBox::from_id_salt("sub-ask")
                        .selected_text(ask.label())
                        .width(ui.available_width().min(170.0))
                        .show_ui(ui, |ui| {
                            for a in [SubAsk::Top, SubAsk::FitTile, SubAsk::Half, SubAsk::Quarter] {
                                ui.selectable_value(&mut ask, a, a.label());
                            }
                        });
                })
                .response
                .on_hover_text("The app's subscriber tells the SFU what size it wants; the SFU forwards the smallest simulcast or SVC layer that covers it. Fit the far-end tile follows the preview's size in physical pixels, at most every 5 s and only when it changes.")
                .on_disabled_hover_text("Publish with simulcast or SVC: with a single layer the SFU has nothing to choose from.");
            });
            stage_card(&mut cols[4], "5 · Receive and decode", &health.receive, |ui| {
                match &me {
                    Some(r) => {
                        ui.label(big(format!("{:.1} fps", r.fps)));
                        let hwtext = match r.hardware {
                            Some(true) => "HW",
                            Some(false) => "SW",
                            None => "?",
                        };
                        ui.label(format!("{} · {}", or_dash(&r.decoder), hwtext));
                        ui.label(RichText::new(format!("{:.2} Mbps · {:.1} ms/frame", r.mbps, r.decode_ms)).small().color(weak));
                    }
                    None => {
                        ui.label(big("–"));
                    }
                }
            });
        });
        if ask != self.sub_ask {
            self.set_sub_ask(ask);
        }
        // What was asked and what arrives.
        let received = me.as_ref().map(|r| (r.width, r.height)).filter(|d| d.0 > 0);
        let arriving = received
            .map(|(w, h)| format!(" · receiving {w}×{h}"))
            .unwrap_or_default();
        let text = if !layered {
            "Single layer: the SFU forwards the only layer there is. Publish with simulcast or SVC to choose.".to_string()
        } else {
            match (self.sub_ask, snap.requested_size) {
                (SubAsk::Top, _) => format!("Asking for the top layer{arriving}"),
                (SubAsk::FitTile, None) => "Waiting for the far-end tile size…".to_string(),
                (SubAsk::FitTile, Some((w, h))) => {
                    let full = self
                        .session
                        .shared
                        .lock()
                        .active_publish
                        .as_ref()
                        .map(|a| a.publish_dims());
                    let note = match full {
                        Some((fw, fh)) if w >= fw && h >= fh => {
                            " (tile is at least the video size, so the top layer)"
                        }
                        _ => "",
                    };
                    let ago = snap
                        .requested_ago_s
                        .map(|s| format!(" · sent {s:.0} s ago"))
                        .unwrap_or_default();
                    format!("Asking {w}×{h} px for the far-end tile{note}{arriving}{ago}")
                }
                (_, Some((w, h))) => format!("Asking {w}×{h}{arriving}"),
                (_, None) => format!("Asking…{arriving}"),
            }
        };
        ui.label(RichText::new(text).small().color(widgets::weak(ui)));
    }

    pub(super) fn set_sub_ask(&mut self, ask: SubAsk) {
        self.sub_ask = ask;
        self.throttle.reset();
        self.ask_sent = None;
        if ask == SubAsk::Top {
            self.session.request_size(None);
        }
        // Fit tile is sent from observe_tile; Half and Quarter from keep_sub_ask.
    }

    /// Run every frame: trailing tile updates, and fixed fractions that must
    /// follow the publish size.
    pub(super) fn keep_sub_ask(&mut self) {
        if !self.session.phase().connected() {
            return;
        }
        match self.sub_ask {
            SubAsk::FitTile => {
                if let Some(size) = self.throttle.poll(Instant::now()) {
                    self.session.request_size(Some(size));
                }
            }
            SubAsk::Half | SubAsk::Quarter => {
                let div = if self.sub_ask == SubAsk::Half { 2 } else { 4 };
                let full = self
                    .session
                    .shared
                    .lock()
                    .active_publish
                    .as_ref()
                    .map(|a| a.publish_dims());
                if let Some((w, h)) = full {
                    let want = ((w / div) & !1, (h / div) & !1);
                    if self.ask_sent != Some(want) {
                        self.ask_sent = Some(want);
                        self.session.request_size(Some(want));
                    }
                }
            }
            SubAsk::Top => {}
        }
    }

    /// Called with the far-end tile size in physical pixels each frame it is shown.
    pub(super) fn observe_tile(&mut self, rect: egui::Rect, ppp: f32) {
        if self.sub_ask != SubAsk::FitTile || !self.session.phase().connected() {
            return;
        }
        let w = ((rect.width() * ppp) as u32) & !1;
        let h = ((rect.height() * ppp) as u32) & !1;
        if w < 16 || h < 16 {
            return;
        }
        if let Some(size) = self.throttle.observe((w, h), Instant::now()) {
            self.session.request_size(Some(size));
        }
    }

    fn previews_ui(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Previews").strong());
            ui.selectable_value(&mut self.layout, PreviewLayout::SideBySide, "Side by side");
            ui.selectable_value(&mut self.layout, PreviewLayout::LocalOnly, "Local only");
            ui.selectable_value(&mut self.layout, PreviewLayout::FarOnly, "Far end only");
            ui.checkbox(&mut self.previews, "Show").on_hover_text(
                "Previews cost some CPU at large sizes. Applies from the next connection.",
            );
        });
        ui.label(
            RichText::new("Undock opens a preview in its own resizable window; closing that window docks it back.")
                .small()
                .color(widgets::weak(ui)),
        );
        let show_local = self.layout != PreviewLayout::FarOnly;
        let show_far = self.layout != PreviewLayout::LocalOnly;
        let count = show_local as usize + show_far as usize;
        let (local_slot, remote_slot) = {
            let s = self.session.shared.lock();
            (s.local_preview.clone(), s.remote_preview.clone())
        };
        let local_caption = format!(
            "{} · {:.0} fps",
            dims(snap.capture.width, snap.capture.height),
            snap.capture.fps
        );
        let far_caption = match snap.self_receive() {
            Some(r) => format!("{} · {:.1} fps", dims(r.width, r.height), r.fps),
            None => "–".into(),
        };
        let gap = 8.0;
        let width = ((ui.available_width() - gap * (count as f32 - 1.0)) / count as f32).max(120.0);
        let height = (width * 9.0 / 16.0).min(360.0);
        let mut tile: Option<egui::Rect> = None;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            if show_local {
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Local · into the encoder").strong());
                        ui.label(
                            RichText::new(&local_caption)
                                .small()
                                .color(widgets::weak(ui)),
                        );
                        if !self.local_undocked && ui.small_button("Undock").clicked() {
                            self.local_undocked = true;
                        }
                    });
                    if self.local_undocked {
                        self.docked_placeholder(ui, width, height, true);
                    } else {
                        self.local_pane.paint(
                            ui,
                            &local_slot,
                            "local",
                            egui::vec2(width, height),
                            "No local video",
                        );
                    }
                });
            }
            if show_far {
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Far end · decoded").strong());
                        ui.label(RichText::new(&far_caption).small().color(widgets::weak(ui)));
                        if self.sub_ask == SubAsk::FitTile {
                            widgets::badge(ui, "FITS TILE", super::theme::ACCENT_TEXT, super::theme::ACCENT_BG)
                                .on_hover_text("The SFU is asked for the layer that fits this preview, docked or undocked.");
                        }
                        if !self.remote_undocked && ui.small_button("Undock").clicked() {
                            self.remote_undocked = true;
                        }
                    });
                    if self.remote_undocked {
                        self.docked_placeholder(ui, width, height, false);
                    } else {
                        tile = Some(self.remote_pane.paint(
                            ui,
                            &remote_slot,
                            "remote",
                            egui::vec2(width, height),
                            "No decoded video",
                        ));
                    }
                });
            }
        });
        if let Some(r) = tile {
            let ppp = ui.ctx().pixels_per_point();
            self.observe_tile(r, ppp);
        }
    }

    fn docked_placeholder(&mut self, ui: &mut egui::Ui, w: f32, h: f32, local: bool) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h.min(80.0)), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            if super::main_is_fullscreen(ui.ctx()) {
                "Shown in a floating panel"
            } else {
                "Shown in its own window"
            },
            egui::FontId::proportional(13.0),
            widgets::weak(ui),
        );
        if ui.small_button("Dock back").clicked() {
            if local {
                self.local_undocked = false;
            } else {
                self.remote_undocked = false;
            }
        }
    }

    /// Undocked previews, each in its own native window.
    /// Undocked previews. Each opens in its own native window, or, while the
    /// main window is full screen, as a floating panel inside it: macOS puts
    /// full-screen apps in their own Space, where extra windows misbehave.
    pub(super) fn preview_windows(&mut self, ctx: &egui::Context) {
        let (local_slot, remote_slot) = {
            let s = self.session.shared.lock();
            (s.local_preview.clone(), s.remote_preview.clone())
        };
        let embedded = super::main_is_fullscreen(ctx);
        if self.local_undocked {
            let (close, _) = show_preview_window(
                ctx,
                embedded,
                "local-preview",
                "Local · into the encoder",
                &mut self.local_pane,
                &local_slot,
                "No local video",
            );
            if close {
                self.local_undocked = false;
            }
        }
        if self.remote_undocked {
            let (close, tile) = show_preview_window(
                ctx,
                embedded,
                "far-preview",
                "Far end · decoded",
                &mut self.remote_pane,
                &remote_slot,
                "No decoded video",
            );
            if let Some((r, ppp)) = tile {
                self.observe_tile(r, ppp);
            }
            if close {
                self.remote_undocked = false;
            }
        }
    }

    fn invite_ui(&mut self, ui: &mut egui::Ui, snap: &Snapshot, phase: Phase) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Invite a browser viewer").strong());
            match &self.viewer {
                Some(v) => {
                    ui.colored_label(
                        widgets::green(ui),
                        format!("● Local HTTP server on port {}", v.port()),
                    );
                    if ui.small_button("Stop server").clicked() {
                        self.viewer = None;
                        self.invite_note.clear();
                    }
                }
                None => {
                    ui.label(RichText::new("○ Local HTTP server stopped").color(widgets::weak(ui)));
                    if ui.small_button("Start server").clicked() {
                        self.start_viewer_server();
                    }
                }
            }
        });
        let can = self.viewer.is_some() && phase.connected();
        ui.checkbox(&mut self.browser_adaptive, "Browser asks optimal resolution").on_hover_text(
            "Applies to the next viewer you open. The page uses the LiveKit JS SDK's adaptive stream: it tells the SFU the size of its video element, and the SFU forwards the smallest layer that fills it. Needs simulcast or SVC to have a choice of layers.",
        );
        ui.horizontal_wrapped(|ui| {
            let reason = if self.viewer.is_none() { "Start the local HTTP server first." } else { "Connect first: the link joins the current room." };
            if ui.add_enabled(can, egui::Button::new("Open in Chrome")).on_disabled_hover_text(reason).clicked() {
                self.invite(Some(crate::viewer::chrome_name()));
            }
            if ui.add_enabled(can, egui::Button::new("Default browser")).on_disabled_hover_text(reason).clicked() {
                self.invite(None);
            }
            if ui.add_enabled(can, egui::Button::new("Copy link")).on_disabled_hover_text(reason).clicked() {
                if let Some(link) = self.invite_link() {
                    ui.ctx().copy_text(link);
                    self.invite_note = "Link copied. It opens on this computer only (the page is served on 127.0.0.1).".into();
                }
            }
        });
        if !self.invite_note.is_empty() {
            ui.label(
                RichText::new(&self.invite_note)
                    .small()
                    .color(widgets::weak(ui)),
            );
        }
        let viewers: Vec<String> = snap
            .receive
            .iter()
            .filter(|r| r.is_browser)
            .map(|r| {
                if r.user_agent.is_empty() {
                    r.who.clone()
                } else {
                    format!("{} ({})", r.who, r.user_agent)
                }
            })
            .collect();
        if !viewers.is_empty() {
            ui.label(
                RichText::new(format!(
                    "Browser viewers reporting stats: {}",
                    viewers.join(", ")
                ))
                .small(),
            );
        }
    }
}

/// One undocked preview. Returns (dock back, tile rect and pixels per point).
fn show_preview_window(
    ctx: &egui::Context,
    embedded: bool,
    key: &str,
    title: &str,
    pane: &mut PreviewPane,
    slot: &SharedPreview,
    placeholder: &str,
) -> (bool, Option<(egui::Rect, f32)>) {
    let mut tile = None;
    if embedded {
        let mut open = true;
        egui::Window::new(title)
            .id(egui::Id::unique((key, "embedded")))
            .open(&mut open)
            .resizable(true)
            .collapsible(false)
            .default_size([800.0, 450.0])
            .min_size([240.0, 135.0])
            .show(ctx, |ui| {
                let size = ui.available_size().max(egui::vec2(240.0, 135.0));
                let r = pane.paint(ui, slot, &format!("{key}-panel"), size, placeholder);
                tile = Some((r, ui.ctx().pixels_per_point()));
            });
        return (!open, tile);
    }
    let builder = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size([800.0, 450.0])
        .with_min_inner_size([240.0, 135.0]);
    let mut close = false;
    ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(key), builder, |ui, _| {
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let size = ui.available_size();
            let r = pane.paint(ui, slot, &format!("{key}-window"), size, placeholder);
            tile = Some((r, ui.ctx().pixels_per_point()));
        });
        // Do not let the window be torn down from under the frame: cancel the
        // close and dock back, which removes the window on the next frame.
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            close = true;
        }
    });
    (close, tile)
}
