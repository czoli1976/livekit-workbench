//! The Stats & Telemetry window: per-layer send stats, per-subscriber receive
//! stats, transport, events, the local server, and what the SDK sends.

use std::io::Write;
use std::path::PathBuf;

use eframe::egui::{self, RichText};

use super::widgets::{self, dims, kv, or_dash, Series};
use super::App;
use crate::local_server::ServerState;
use crate::stats::{Snapshot, HISTORY};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StatsTab {
    #[default]
    Stats,
    Server,
    Telemetry,
}

#[derive(Default)]
pub struct StatsUi {
    pub tab: StatsTab,
    pub paused: Option<Snapshot>,
    pub recorder: Option<Recorder>,
    pub note: String,
}

pub struct Recorder {
    pub path: PathBuf,
    file: std::fs::File,
    last_seq: u64,
    pub rows: u64,
}

impl Recorder {
    fn start() -> std::io::Result<Self> {
        let dir = directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let name = format!(
            "workbench-{}.csv",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path)?;
        writeln!(
            file,
            "time,capture_w,capture_h,capture_fps,top_w,top_h,encode_fps,encoder,hardware,encode_ms,qp,limit,cap_mbps,target_mbps,sent_mbps,retransmit_mbps,recv_w,recv_h,decode_fps,recv_mbps,loss_ratio,freezes,rtt_ms,available_out_mbps"
        )?;
        Ok(Self {
            path,
            file,
            last_seq: 0,
            rows: 0,
        })
    }

    fn record(&mut self, s: &Snapshot) {
        if s.seq == self.last_seq {
            return;
        }
        self.last_seq = s.seq;
        let top = s.top_layer().cloned().unwrap_or_default();
        let me = s.self_receive().cloned().unwrap_or_default();
        let line = format!(
            "{},{},{},{:.2},{},{},{:.2},{},{},{:.2},{:.1},{},{:.3},{:.3},{:.3},{:.3},{},{},{:.2},{:.3},{:.4},{},{:.1},{:.3}",
            chrono::Local::now().format("%H:%M:%S"),
            s.capture.width,
            s.capture.height,
            s.capture.fps,
            top.width,
            top.height,
            top.fps,
            top.encoder.replace(',', ";"),
            top.hardware,
            top.encode_ms,
            top.qp,
            top.limit,
            s.total_max_mbps,
            s.total_target_mbps,
            s.total_sent_mbps,
            s.retransmit_mbps,
            me.width,
            me.height,
            me.fps,
            me.mbps,
            me.loss_ratio,
            me.freezes,
            s.transport.rtt_ms,
            s.transport.available_out_mbps,
        );
        if writeln!(self.file, "{line}").is_ok() {
            self.rows += 1;
        }
    }
}

