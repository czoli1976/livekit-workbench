//! The main window and the state behind every window.

mod dialogs;
mod form;
mod pipeline;
mod saved;
mod stats_window;
mod theme;
mod widgets;

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use crate::capabilities::HostCaps;
use crate::capture::{self, CameraDesc, CameraFormat, SourceKind};
use crate::local_server::{LocalServer, ServerState};
use crate::profiles::{LocalServerOptions, Profile, ProfileStore};
use crate::session::{self, ConnectConfig, Phase, Session};
use crate::throttle::{SizeThrottle, DEFAULT_INTERVAL};
use crate::version;
use crate::viewer::ViewerServer;

use dialogs::ProfileEditor;
use form::PublishForm;
use pipeline::{ChartMetric, PreviewLayout, PreviewPane, SubAsk};
use stats_window::StatsUi;

/// Camera formats listed in the background: (camera index, formats or error).
/// Camera formats listed in the background: (request number, formats or error).
type FormatsResult = (u64, Result<Vec<CameraFormat>, String>);

pub struct App {
    rt: tokio::runtime::Runtime,
    session: Session,
    caps: HostCaps,

    store: ProfileStore,
    profile_id: String,
    /// The profile and secret of the current connection (for viewer tokens).
    connection: Option<(Profile, String)>,
    room: String,
    auto_publish: bool,

    server: LocalServer,
    /// What the background poller should watch; None when the profile is remote.
    server_opts: Arc<Mutex<Option<LocalServerOptions>>>,

    form: PublishForm,
    cameras: Vec<CameraDesc>,
    camera_formats: Vec<CameraFormat>,
    formats_for: Option<u32>,
    formats_pending: Arc<Mutex<Option<FormatsResult>>>,
    /// Numbers each format request, so a late answer to an older one is ignored.
    formats_generation: u64,
    formats_retry: Instant,
    /// The source last sent to the engine's preview-only capture.
    preview_sent: Option<session::PublishConfig>,
    /// Last local preview frame number seen, and when it changed.
    preview_seq: (u64, Instant),
    /// The input in the previous frame, to notice a switch to Screen.
    last_source: SourceKind,
    camera_error: Option<String>,

    previews: bool,
    chart: ChartMetric,
    layout: PreviewLayout,
    local_undocked: bool,
    remote_undocked: bool,
    local_pane: PreviewPane,
    remote_pane: PreviewPane,
    /// What the app's subscriber asks the SFU for.
    sub_ask: SubAsk,
    /// The size last sent for Half/Quarter, to resend when the publish size changes.
    ask_sent: Option<(u32, u32)>,
    throttle: SizeThrottle,

    viewer: Option<ViewerServer>,
    invites: u32,
    invite_note: String,

    show_stats: bool,
    show_caps: bool,
    show_profiles: bool,
    show_about: bool,
    profile_editor: ProfileEditor,
    stats_ui: StatsUi,
    notice: Option<(String, Instant)>,
    autoconnect_pending: bool,
    /// Encoders and decoders WebRTC reported this session, per codec line.
    seen: std::collections::BTreeMap<
        String,
        (
            std::collections::BTreeSet<String>,
            std::collections::BTreeSet<String>,
        ),
    >,
    /// The left settings panel is open.
    settings_open: bool,
    test_pending_done: bool,
    codec_versions: crate::codec_versions::CodecVersions,
    /// End-to-end encryption for the next connection, and its passphrase.
    e2ee_on: bool,
    /// Connect through the in-app impairment relay.
    impair_on: bool,
    connected_impair: bool,
    /// Microphone names, listed the first time the audio input list opens.
    microphones: Option<Vec<String>>,
    /// Why the microphones could not be listed; the list is retried on open.
    mic_error: Option<String>,
    e2ee_key: String,
    /// The passphrase of the current connection, if it is encrypted.
    connected_e2ee: Option<String>,
    /// Settings as last written to settings.json, and when.
    saved: Option<saved::Saved>,
    saved_at: Instant,
    /// False for scripted launches (WB_OPEN) and when the settings file could
    /// not be read: then nothing is written over the user's settings.
    persist_settings: bool,
    /// Newer livekit-server check and update, shared with background work.
    server_update: Arc<Mutex<crate::server_update::UpdateState>>,
    /// Browser viewers ask the SFU for a layer that fits their video element.
    browser_adaptive: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("workbench-rt")
            .build()
            .expect("tokio runtime");
        theme::install(&cc.egui_ctx);
        // Scanning the executable for WebRTC's source stamp reads the whole
        // file once; do it off the UI thread.
        std::thread::spawn(|| {
            let _ = version::webrtc_source_stamp();
        });
        let ctx = cc.egui_ctx.clone();
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        let session = Session::start(rt.handle(), repaint);

        let mut store = ProfileStore::load();
        store.repair();
        let profile = store.default_profile().clone();
        let server = LocalServer::default();
        let server_opts = Arc::new(Mutex::new(None));

        let mut app = Self {
            caps: HostCaps::probe(),
            session,
            profile_id: profile.id.clone(),
            connection: None,
            room: profile.default_room.clone(),
            auto_publish: true,
            store,
            server,
            server_opts,
            form: PublishForm::default(),
            cameras: Vec::new(),
            camera_formats: Vec::new(),
            formats_for: None,
            formats_pending: Default::default(),
            formats_generation: 0,
            formats_retry: Instant::now(),
            preview_sent: None,
            preview_seq: (0, Instant::now()),
            last_source: SourceKind::Detail,
            camera_error: None,
            previews: true,
            chart: ChartMetric::Bitrate,
            layout: PreviewLayout::SideBySide,
            local_undocked: false,
            remote_undocked: false,
            local_pane: PreviewPane::default(),
            remote_pane: PreviewPane::default(),
            sub_ask: SubAsk::Top,
            ask_sent: None,
            throttle: SizeThrottle::new(DEFAULT_INTERVAL),
            viewer: None,
            invites: 0,
            invite_note: String::new(),
            show_stats: false,
            show_caps: false,
            show_profiles: false,
            show_about: false,
            profile_editor: ProfileEditor::default(),
            stats_ui: StatsUi::default(),
            notice: None,
            browser_adaptive: false,
            seen: Default::default(),
            settings_open: true,
            test_pending_done: false,
            codec_versions: crate::codec_versions::read(),
            e2ee_on: false,
            impair_on: false,
            connected_impair: false,
            microphones: None,
            mic_error: None,
            connected_e2ee: None,
            e2ee_key: random_passphrase(),
            saved: None,
            saved_at: Instant::now(),
            persist_settings: std::env::var_os("WB_OPEN").is_none(),
            server_update: Default::default(),
            autoconnect_pending: std::env::var_os("WB_AUTOCONNECT").is_some(),
            rt,
        };
        app.sanitize_form();
        // WB_OPEN=stats,caps,profiles,about opens those windows at start-up (demos, checks).
        if let Ok(open) = std::env::var("WB_OPEN") {
            for w in open.split(',') {
                match w.trim() {
                    "stats" => app.show_stats = true,
                    "caps" => app.show_caps = true,
                    "profiles" => app.show_profiles = true,
                    "about" => app.show_about = true,
                    "far" => app.remote_undocked = true,
                    "nosettings" => app.settings_open = false,
                    "simulcast" => app.form.layers = form::LayerKind::Simulcast,
                    "local" => app.local_undocked = true,
                    _ => {}
                }
            }
        }
        app.start_server_poller();
        app.activate_profile_server(true);
        app.check_server_update();
        app.start_viewer_server();
        app.refresh_cameras();
        // Start on the system's camera (the first one it lists); the synthetic
        // sources stay one click away, and are the input when there is no camera.
        if let Some(c) = app.cameras.first() {
            app.form.source = SourceKind::Camera(c.index);
        }
        // Settings from the last run, unless this is a scripted dev launch.
        if app.persist_settings {
            match saved::load() {
                Ok(Some(s)) => {
                    app.apply_saved(&s);
                    app.saved = Some(s);
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("settings: {e}");
                    // A file that could not even be read is not overwritten.
                    app.persist_settings = e.contains("is not valid");
                }
            }
        }
        app.last_source = app.form.source.clone();
        app
    }

    fn current_profile(&self) -> Profile {
        self.store
            .get(&self.profile_id)
            .cloned()
            .unwrap_or_else(|| self.store.default_profile().clone())
    }

    fn save_store(&mut self) {
        if let Err(e) = self.store.save() {
            self.notify(format!("Could not save profiles: {e:#}"));
        }
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    // ---- Cameras

    fn refresh_cameras(&mut self) {
        match capture::list_cameras() {
            Ok(c) => {
                self.cameras = c;
                self.camera_error = None;
            }
            Err(e) => {
                self.cameras.clear();
                self.camera_error = Some(format!("{e:#}"));
            }
        }
        self.formats_for = None;
        if let SourceKind::Camera(i) = self.form.source {
            if !self.cameras.iter().any(|c| c.index == i) {
                self.form.source = SourceKind::Detail;
            }
        }
    }

    /// Ask the camera for its formats off the UI thread: opening a camera can
    /// take seconds, or block when access is not allowed.
    fn load_formats(&mut self, index: u32) {
        // Camera access can be granted after the first try (the macOS prompt is
        // still open at start-up, or the user allows it in Settings): re-check
        // every 2 s while the panel shows an error, and list again once allowed.
        if self.camera_error.is_some() && self.formats_retry.elapsed() > Duration::from_secs(2) {
            self.formats_retry = Instant::now();
            if capture::camera_access().is_ok() {
                self.camera_error = None;
                self.formats_for = None;
                // Start the preview again now that the camera can be opened.
                self.preview_sent = None;
            }
        }
        if self.formats_for != Some(index) {
            self.formats_for = Some(index);
            self.camera_formats.clear();
            self.formats_generation += 1;
            let generation = self.formats_generation;
            let slot = self.formats_pending.clone();
            *slot.lock() = None;
            std::thread::spawn(move || {
                let r = capture::camera_formats(index).map_err(|e| format!("{e:#}"));
                let mut s = slot.lock();
                // Never let an older request overwrite a newer answer.
                if s.as_ref().is_none_or(|(g, _)| *g <= generation) {
                    *s = Some((generation, r));
                }
            });
        }
        let ready = {
            let mut s = self.formats_pending.lock();
            match s.as_ref() {
                Some((g, _)) if *g == self.formats_generation => s.take(),
                _ => None,
            }
        };
        let Some((_, result)) = ready else {
            return;
        };
        match result {
            Ok(mut f) => {
                self.camera_error = None;
                if f.is_empty() {
                    // Some cameras list no formats through this API: offer the
                    // usual sizes and let the camera pick the closest.
                    f = form::SIZES
                        .iter()
                        .filter(|s| s.2 <= 1080)
                        .map(|&(_, w, h)| CameraFormat {
                            width: w,
                            height: h,
                            fps: 30,
                        })
                        .collect();
                }
                let current = (self.form.capture, self.form.fps);
                if !f.iter().any(|x| ((x.width, x.height), x.fps) == current) {
                    // Prefer 1080p30 or the closest below it, else the first format.
                    let pick = f
                        .iter()
                        .find(|x| x.width <= 1920 && x.fps <= 30)
                        .or_else(|| f.first())
                        .copied();
                    if let Some(x) = pick {
                        self.form.capture = (x.width, x.height);
                        self.form.fps = x.fps;
                    }
                }
                self.camera_formats = f;
            }
            Err(e) => self.camera_error = Some(e),
        }
    }

    // ---- Local server

    fn start_server_poller(&self) {
        let server = self.server.clone();
        let opts = self.server_opts.clone();
        self.rt.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let o = opts.lock().clone();
                if let Some(o) = o {
                    server.poll(&o).await;
                }
            }
        });
    }

    /// Start or stop the local server to match the selected profile.
    fn activate_profile_server(&mut self, at_launch: bool) {
        let p = self.current_profile();
        if p.is_local() {
            *self.server_opts.lock() = Some(p.local.clone());
            let should_start = !at_launch || p.local.start_with_app;
            if should_start
                && matches!(
                    self.server.state(),
                    ServerState::Stopped | ServerState::Failed(_)
                )
            {
                self.start_server();
            }
        } else {
            *self.server_opts.lock() = None;
            let server = self.server.clone();
            self.rt.spawn(async move { server.stop().await });
        }
    }

    fn start_server(&mut self) {
        let p = self.current_profile();
        if !p.is_local() {
            return;
        }
        let server = self.server.clone();
        self.rt.spawn(async move {
            if let Err(e) = server.start(p.local).await {
                log::warn!("local server: {e:#}");
            }
        });
    }

    fn restart_server(&mut self) {
        let p = self.current_profile();
        if !p.is_local() {
            return;
        }
        *self.server_opts.lock() = Some(p.local.clone());
        if self.session.phase().connected() {
            self.session.disconnect();
        }
        let server = self.server.clone();
        self.rt.spawn(async move {
            server.stop().await;
            if let Err(e) = server.start(p.local).await {
                log::warn!("local server: {e:#}");
            }
        });
    }

    fn server_buttons(&mut self, ui: &mut egui::Ui) {
        match self.server.state() {
            ServerState::Running => {
                if ui.small_button("Restart").clicked() {
                    self.restart_server();
                }
                if ui.small_button("Stop").clicked() {
                    if self.session.phase().connected() {
                        self.session.disconnect();
                    }
                    let s = self.server.clone();
                    self.rt.spawn(async move { s.stop().await });
                }
            }
            ServerState::Starting | ServerState::External => {}
            ServerState::Stopped | ServerState::Failed(_) => {
                if ui.small_button("Start server").clicked() {
                    self.start_server();
                }
            }
        }
    }

    // ---- Connection

    fn connect(&mut self) {
        let profile = self.current_profile();
        if let Some(p) = profile.validate().into_iter().next() {
            self.notify(format!("Profile \"{}\": {p}", profile.name));
            self.show_profiles = true;
            return;
        }
        if profile.is_local() && !self.server.is_usable() {
            self.notify("The local server is not running. Start it from the status line below.");
            return;
        }
        let secret = match profile.api_secret() {
            Ok(s) => s,
            Err(e) => {
                self.notify(format!("{e:#}. Add it in Edit profiles."));
                self.show_profiles = true;
                return;
            }
        };
        if self.e2ee_on && self.e2ee_key.trim().is_empty() {
            self.notify("End-to-end encryption is on but the passphrase is empty. Enter one, or turn encryption off.");
            return;
        }
        let room = self.room.trim().to_string();
        if room.is_empty() {
            self.notify("Enter a room name.");
            return;
        }
        let publish = if self.auto_publish {
            match self.validate_form() {
                Ok(c) => Some(c),
                Err(e) => {
                    self.notify(format!("Cannot publish: {e}"));
                    return;
                }
            }
        } else {
            None
        };
        let cfg = ConnectConfig {
            url: profile.connect_url(),
            api_key: profile.api_key.trim().to_string(),
            api_secret: secret.clone(),
            room,
            decode: true,
            preview: self.previews,
            e2ee_key: self.e2ee_on.then(|| self.e2ee_key.trim().to_string()),
            impair: self.impair_on,
        };
        self.ask_sent = None;
        self.throttle.reset();
        self.connection = Some((profile, secret));
        self.connected_e2ee = cfg.e2ee_key.clone();
        self.connected_impair = cfg.impair;
        self.session.connect(cfg, publish);
    }

    fn switch_profile(&mut self, id: String) {
        if id == self.profile_id {
            return;
        }
        if self.session.phase() != Phase::Disconnected {
            self.session.disconnect();
        }
        let was_local = self.current_profile().is_local();
        self.profile_id = id;
        let p = self.current_profile();
        self.room = p.default_room.clone();
        if was_local || p.is_local() {
            self.activate_profile_server(false);
        }
        if !p.is_local() && self.stats_ui.tab == stats_window::StatsTab::Server {
            self.stats_ui.tab = stats_window::StatsTab::Stats;
        }
    }

    // ---- Browser viewer

    fn start_viewer_server(&mut self) {
        match ViewerServer::start() {
            Ok(v) => self.viewer = Some(v),
            Err(e) => self.invite_note = format!("Could not start the local HTTP server: {e}"),
        }
    }

    fn launch_chrome_publisher(&mut self) {
        let Some(viewer) = self.viewer.as_ref() else {
            return;
        };
        let Some((profile, secret)) = self.connection.clone() else {
            return;
        };
        let room = self.session.shared.lock().room.clone();
        self.invites += 1;
        let identity = format!("{}{}", session::BROWSER_PUB_PREFIX, self.invites);
        let cfg = self.form.to_config();
        let (w, h) = cfg.publish_dims();
        let codec = match cfg.codec {
            livekit::options::VideoCodec::H264 => "h264",
            livekit::options::VideoCodec::H265 => "h265",
            livekit::options::VideoCodec::VP8 => "vp8",
            livekit::options::VideoCodec::VP9 => "vp9",
            livekit::options::VideoCodec::AV1 => "av1",
        };
        let layers = match &cfg.layering {
            session::Layering::Single => "single".to_string(),
            session::Layering::Simulcast => "simulcast".to_string(),
            session::Layering::Svc(m) => m.clone(),
        };
        let token = match session::publisher_token(&profile.api_key, &secret, &room, &identity) {
            Ok(t) => t,
            Err(e) => {
                self.notify(format!("Could not create a token: {e:#}"));
                return;
            }
        };
        let link = viewer.publisher_link(
            &profile.connect_url(),
            &token,
            &crate::viewer::PublisherParams {
                codec,
                width: w,
                height: h,
                fps: cfg.fps.min(cfg.max_fps),
                mbps: cfg.bitrate_bps as f64 / 1e6,
                layers: &layers,
            },
        );
        match crate::viewer::launch_chrome_with_flexfec(&link) {
            Ok(()) => self.notify(format!(
                "Chrome publisher {identity} starting with FlexFEC field trials"
            )),
            Err(e) => self.notify(format!("Could not start Chrome: {e}")),
        }
    }

    fn invite_link(&mut self) -> Option<String> {
        let viewer = self.viewer.as_ref()?;
        let (profile, secret) = self.connection.clone()?;
        let room = self.session.shared.lock().room.clone();
        self.invites += 1;
        let identity = format!("browser-{}", self.invites);
        match session::viewer_token(&profile.api_key, &secret, &room, &identity) {
            Ok(token) => {
                let key = self.connected_e2ee.clone();
                Some(viewer.link(
                    &profile.connect_url(),
                    &token,
                    self.browser_adaptive,
                    key.as_deref(),
                ))
            }
            Err(e) => {
                self.invite_note = format!("Could not create a token: {e:#}");
                None
            }
        }
    }

    fn invite(&mut self, browser: Option<&str>) {
        let Some(link) = self.invite_link() else {
            return;
        };
        self.invite_note = match crate::viewer::open_url(&link, browser) {
            Ok(()) => {
                "Viewer opened. Its stats appear in Stats & Telemetry within a few seconds.".into()
            }
            Err(_) if browser.is_some() => match crate::viewer::open_url(&link, None) {
                Ok(()) => "Chrome was not found; opened the default browser instead.".into(),
                Err(e) => format!("Could not open a browser ({e}). Use Copy link."),
            },
            Err(e) => format!("Could not open a browser ({e}). Use Copy link."),
        };
    }

    // ---- Top bar and status line

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let phase = self.session.phase();
        let profile = self.current_profile();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.label(
                RichText::new(version::APP_NAME)
                    .strong()
                    .size(15.0)
                    .color(theme::TEXT),
            );
            ui.add_space(4.0);

            // Profile: one segment per profile, like the mockup's Local dev / LiveKit Cloud.
            let mut chosen = None;
            widgets::segments(ui, |ui| {
                for p in &self.store.profiles {
                    let on = p.id == self.profile_id;
                    if widgets::segment(ui, &p.name, on, theme::ACCENT_BG, theme::TEXT)
                        .on_hover_text("Switching profile disconnects first.")
                        .clicked()
                    {
                        chosen = Some(p.id.clone());
                    }
                }
            });
            if let Some(id) = chosen {
                self.switch_profile(id);
            }
            let (dot, dot_color) = if profile.is_local() {
                match self.server.state() {
                    ServerState::Running | ServerState::External => {
                        ("server running", theme::GREEN_TEXT)
                    }
                    ServerState::Starting => ("server starting", theme::AMBER_TEXT),
                    _ => ("server stopped", theme::RED),
                }
            } else {
                ("remote", theme::MUTED)
            };
            ui.label(
                RichText::new(format!("{} · ", profile.connect_url()))
                    .monospace()
                    .size(12.0)
                    .color(theme::MUTED),
            );
            ui.label(RichText::new(dot).size(12.0).color(dot_color));
            if ui.button("Edit profiles").clicked() {
                self.show_profiles = true;
                self.profile_editor.draft = None;
                self.profile_editor.selected = self.profile_id.clone();
            }
            ui.label(RichText::new("Room").color(theme::MUTED));
            ui.add_enabled(
                phase == Phase::Disconnected,
                egui::TextEdit::singleline(&mut self.room)
                    .desired_width(100.0)
                    .font(egui::TextStyle::Monospace),
            );

            // Phase: Disconnected / Connected / Publishing, the current one lit.
            widgets::segments(ui, |ui| {
                let busy = matches!(
                    phase,
                    Phase::Connecting | Phase::Unpublishing | Phase::Disconnecting
                );
                let label_for = |base: Phase, text: &'static str| -> String {
                    if busy && phase_base(phase) == base {
                        phase.label().to_string()
                    } else {
                        text.to_string()
                    }
                };
                let base = phase_base(phase);
                widgets::segment(
                    ui,
                    &label_for(Phase::Disconnected, "Disconnected"),
                    base == Phase::Disconnected,
                    theme::RAISED,
                    theme::TEXT,
                );
                widgets::segment(
                    ui,
                    &label_for(Phase::Connected, "Connected"),
                    base == Phase::Connected,
                    theme::ACCENT,
                    theme::BG,
                );
                widgets::segment(
                    ui,
                    &label_for(Phase::Publishing, "Publishing"),
                    base == Phase::Publishing,
                    theme::GREEN,
                    theme::BG,
                );
            });

            let connect_label = "Connect";
            if ui
                .add_enabled(
                    phase == Phase::Disconnected,
                    widgets::accent_button(connect_label),
                )
                .on_hover_text(if self.auto_publish {
                    "Connects and publishes (Auto-publish is on)."
                } else {
                    "Connects without publishing."
                })
                .clicked()
            {
                self.connect();
            }
            if ui
                .add_enabled(phase == Phase::Connected, widgets::accent_button("Publish"))
                .clicked()
            {
                self.publish_or_republish(false);
            }
            if ui
                .add_enabled(
                    phase == Phase::Publishing,
                    widgets::warn_button("Unpublish"),
                )
                .clicked()
            {
                self.session.unpublish();
            }
            let can_disconnect = matches!(
                phase,
                Phase::Connecting | Phase::Connected | Phase::Publishing
            );
            if ui
                .add_enabled(can_disconnect, egui::Button::new("Disconnect"))
                .clicked()
            {
                self.session.disconnect();
            }
            ui.checkbox(&mut self.auto_publish, "Auto-publish on connect");

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(RichText::new("i").strong())
                    .on_hover_text("About")
                    .clicked()
                {
                    self.show_about = true;
                }
                if ui
                    .add(widgets::toggle_button("Stats & Telemetry", self.show_stats))
                    .clicked()
                {
                    self.show_stats = !self.show_stats;
                }
                if ui
                    .add(widgets::toggle_button("Host capabilities", self.show_caps))
                    .clicked()
                {
                    self.show_caps = !self.show_caps;
                }
            });
        });
    }

    fn status_line(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let profile = self.current_profile();
            if profile.is_local() {
                let st = self.server.stats();
                let (text, color) = match self.server.state() {
                    ServerState::Running => (
                        format!(
                            "Local server running · {} · CPU {} · {} room{}",
                            if st.version.is_empty() {
                                "livekit-server".to_string()
                            } else {
                                format!("v{}", st.version)
                            },
                            st.cpu_percent
                                .map(|c| format!("{c:.0}%"))
                                .unwrap_or_else(|| "–".into()),
                            st.rooms.unwrap_or(0),
                            if st.rooms == Some(1) { "" } else { "s" }
                        ),
                        widgets::green(ui),
                    ),
                    ServerState::External => (
                        "Using a server already running on the port".into(),
                        widgets::amber(ui),
                    ),
                    ServerState::Starting => ("Local server starting…".into(), widgets::amber(ui)),
                    ServerState::Stopped => ("Local server stopped".into(), widgets::weak(ui)),
                    ServerState::Failed(e) => {
                        (format!("Local server failed: {e}"), widgets::red(ui))
                    }
                };
                ui.colored_label(color, text);
                self.server_buttons(ui);
                if ui.small_button("Server stats").clicked() {
                    self.show_stats = true;
                    self.stats_ui.tab = stats_window::StatsTab::Server;
                }
                self.check_version_button(ui);
                self.server_update_ui(ui);
                ui.separator();
            }
            if let Some((text, at)) = &self.notice {
                if at.elapsed() < Duration::from_secs(12) {
                    ui.colored_label(widgets::amber(ui), text);
                } else {
                    self.notice = None;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "LiveKit Rust SDK {} · WebRTC {} · v{}",
                        version::LIVEKIT_SDK,
                        version::WEBRTC_MILESTONE,
                        version::APP_VERSION
                    ))
                    .small()
                    .color(widgets::weak(ui)),
                );
            });
        });
    }

    fn shutdown(&mut self) {
        self.session.disconnect();
        // Give the engine a moment to leave the rooms cleanly.
        let started = Instant::now();
        let closing = self.session.shared.lock().closing.clone();
        while (self.session.phase() != Phase::Disconnected
            || closing.load(std::sync::atomic::Ordering::SeqCst) > 0)
            && started.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        self.server.kill_now();
        self.viewer = None;
    }
}