impl App {
    pub(super) fn stats_window(&mut self, ctx: &egui::Context) {
        // Keep recording whether or not the window is open.
        if let Some(r) = &mut self.stats_ui.recorder {
            let snap = self.session.shared.lock().snapshot.clone();
            r.record(&snap);
        }
        if !self.show_stats {
            return;
        }
        let id = egui::ViewportId::from_hash_of("stats-window");
        let builder = egui::ViewportBuilder::default()
            .with_title("Stats & Telemetry · Workbench for LiveKit")
            .with_inner_size([1100.0, 780.0])
            .with_min_inner_size([640.0, 400.0]);
        if super::main_is_fullscreen(ctx) {
            // Full screen: a floating panel inside the main window (see previews).
            let mut open = true;
            egui::Window::new("Stats & Telemetry")
                .id(egui::Id::unique("stats-embedded"))
                .open(&mut open)
                .resizable(true)
                .collapsible(false)
                .default_size([1000.0, 700.0])
                .show(ctx, |ui| {
                    self.stats_toolbar(ui);
                    ui.separator();
                    self.stats_body(ui);
                });
            if !open {
                self.show_stats = false;
            }
            return;
        }
        let mut close = false;
        ctx.show_viewport_immediate(id, builder, |ui, _| {
            // A child window repaints only when asked: refresh with each sample.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(500));
            egui::Panel::top("stats-top")
                .frame(super::theme::top_frame())
                .show(ui, |ui| {
                    self.stats_toolbar(ui);
                });
            egui::CentralPanel::default_margins()
                .frame(super::theme::central_frame())
                .show(ui, |ui| {
                    self.stats_body(ui);
                });
            // Cancel the OS close and stop showing the window instead (see previews).
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                close = true;
            }
        });
        if close {
            self.show_stats = false;
        }
    }

    fn stats_body(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| match self.stats_ui.tab {
                StatsTab::Stats => self.stats_tab(ui),
                StatsTab::Server => self.server_tab(ui),
                StatsTab::Telemetry => telemetry_tab(ui),
            });
    }

    fn stats_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("Stats & Telemetry");
            ui.add_space(12.0);
            ui.selectable_value(&mut self.stats_ui.tab, StatsTab::Stats, "Stats");
            let local = self.current_profile().is_local();
            ui.add_enabled_ui(local, |ui| {
                ui.selectable_value(&mut self.stats_ui.tab, StatsTab::Server, "Server");
            })
            .response
            .on_disabled_hover_text(
                "Shown for the Local dev profile, where the app starts and owns the server.",
            );
            ui.selectable_value(&mut self.stats_ui.tab, StatsTab::Telemetry, "Telemetry");
            ui.separator();
            let paused = self.stats_ui.paused.is_some();
            if ui
                .selectable_label(paused, if paused { "▶ Resume" } else { "⏸ Pause" })
                .clicked()
            {
                self.stats_ui.paused = if paused {
                    None
                } else {
                    Some(self.session.shared.lock().snapshot.clone())
                };
            }
            let rec = self.stats_ui.recorder.is_some();
            if ui
                .selectable_label(
                    rec,
                    if rec {
                        "■ Stop CSV"
                    } else {
                        "● Record CSV"
                    },
                )
                .clicked()
            {
                if let Some(r) = self.stats_ui.recorder.take() {
                    self.stats_ui.note = format!("Saved {} rows to {}", r.rows, r.path.display());
                } else {
                    match Recorder::start() {
                        Ok(r) => {
                            self.stats_ui.note =
                                format!("Recording one row per second to {}", r.path.display());
                            self.stats_ui.recorder = Some(r);
                        }
                        Err(e) => {
                            self.stats_ui.note = format!("Could not create the CSV file: {e}")
                        }
                    }
                }
            }
            if ui
                .button("Copy raw JSON")
                .on_hover_text("The current snapshot, including the 2-minute history.")
                .clicked()
            {
                let snap = self
                    .stats_ui
                    .paused
                    .clone()
                    .unwrap_or_else(|| self.session.shared.lock().snapshot.clone());
                match serde_json::to_string_pretty(&snap) {
                    Ok(j) => {
                        ui.ctx().copy_text(j);
                        self.stats_ui.note = "Snapshot copied as JSON.".into();
                    }
                    Err(e) => self.stats_ui.note = format!("Could not serialise: {e}"),
                }
            }
        });
        if !self.stats_ui.note.is_empty() {
            ui.label(
                RichText::new(&self.stats_ui.note)
                    .small()
                    .color(widgets::weak(ui)),
            );
        }
        if self.stats_ui.tab == StatsTab::Server && !self.current_profile().is_local() {
            self.stats_ui.tab = StatsTab::Stats;
        }
    }

    fn stats_tab(&mut self, ui: &mut egui::Ui) {
        let (snap, active) = {
            let s = self.session.shared.lock();
            (
                self.stats_ui
                    .paused
                    .clone()
                    .unwrap_or_else(|| s.snapshot.clone()),
                s.active_publish.clone(),
            )
        };
        if let Some(a) = &active {
            let (w, h) = a.publish_dims();
            ui.label(
                RichText::new(format!(
                    "{} · {:?} · {}×{} · cap {:.1} Mbps · source: {} · {}",
                    crate::capabilities::codec_name(a.codec),
                    a.layering,
                    w,
                    h,
                    a.bitrate_bps as f64 / 1e6,
                    if a.screen_share {
                        "screen share"
                    } else {
                        "camera"
                    },
                    super::form::degradation_name(a.degradation).to_lowercase(),
                ))
                .color(widgets::weak(ui)),
            );
        }

        // Summary tiles.
        let me = snap.self_receive().cloned();
        ui.columns(6, |c| {
            tile(
                &mut c[0],
                "Sent, all layers",
                format!("{:.2} Mbps", snap.total_sent_mbps),
                format!("cap {:.2}", snap.total_max_mbps),
            );
            tile(
                &mut c[1],
                "Bandwidth estimate",
                format!("{:.2} Mbps", snap.transport.available_out_mbps),
                if snap.total_max_mbps > 0.0
                    && snap.transport.available_out_mbps < snap.total_max_mbps * 0.9
                {
                    "below cap".into()
                } else {
                    "available outgoing".into()
                },
            );
            tile(
                &mut c[2],
                "Round trip",
                format!("{:.0} ms", snap.transport.rtt_ms),
                or_dash(&snap.transport.local_candidate),
            );
            tile(
                &mut c[3],
                "Received",
                me.as_ref()
                    .map(|r| format!("{:.2} Mbps", r.mbps))
                    .unwrap_or_else(|| "–".into()),
                me.as_ref()
                    .map(|r| dims(r.width, r.height))
                    .unwrap_or_default(),
            );
            tile(
                &mut c[4],
                "Packet loss",
                me.as_ref()
                    .map(|r| format!("{:.1} %", r.loss_ratio * 100.0))
                    .unwrap_or_else(|| "–".into()),
                me.as_ref()
                    .map(|r| format!("{} lost · {} NACK", r.lost, r.nack))
                    .unwrap_or_default(),
            );
            tile(
                &mut c[5],
                "Freezes",
                me.as_ref()
                    .map(|r| r.freezes.to_string())
                    .unwrap_or_else(|| "–".into()),
                me.as_ref()
                    .map(|r| format!("{:.1} s total", r.freeze_s))
                    .unwrap_or_default(),
            );
        });

        // Send table.
        ui.add_space(10.0);
        ui.label(RichText::new("Send · one row per RTP stream").strong());
        if snap.layers.is_empty() {
            ui.label(RichText::new("Not publishing.").color(widgets::weak(ui)));
        } else {
            egui::Grid::new("send-table")
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    for h in [
                        "Layer",
                        "Resolution target → actual",
                        "Frame rate target → actual",
                        "Bitrate cap → target → sent",
                        "Encoder",
                        "Encode",
                        "Degradation",
                        "Keyframes · PLI · NACK",
                    ] {
                        ui.label(RichText::new(h).small().strong());
                    }
                    ui.end_row();
                    for l in &snap.layers {
                        let name = if l.rid.is_empty() {
                            if l.scalability_mode.is_empty() {
                                "single".to_string()
                            } else {
                                l.scalability_mode.clone()
                            }
                        } else {
                            format!("{} · {}", l.rid, l.label())
                        };
                        let state = if l.active { "" } else { " (paused)" };
                        ui.label(format!("{name}{state}"));
                        ui.label(format!(
                            "{} → {}",
                            dims(l.target_w, l.target_h),
                            dims(l.width, l.height)
                        ));
                        ui.label(format!("{:.0} → {:.1}", l.target_fps, l.fps));
                        ui.label(format!(
                            "{:.2} → {:.2} → {:.2}",
                            l.max_mbps, l.target_mbps, l.sent_mbps
                        ));
                        ui.label(format!(
                            "{} · {}",
                            or_dash(&l.encoder),
                            if l.hardware { "HW" } else { "SW" }
                        ));
                        ui.label(if l.encode_ms > 0.0 {
                            format!("{:.1} ms · QP {:.0}", l.encode_ms, l.qp)
                        } else {
                            "–".into()
                        });
                        let limited = !(l.limit.is_empty() || l.limit == "None");
                        let lim = format!(
                            "{} · {} size change{}",
                            if limited {
                                l.limit.clone()
                            } else {
                                "None".into()
                            },
                            l.resolution_changes,
                            if l.resolution_changes == 1 { "" } else { "s" }
                        );
                        if limited {
                            ui.colored_label(widgets::amber(ui), lim);
                        } else {
                            ui.label(lim);
                        }
                        ui.label(format!("{} · {} · {}", l.keyframes, l.pli, l.nack));
                        ui.end_row();
                    }
                });
            ui.label(
                RichText::new("With SVC there is a single row: WebRTC stats do not break out spatial layers inside one stream. Amber marks a layer the encoder is currently degrading.")
                    .small()
                    .color(widgets::weak(ui)),
            );
        }

        // Per-layer chart and degradation shares.
        ui.add_space(8.0);
        ui.columns(2, |c| {
            let ui = &mut c[0];
            ui.label(RichText::new("Bitrate, total and per layer · Mbps").strong());
            let mut series = vec![
                Series {
                    name: "cap",
                    data: &snap.history.cap,
                    color: widgets::series(ui, 0),
                    dashed: true,
                },
                Series {
                    name: "total sent",
                    data: &snap.history.sent,
                    color: widgets::series(ui, 2),
                    dashed: false,
                },
            ];
            for (i, (rid, data)) in snap.history.layer_sent.iter().enumerate() {
                if snap.history.layer_sent.len() > 1 {
                    series.push(Series {
                        name: rid_name(rid),
                        data,
                        color: widgets::series(ui, 3 + i),
                        dashed: false,
                    });
                }
            }
            widgets::legend(ui, &series);
            widgets::line_chart(ui, 140.0, "Mbps", &series, HISTORY);

            let ui = &mut c[1];
            ui.label(RichText::new("Why it degraded · share of limited time, top layer").strong());
            match snap.top_layer() {
                Some(t) => {
                    for (name, share) in [
                        ("Bandwidth", t.limit_share_bandwidth),
                        ("CPU", t.limit_share_cpu),
                        ("Other", t.limit_share_other),
                    ] {
                        ui.horizontal(|ui| {
                            ui.add_sized([80.0, 16.0], egui::Label::new(name));
                            ui.add(
                                egui::ProgressBar::new(share as f32)
                                    .desired_width(200.0)
                                    .text(format!("{:.0}%", share * 100.0)),
                            );
                        });
                    }
                    ui.label(
                        RichText::new(format!(
                            "From WebRTC's quality limitation durations. Resolution changes: {}.",
                            t.resolution_changes
                        ))
                        .small()
                        .color(widgets::weak(ui)),
                    );
                }
                None => {
                    ui.label(RichText::new("Not publishing.").color(widgets::weak(ui)));
                }
            }
        });

        // Receive table.
        ui.add_space(10.0);
        ui.label(RichText::new("Receive · per subscriber").strong());
        if snap.receive.is_empty() {
            ui.label(RichText::new("No subscriber is receiving yet.").color(widgets::weak(ui)));
        } else {
            egui::Grid::new("recv-table")
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    for h in [
                        "Subscriber",
                        "Asked for",
                        "Decoded",
                        "Received",
                        "Codec · decoder",
                        "Decode",
                        "Jitter buffer",
                        "Freezes",
                        "Lost · NACK · PLI",
                        "FEC recovered",
                    ] {
                        ui.label(RichText::new(h).small().strong());
                    }
                    ui.end_row();
                    for r in &snap.receive {
                        let who = if r.is_browser {
                            format!(
                                "{} ({})",
                                r.who,
                                if r.user_agent.is_empty() {
                                    "browser"
                                } else {
                                    &r.user_agent
                                }
                            )
                        } else {
                            r.who.clone()
                        };
                        ui.label(who);
                        ui.label(match r.requested {
                            Some((w, h)) => format!("{w}×{h}"),
                            None => "top layer".into(),
                        });
                        ui.label(format!("{} @ {:.1}", dims(r.width, r.height), r.fps));
                        ui.label(format!("{:.2} Mbps", r.mbps));
                        let hwt = match r.hardware {
                            Some(true) => " · HW",
                            Some(false) => " · SW",
                            None => "",
                        };
                        ui.label(format!(
                            "{} · {}{}",
                            or_dash(&r.codec),
                            or_dash(&r.decoder),
                            hwt
                        ));
                        ui.label(if r.decode_ms > 0.0 {
                            format!("{:.1} ms", r.decode_ms)
                        } else {
                            "–".into()
                        });
                        ui.label(if r.jitter_buffer_ms > 0.0 {
                            format!("{:.0} ms", r.jitter_buffer_ms)
                        } else {
                            "–".into()
                        });
                        ui.label(format!("{} · {:.1} s", r.freezes, r.freeze_s));
                        ui.label(format!("{} · {} · {}", r.lost, r.nack, r.pli));
                        ui.label(if snap.fec_negotiated || r.fec_recovered > 0 {
                            r.fec_recovered.to_string()
                        } else {
                            "not negotiated".into()
                        });
                        ui.end_row();
                    }
                });
        }
        ui.label(
            RichText::new("Browser rows come from the viewer page, which sends its own WebRTC stats over the room's data channel: an app cannot read another participant's stats.")
                .small()
                .color(widgets::weak(ui)),
        );

        // Transport and events.
        ui.add_space(10.0);
        ui.columns(2, |c| {
            let ui = &mut c[0];
            ui.label(RichText::new("Transport · publisher").strong());
            egui::Grid::new("transport")
                .num_columns(2)
                .spacing([12.0, 3.0])
                .show(ui, |ui| {
                    let t = &snap.transport;
                    kv(
                        ui,
                        "Candidate pair",
                        format!(
                            "{} ↔ {}",
                            or_dash(&t.local_candidate),
                            or_dash(&t.remote_candidate)
                        ),
                    );
                    kv(
                        ui,
                        "Available outgoing",
                        format!("{:.2} Mbps", t.available_out_mbps),
                    );
                    kv(ui, "Current RTT", format!("{:.0} ms", t.rtt_ms));
                    kv(
                        ui,
                        "DTLS · SRTP",
                        format!("{} · {}", or_dash(&t.dtls), or_dash(&t.srtp)),
                    );
                    kv(ui, "Pair changes", t.pair_changes.to_string());
                    kv(
                        ui,
                        "Server rating",
                        format!(
                            "publisher {} · subscriber {}",
                            snap.server_quality_pub.as_deref().unwrap_or("–"),
                            snap.server_quality_sub.as_deref().unwrap_or("–")
                        ),
                    );
                    kv(
                        ui,
                        "Video FEC",
                        if snap.fec.is_empty() {
                            "none negotiated (NACK and RTX only)".to_string()
                        } else {
                            format!("{} negotiated", snap.fec.join(", "))
                        },
                    );
                });
            let ui = &mut c[1];
            ui.label(RichText::new("Event log").strong());
            egui::ScrollArea::vertical()
                .id_salt("events")
                .max_height(170.0)
                .show(ui, |ui| {
                    if snap.events.is_empty() {
                        ui.label(RichText::new("No events yet.").color(widgets::weak(ui)));
                    }
                    for (t, e) in &snap.events {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(t)
                                    .monospace()
                                    .small()
                                    .color(widgets::weak(ui)),
                            );
                            ui.label(RichText::new(e).small());
                        });
                    }
                });
        });
    }

    fn server_tab(&mut self, ui: &mut egui::Ui) {
        let state = self.server.state();
        let st = self.server.stats();
        ui.label(RichText::new("Local dev server").strong());
        ui.horizontal_wrapped(|ui| {
            let (text, color) = match &state {
                ServerState::Running => (
                    format!(
                        "livekit-server {} · running · up {}",
                        or_dash(&st.version),
                        st.started_at
                            .map(|t| fmt_uptime(t.elapsed().as_secs()))
                            .unwrap_or_else(|| "–".into())
                    ),
                    widgets::green(ui),
                ),
                ServerState::External => (
                    "A server is already listening on the port; using it (not started by the app)"
                        .into(),
                    widgets::amber(ui),
                ),
                ServerState::Starting => ("Starting…".into(), widgets::amber(ui)),
                ServerState::Stopped => ("Stopped".into(), widgets::weak(ui)),
                ServerState::Failed(e) => (format!("Failed: {e}"), widgets::red(ui)),
            };
            ui.colored_label(color, text);
            self.server_buttons(ui);
            self.check_version_button(ui);
            self.server_update_ui(ui);
        });
        ui.add_space(6.0);
        let f =
            |v: Option<f64>, d: usize| v.map(|x| format!("{x:.d$}")).unwrap_or_else(|| "–".into());
        ui.columns(6, |c| {
            tile(
                &mut c[0],
                "Server CPU",
                format!("{} %", f(st.cpu_percent, 0)),
                "of one core".into(),
            );
            tile(
                &mut c[1],
                "Server memory",
                format!("{} MB", f(st.memory_mb, 0)),
                "resident".into(),
            );
            tile(
                &mut c[2],
                "Rooms · participants",
                format!(
                    "{} · {}",
                    st.rooms.map(|r| r.to_string()).unwrap_or("–".into()),
                    st.participants.map(|r| r.to_string()).unwrap_or("–".into())
                ),
                st.participant_names.join(", "),
            );
            tile(
                &mut c[3],
                "Server in · out",
                format!("{} · {}", f(st.mbps_in, 1), f(st.mbps_out, 1)),
                "Mbps".into(),
            );
            tile(
                &mut c[4],
                "Packets in · out",
                format!(
                    "{} · {}",
                    f(st.packets_in_per_s, 0),
                    f(st.packets_out_per_s, 0)
                ),
                "per second".into(),
            );
            tile(
                &mut c[5],
                "Warnings · errors",
                format!("{} · {}", st.warnings, st.errors),
                "in the log since start".into(),
            );
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Where these come from: CPU, memory, traffic and packets from the server's Prometheus endpoint (the app turns it on, on port +1000); rooms and participants from the server's room API with the dev key; warnings from the server log the app captures.",
            )
            .small()
            .color(widgets::weak(ui)),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Server log").strong());
            if ui.small_button("Copy log").clicked() {
                let text: Vec<String> = st
                    .log
                    .iter()
                    .map(|l| format!("{} {}", l.level, l.text))
                    .collect();
                ui.ctx().copy_text(text.join("\n"));
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("server-log")
            .max_height(300.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if st.log.is_empty() {
                    ui.label(RichText::new("Nothing logged yet.").color(widgets::weak(ui)));
                }
                for l in &st.log {
                    let color = match l.level.as_str() {
                        "WARN" => widgets::amber(ui),
                        "ERROR" | "FATAL" => widgets::red(ui),
                        _ => ui.visuals().text_color(),
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!("{:5}", l.level))
                                .monospace()
                                .small()
                                .color(color),
                        );
                        ui.label(RichText::new(&l.text).monospace().small());
                    });
                }
            });
    }
}