impl App {
    /// Remember which encoder and decoder WebRTC used for each codec line.
    fn note_seen(&mut self) {
        let snap = self.session.shared.lock().snapshot.clone();
        for l in snap
            .layers
            .iter()
            .filter(|l| !l.codec.is_empty() && !l.encoder.is_empty())
        {
            let hw = if l.hardware { "HW" } else { "SW" };
            self.seen
                .entry(l.codec.clone())
                .or_default()
                .0
                .insert(format!("{} ({hw})", l.encoder));
        }
        if let Some(r) = snap
            .self_receive()
            .filter(|r| !r.codec.is_empty() && !r.decoder.is_empty())
        {
            let hw = match r.hardware {
                Some(true) => " (HW)",
                Some(false) => " (SW)",
                None => "",
            };
            self.seen
                .entry(r.codec.clone())
                .or_default()
                .1
                .insert(format!("{}{hw}", r.decoder));
        }
    }
}

/// Whether the main window is in full-screen mode (always asked of the root).
fn main_is_fullscreen(ctx: &egui::Context) -> bool {
    // WB_EMBED=1 draws child windows inside the main one (screenshots, demos).
    if std::env::var_os("WB_EMBED").is_some() {
        return true;
    }
    ctx.input_for(egui::ViewportId::ROOT, |i| {
        i.viewport().fullscreen.unwrap_or(false)
    })
}