fn tile(ui: &mut egui::Ui, title: &str, value: String, sub: String) {
    egui::Frame::group(ui.style())
        .corner_radius(6.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).small().color(widgets::weak(ui)));
            ui.label(RichText::new(value).size(16.0).strong());
            ui.label(RichText::new(sub).small().color(widgets::weak(ui)));
        });
}

fn rid_name(rid: &str) -> &'static str {
    match rid {
        "f" => "layer f",
        "h" => "layer h",
        "q" => "layer q",
        _ => "stream",
    }
}

fn fmt_uptime(s: u64) -> String {
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        format!("{} h {} min", s / 3600, (s / 60) % 60)
    }
}

/// Reference: what the Rust SDK sends, read from its source. Not a live capture.
fn telemetry_tab(ui: &mut egui::Ui) {
    ui.label(RichText::new("Telemetry: what this client sends").strong());
    ui.label(
        RichText::new(format!(
            "Read from the source of livekit {} (the SDK this build uses). Reference material, not a live capture: the SDK has no hook to observe its signalling messages.",
            crate::version::LIVEKIT_SDK
        ))
        .small()
        .color(widgets::weak(ui)),
    );
    ui.add_space(6.0);
    let rows: [(&str, &str, &str, &str); 9] = [
        (
            "Join request",
            "signal WebSocket URL",
            "Connect, and every reconnect",
            "sdk=rust · version · protocol · os · os_version · device_model · auto_subscribe · adaptive_stream; on reconnect also reconnect=1 · sid · reconnect_reason. Plus the access token: identity, name, room and grants.",
        ),
        (
            "Region lookup",
            "HTTPS",
            "Before connecting, LiveKit Cloud URLs only",
            "GET /settings/regions with the token; returns the regions the SDK tries in order.",
        ),
        (
            "Ping",
            "signal",
            "Every ping interval from the server's join response",
            "timestamp · rtt (the signal round trip measured from the previous pong). No pong within the timeout ends the connection.",
        ),
        (
            "Offer, answer, ICE candidates",
            "signal",
            "Connect, publish, renegotiation, ICE restart",
            "Full SDP (codecs, profiles, extensions) and each ICE candidate, which includes this machine's local and public IP addresses.",
        ),
        (
            "Add track",
            "signal",
            "Each publish",
            "name · type · source · muted · dimensions · layers · codecs · disable_dtx · encryption · stream",
        ),
        (
            "Mute, subscription, track settings",
            "signal",
            "Mute, subscribe, or a subscriber changes layer, size or enabled state",
            "track_sids · muted · subscribe · disabled · width · height · quality. \"Ask optimal resolution\" sends track settings at most every 5 s.",
        ),
        (
            "Metadata, permissions, sync, leave",
            "signal",
            "Name or attribute changes; permissions; resume after a drop; disconnect",
            "name · metadata · attributes · track permissions · sync state · leave reason",
        ),
        (
            "Client metrics batch",
            "data packet",
            "Never, in the Rust SDK",
            "The Swift SDK sends a MetricsBatch per track when statistics reporting is on: freezes, pauses, jitter buffer, quality limitation time, RTT.",
        ),
        (
            "RTCP",
            "media path, by WebRTC",
            "Continuously while media flows",
            "Receiver and sender reports (loss, jitter, timing), congestion feedback, NACK, PLI. The server judges connection quality from these.",
        ),
    ];
    egui::Grid::new("telemetry")
        .striped(true)
        .spacing([14.0, 6.0])
        .min_col_width(90.0)
        .show(ui, |ui| {
            for h in ["What", "Channel", "When", "Fields sent"] {
                ui.label(RichText::new(h).small().strong());
            }
            ui.end_row();
            for (what, ch, when, fields) in rows {
                ui.label(RichText::new(what).strong());
                ui.label(ch);
                ui.label(when);
                ui.add(egui::Label::new(fields).wrap());
                ui.end_row();
            }
        });
    ui.add_space(8.0);
    ui.label(RichText::new("What the Rust SDK does not send").strong());
    ui.label(
        "No frame rate, bitrate or CPU figures, no WebRTC stats upload, and no client metrics batch. Of the client info fields, browser, browser version, address and network are left empty. The server measures frame rate and bitrate itself from the media it forwards.",
    );
}