impl App {
    /// Ask GitHub, off the UI thread, whether a newer livekit-server exists.
    fn check_server_update(&mut self) {
        use crate::server_update::UpdateState;
        let profile = self.current_profile();
        if !profile.is_local() {
            return;
        }
        let Some(binary) = crate::local_server::find_binary(&profile.local.binary_path) else {
            return;
        };
        let state = self.server_update.clone();
        *state.lock() = UpdateState::Checking;
        std::thread::spawn(move || {
            let result = crate::server_update::check(&binary);
            *state.lock() = match result {
                Ok(s) => s,
                Err(e) => {
                    // Offline or rate-limited: say nothing in the UI.
                    log::info!("server update check: {e:#}");
                    UpdateState::Idle
                }
            };
        });
    }

    /// Stop the server, update it, start it again.
    fn run_server_update(&mut self, info: crate::server_update::UpdateInfo) {
        use crate::server_update::UpdateState;
        if self.session.phase() != Phase::Disconnected {
            self.session.disconnect();
        }
        let state = self.server_update.clone();
        let server = self.server.clone();
        let opts = self.current_profile().local.clone();
        *state.lock() = UpdateState::Updating("Stopping the server…".into());
        self.rt.spawn(async move {
            server.stop().await;
            let progress_state = state.clone();
            let result = tokio::task::spawn_blocking(move || {
                let progress = move |s: String| *progress_state.lock() = UpdateState::Updating(s);
                crate::server_update::update(&info, &progress)
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|r| r);
            *state.lock() = match result {
                Ok(v) => UpdateState::Updated(v),
                Err(e) => UpdateState::Failed(format!("{e:#}")),
            };
            if let Err(e) = server.start(opts).await {
                log::warn!("local server after update: {e:#}");
            }
        });
    }

    pub(super) fn check_version_button(&mut self, ui: &mut egui::Ui) {
        use crate::server_update::UpdateState;
        let busy = matches!(
            *self.server_update.lock(),
            UpdateState::Checking | UpdateState::Updating(_)
        );
        if ui
            .add_enabled(!busy, egui::Button::new("Check version").small())
            .on_hover_text(
                "Compare the installed livekit-server with LiveKit's latest GitHub release.",
            )
            .clicked()
        {
            self.check_server_update();
        }
    }

    pub(super) fn server_update_ui(&mut self, ui: &mut egui::Ui) {
        use crate::server_update::{InstallKind, UpdateState};
        let state = self.server_update.lock().clone();
        match state {
            UpdateState::Available(info) => {
                ui.colored_label(
                    theme::AMBER_TEXT,
                    format!(
                        "livekit-server {} available (you have {})",
                        info.latest, info.installed
                    ),
                );
                match info.kind.clone() {
                    InstallKind::Other => {
                        ui.label(
                            RichText::new(
                                "installed outside the app; update it the way it was installed",
                            )
                            .small()
                            .color(theme::MUTED),
                        )
                        .on_hover_text(info.binary.display().to_string());
                    }
                    kind => {
                        let how = if kind == InstallKind::Homebrew {
                            "Runs brew upgrade livekit, then restarts the server. Disconnects first."
                        } else {
                            "Downloads the release from GitHub, checks its SHA-256, replaces the server and restarts it. Disconnects first."
                        };
                        if ui
                            .add(widgets::accent_button("Update"))
                            .on_hover_text(how)
                            .clicked()
                        {
                            self.run_server_update(info);
                        }
                    }
                }
            }
            UpdateState::Updating(step) => {
                ui.add(egui::Spinner::new().size(12.0));
                ui.colored_label(theme::AMBER_TEXT, step);
            }
            UpdateState::Updated(v) => {
                ui.colored_label(theme::GREEN_TEXT, format!("Updated to {v}"));
                if ui.small_button("OK").clicked() {
                    *self.server_update.lock() = UpdateState::UpToDate(v);
                }
            }
            UpdateState::Failed(e) => {
                ui.colored_label(theme::RED, format!("Update failed: {e}"));
                if ui.small_button("Dismiss").clicked() {
                    *self.server_update.lock() = UpdateState::Idle;
                }
            }
            UpdateState::Checking => {
                ui.label(
                    RichText::new("checking for updates…")
                        .small()
                        .color(theme::MUTED),
                );
            }
            UpdateState::UpToDate(v) => {
                ui.label(RichText::new("✓ latest").small().color(theme::GREEN_TEXT))
                    .on_hover_text(format!("Checked LiveKit's GitHub releases at start-up: {v} is the newest livekit-server."));
            }
            UpdateState::Idle => {}
        }
    }
}

/// The resting state a transitional phase belongs to, for the phase indicator.
fn phase_base(p: Phase) -> Phase {
    match p {
        Phase::Connecting | Phase::Disconnected => Phase::Disconnected,
        Phase::Unpublishing | Phase::Connected => Phase::Connected,
        Phase::Disconnecting => Phase::Disconnected,
        Phase::Publishing => Phase::Publishing,
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("top")
            .frame(theme::top_frame())
            .show(ui, |ui| {
                self.top_bar(ui);
            });
        egui::Panel::bottom("status")
            .frame(theme::status_frame())
            .show(ui, |ui| {
                self.status_line(ui);
            });
        self.note_seen();
        self.save_settings(false);
        // Show the chosen source in the local tile even when not publishing.
        // Choosing the screen: publish it as a screen share with the detail hint.
        if self.form.source == SourceKind::Screen && self.last_source != SourceKind::Screen {
            self.form.screen_share = true;
            self.form.detail_hint = true;
        }
        self.last_source = self.form.source.clone();
        // No preview before publishing for a screen: on macOS each capture start
        // opens the system picker, so the user would have to pick twice.
        let want = (self.previews && self.form.source != SourceKind::Screen)
            .then(|| self.form.to_config());
        // No preview frame for 3 s while one should show (the source failed to
        // start, e.g. camera access still pending at start-up): ask again.
        let seq = self.session.shared.lock().local_preview.lock().seq;
        if seq != self.preview_seq.0 {
            self.preview_seq = (seq, Instant::now());
            // Frames from the camera are arriving now: any access error is stale.
            if matches!(self.form.source, SourceKind::Camera(_)) {
                self.camera_error = None;
            }
        } else if want.is_some()
            && self.session.phase() != Phase::Publishing
            && self.preview_seq.1.elapsed() > Duration::from_secs(3)
        {
            self.preview_seq.1 = Instant::now();
            self.preview_sent = None;
        }
        if want != self.preview_sent {
            self.session.set_preview(want.clone());
            self.preview_sent = want;
        }
        // WB_TEST_PENDING=1: make one republish change once publishing (checks).
        if self.session.phase() == Phase::Publishing
            && std::env::var_os("WB_TEST_PENDING").is_some()
            && !self.test_pending_done
        {
            self.test_pending_done = true;
            self.form.bitrate_mbps += 1.0;
        }
        // A handle strip next to the settings panel opens and closes it.
        let mut open = self.settings_open;
        egui::Panel::left("settings")
            .frame(theme::sidebar_frame())
            .default_size(400.0)
            .resizable(true)
            .show_collapsible(ui, &mut open, |ui| {
                self.pending_banner(ui);
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| self.settings_ui(ui));
            });
        self.settings_open = open;
        egui::Panel::left("settings-handle")
            .frame(
                egui::Frame::new()
                    .fill(theme::SIDEBAR)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER)),
            )
            .exact_size(18.0)
            .resizable(false)
            .show(ui, |ui| {
                let (rect, resp) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
                let hovered = resp.hovered();
                if hovered {
                    ui.painter().rect_filled(rect, 0.0, theme::HOVER);
                }
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if self.settings_open { "‹" } else { "›" },
                    egui::FontId::proportional(18.0),
                    if hovered { theme::TEXT } else { theme::MUTED },
                );
                let resp = resp.on_hover_text(if self.settings_open {
                    "Hide settings"
                } else {
                    "Show settings"
                });
                if resp.clicked() {
                    self.settings_open = !self.settings_open;
                }
            });
        egui::CentralPanel::default_margins()
            .frame(theme::central_frame())
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| self.pipeline_ui(ui));
            });

        self.keep_sub_ask();
        if self.show_caps {
            self.capabilities_window(&ctx);
        }
        if self.show_profiles {
            self.profiles_window(&ctx);
        }
        if self.show_about {
            self.about_window(&ctx);
        }
        self.preview_windows(&ctx);
        self.stats_window(&ctx);

        // WB_AUTOCONNECT=1 connects (and publishes) once at start-up: for demos and checks.
        if self.autoconnect_pending && self.server.is_usable() {
            self.autoconnect_pending = false;
            self.connect();
        }
        // Previews wake the UI on each new frame; this keeps numbers and states fresh.
        let busy = self.session.phase() != Phase::Disconnected
            || matches!(self.server.state(), ServerState::Starting);
        ctx.request_repaint_after(Duration::from_millis(if busy { 250 } else { 1000 }));
    }

    fn on_exit(&mut self) {
        self.save_settings(true);
        self.shutdown();
    }
}

/// A fresh passphrase for first use, so nobody starts on a key published in
/// this source. Not a password generator: it is a test key, saved in plain text.
fn random_passphrase() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    h.update(now.as_nanos().to_le_bytes());
    h.update(std::process::id().to_le_bytes());
    let local = 0u8;
    h.update((&local as *const u8 as usize).to_le_bytes());
    let d = h.finalize();
    format!(
        "wb-{}",
        d.iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
