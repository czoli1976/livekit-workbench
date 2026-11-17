//! The session engine: one task that owns both room connections, the published
//! track, the capture thread and the stats sampler, driven by commands from the UI.
//!
//! Two connections are used because a participant cannot subscribe to its own
//! tracks: a publisher, and a subscriber that receives the publisher's video
//! back from the server ("subscribe to self").

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use livekit::options::{
    compute_video_encodings, AudioEncoding, DegradationPreference, TrackPublishOptions, VideoCodec,
    VideoEncoding,
};
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::{AudioSourceOptions, RtcAudioSource};
use livekit::webrtc::native::yuv_helper;
use livekit::webrtc::video_frame::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation};
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_source::{RtcVideoSource, VideoResolution};
use livekit::webrtc::video_stream::native::NativeVideoStream;
use livekit_api::access_token;
use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::capabilities::EncoderChoice;
use crate::capture::{self, CaptureInfo, SourceKind};
use crate::health::{self, HealthInput};
use crate::stats::{
    self, push, LayerRow, RawAudioIn, RawAudioOut, RawIn, RawOut, ReceiveRow, Snapshot,
};

pub const VIEWER_STATS_TOPIC: &str = "workbench.viewer-stats";
pub const PUBLISHER_STATS_TOPIC: &str = "workbench.publisher-stats";
/// Identity prefix of a Chrome publisher started from the app.
pub const BROWSER_PUB_PREFIX: &str = "chrome-pub-";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A connect through the impairment relay normally takes under a second.
/// About one in twenty never connects (no packet passes the relay for one
/// connection), so the first try gives up early and a fresh relay is tried.
const RELAY_FIRST_TRY_TIMEOUT: Duration = Duration::from_secs(5);
const PREVIEW_MAX_W: u32 = 640;
const PREVIEW_INTERVAL: Duration = Duration::from_millis(30);
const HEALTH_WINDOW: usize = 5;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Publishing,
    Unpublishing,
    Disconnecting,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Disconnected => "Disconnected",
            Phase::Connecting => "Connecting…",
            Phase::Connected => "Connected",
            Phase::Publishing => "Publishing",
            Phase::Unpublishing => "Unpublishing…",
            Phase::Disconnecting => "Disconnecting…",
        }
    }
    pub fn connected(self) -> bool {
        matches!(
            self,
            Phase::Connected | Phase::Publishing | Phase::Unpublishing
        )
    }
}

#[derive(Clone, Debug)]
pub struct ConnectConfig {
    pub url: String,
    pub api_key: String,
    pub api_secret: String,
    pub room: String,
    pub decode: bool,
    pub preview: bool,
    /// End-to-end encryption with this shared passphrase (AES-GCM frame
    /// encryption); both connections use it, so the subscriber can decrypt.
    pub e2ee_key: Option<String>,
    /// Route both connections through the in-app relay that applies the
    /// impairment settings in `Shared::links`.
    pub impair: bool,
}

fn encryption(cfg: &ConnectConfig) -> Option<livekit::e2ee::E2eeOptions> {
    use livekit::e2ee::key_provider::{KeyProvider, KeyProviderOptions};
    let key = cfg.e2ee_key.as_ref()?;
    Some(livekit::e2ee::E2eeOptions {
        encryption_type: livekit::e2ee::EncryptionType::Gcm,
        key_provider: KeyProvider::with_shared_key(
            KeyProviderOptions::default(),
            key.as_bytes().to_vec(),
        ),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum Layering {
    Single,
    Simulcast,
    Svc(String),
}

/// Where the published audio comes from.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum AudioInput {
    #[default]
    Off,
    /// A generated beep: needs no microphone and plays nothing locally.
    Tone,
    /// A microphone by device name, through the SDK's platform audio. Received
    /// audio then plays through the speakers too.
    Microphone(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioConfig {
    pub input: AudioInput,
    /// Opus maximum bitrate.
    pub bitrate_bps: u64,
    /// Opus discontinuous transmission: send almost nothing during silence.
    pub dtx: bool,
    /// Redundant audio: each packet also carries the previous frames.
    pub red: bool,
    /// Microphone processing (WebRTC's audio processing module).
    pub echo_cancellation: bool,
    pub auto_gain: bool,
    pub noise_suppression: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            input: AudioInput::Off,
            bitrate_bps: 32_000,
            dtx: true,
            red: true,
            echo_cancellation: true,
            auto_gain: true,
            noise_suppression: true,
        }
    }
}

impl AudioInput {
    pub fn label(&self) -> String {
        match self {
            AudioInput::Off => "Off".into(),
            AudioInput::Tone => "Synthetic tone".into(),
            AudioInput::Microphone(name) => name.clone(),
        }
    }
}

/// The microphones the SDK's platform audio can open.
pub fn microphones() -> Result<Vec<String>, String> {
    PlatformAudio::new()
        .map(|a| a.recording_devices().map(|d| d.name).collect())
        .map_err(|e| format!("could not list microphones: {e}"))
}

#[derive(Clone, Debug, PartialEq)]
pub struct PublishConfig {
    pub source: SourceKind,
    pub capture_w: u32,
    pub capture_h: u32,
    pub fps: u32,
    /// Encoder frame-rate ceiling (the track's max_framerate).
    pub max_fps: u32,
    /// Publish resolution; None = the capture size.
    pub publish_size: Option<(u32, u32)>,
    pub codec: VideoCodec,
    pub encoder: EncoderChoice,
    pub bitrate_bps: u64,
    pub degradation: DegradationPreference,
    pub layering: Layering,
    /// Publish as a screen-share track (the SDK then seeds a higher start bitrate).
    pub screen_share: bool,
    /// Tell WebRTC the content is detail (screencast) rather than motion.
    pub detail_hint: bool,
    pub audio: AudioConfig,
}

impl PublishConfig {
    pub fn publish_dims(&self) -> (u32, u32) {
        self.publish_size
            .unwrap_or((self.capture_w, self.capture_h))
    }
}

/// A small RGBA thumbnail for the UI. `seq` changes whenever the pixels do.
#[derive(Default)]
pub struct Preview {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
    pub seq: u64,
    pub on_update: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Width in pixels the UI draws this preview at (0: not drawn yet).
    /// Frames are scaled to it, up to the frame's own width, so an undocked
    /// preview is as sharp as the frame itself.
    pub want_w: u32,
}

pub type SharedPreview = Arc<Mutex<Preview>>;

#[derive(Default)]
pub struct Shared {
    pub phase: Phase,
    pub error: Option<String>,
    pub room: String,
    pub snapshot: Snapshot,
    pub active_publish: Option<PublishConfig>,
    pub local_preview: SharedPreview,
    pub remote_preview: SharedPreview,
    /// Connections still closing in the background after a disconnect.
    pub closing: Arc<std::sync::atomic::AtomicUsize>,
    /// Why the preview-only capture failed, cleared by its first frame.
    pub preview_error: Option<String>,
    /// The subscriber's frame decryption state when E2EE is on: "Ok",
    /// "DecryptionFailed", "MissingKey"…
    pub e2ee_state: Option<String>,
    /// Impairment settings and counters, live while connected through the relay.
    pub links: crate::impair::Links,
}

enum Command {
    Connect(ConnectConfig, Option<PublishConfig>),
    Publish(PublishConfig),
    Republish(PublishConfig),
    Unpublish,
    Disconnect,
    RequestSize(Option<(u32, u32)>),
    SwitchSource(PublishConfig),
    /// The source to show in the local preview while not publishing (None: off).
    SetPreview(Option<PublishConfig>),
}

/// The UI's handle to the engine.
#[derive(Clone)]
pub struct Session {
    tx: mpsc::UnboundedSender<Command>,
    pub shared: Arc<Mutex<Shared>>,
}

impl Session {
    pub fn start(rt: &tokio::runtime::Handle, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        {
            let s = shared.lock();
            s.local_preview.lock().on_update = Some(repaint.clone());
            s.remote_preview.lock().on_update = Some(repaint.clone());
        }
        rt.spawn(Engine::new(shared.clone(), repaint).run(rx));
        Self { tx, shared }
    }

    pub fn connect(&self, cfg: ConnectConfig, publish: Option<PublishConfig>) {
        let _ = self.tx.send(Command::Connect(cfg, publish));
    }
    pub fn publish(&self, cfg: PublishConfig) {
        let _ = self.tx.send(Command::Publish(cfg));
    }
    pub fn republish(&self, cfg: PublishConfig) {
        let _ = self.tx.send(Command::Republish(cfg));
    }
    pub fn unpublish(&self) {
        let _ = self.tx.send(Command::Unpublish);
    }
    pub fn disconnect(&self) {
        let _ = self.tx.send(Command::Disconnect);
    }
    pub fn request_size(&self, size: Option<(u32, u32)>) {
        let _ = self.tx.send(Command::RequestSize(size));
    }
    /// Change the input or capture format without republishing. Frames are
    /// scaled to the size the track was published with.
    /// Show this source in the local preview whenever nothing is being
    /// published. The engine hands the source over to the publisher.
    pub fn set_preview(&self, cfg: Option<PublishConfig>) {
        let _ = self.tx.send(Command::SetPreview(cfg));
    }
    pub fn switch_source(&self, cfg: PublishConfig) {
        let _ = self.tx.send(Command::SwitchSource(cfg));
    }
    pub fn phase(&self) -> Phase {
        self.shared.lock().phase
    }
}

/// Subscribe-only token for an extra viewer, such as a browser. It may send
/// data so the viewer page can report its own stats back to the app.
pub fn viewer_token(api_key: &str, api_secret: &str, room: &str, identity: &str) -> Result<String> {
    access_token::AccessToken::with_api_key(api_key, api_secret)
        .with_identity(identity)
        .with_name(identity)
        .with_ttl(Duration::from_secs(6 * 3600))
        .with_grants(access_token::VideoGrants {
            room_join: true,
            room: room.to_string(),
            can_publish: Some(false),
            can_subscribe: Some(true),
            can_publish_data: Some(true),
            ..Default::default()
        })
        .to_jwt()
        .context("creating viewer token")
}

fn token(cfg: &ConnectConfig, identity: &str, publish: bool) -> Result<String> {
    access_token::AccessToken::with_api_key(&cfg.api_key, &cfg.api_secret)
        .with_identity(identity)
        .with_name(identity)
        .with_grants(access_token::VideoGrants {
            room_join: true,
            room: cfg.room.clone(),
            can_publish: Some(publish),
            can_subscribe: Some(!publish),
            can_publish_data: Some(true),
            ..Default::default()
        })
        .to_jwt()
        .context("creating access token")
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

struct Remote {
    track: RemoteVideoTrack,
    publication: RemoteTrackPublication,
    original: TrackDimension,
    consumer: Option<tokio::task::JoinHandle<()>>,
}

struct PublishedAudio {
    track: LocalAudioTrack,
    sid: TrackSid,
    /// The tone generator, when the input is the tone.
    task: Option<ToneTask>,
    /// Holds the microphone (and speaker playout) open while published.
    platform: Option<PlatformAudio>,
    label: String,
}

struct Published {
    audio: Option<PublishedAudio>,
    track: LocalVideoTrack,
    video_source: NativeVideoSource,
    sid: TrackSid,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    captured: Arc<AtomicU64>,
    last_frame: Arc<Mutex<Instant>>,
    info: Arc<Mutex<CaptureInfo>>,
    error: Arc<Mutex<Option<String>>>,
    /// rid -> (target width, height, max bitrate bps, max fps)
    targets: HashMap<String, (u32, u32, u64, f64)>,
    cfg: PublishConfig,
}

#[derive(Default)]
struct Prev {
    at: Option<Instant>,
    outs: HashMap<String, RawOut>,
    inn: Option<RawIn>,
    captured: u64,
    audio_out: Option<RawAudioOut>,
    audio_in: Option<RawAudioIn>,
    links: Option<(crate::impair::LinkCounters, crate::impair::LinkCounters)>,
}

struct Connection {
    cfg: ConnectConfig,
    sub_room: Room,
    sub_events: mpsc::UnboundedReceiver<RoomEvent>,
    pub_room: Room,
    pub_drain: tokio::task::JoinHandle<()>,
    remote: Option<Remote>,
    /// Our publisher's audio as the subscriber receives it.
    remote_audio: Option<(RemoteAudioTrack, TrackSid)>,
    published: Option<Published>,
    sink_frames: Arc<AtomicU64>,
    viewers: HashMap<String, (ReceiveRow, Instant)>,
    browser_pub: Option<(crate::stats::BrowserPublisher, Instant)>,
    following_chrome: bool,
    prev: Prev,
    health_window: VecDeque<HealthInput>,
    requested: Option<((u32, u32), Instant)>,
    /// The server's own rating of each connection (ConnectionQualityUpdate).
    quality_pub: Option<ConnectionQuality>,
    quality_sub: Option<ConnectionQuality>,
    relay: Option<crate::impair::Relay>,
}

struct Engine {
    shared: Arc<Mutex<Shared>>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    conn: Option<Connection>,
    /// Preview-only capture while not publishing.
    standby_cfg: Option<PublishConfig>,
    standby: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
}

impl Engine {
    fn new(shared: Arc<Mutex<Shared>>, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            shared,
            repaint,
            conn: None,
            standby_cfg: None,
            standby: None,
        }
    }

    fn set_phase(&self, phase: Phase) {
        self.shared.lock().phase = phase;
        (self.repaint)();
    }

    fn set_error(&self, e: Option<String>) {
        self.shared.lock().error = e;
        (self.repaint)();
    }

    fn event(&self, text: impl Into<String>) {
        self.shared.lock().snapshot.event(text);
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Command>) {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let sub_event = async {
                match self.conn.as_mut() {
                    Some(c) => c.sub_events.recv().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    self.handle(cmd).await;
                }
                ev = sub_event => {
                    match ev {
                        Some(ev) => self.on_sub_event(ev),
                        None => self.on_connection_lost("Subscriber connection closed").await,
                    }
                }
                _ = tick.tick() => {
                    if self.conn.is_some() {
                        self.sample().await;
                    }
                }
            }
        }
        self.disconnect().await;
    }

    async fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Connect(cfg, publish) => {
                if self.conn.is_some() {
                    return;
                }
                self.set_error(None);
                self.set_phase(Phase::Connecting);
                match self.connect(cfg).await {
                    Ok(()) => {
                        self.set_phase(Phase::Connected);
                        if let Some(p) = publish {
                            self.publish(p).await;
                        }
                    }
                    Err(e) => {
                        self.set_error(Some(format!("{e:#}")));
                        self.set_phase(Phase::Disconnected);
                    }
                }
            }
            Command::Publish(p) => self.publish(p).await,
            Command::Republish(p) => {
                self.unpublish().await;
                self.publish(p).await;
            }
            Command::Unpublish => self.unpublish().await,
            Command::Disconnect => self.disconnect().await,
            Command::RequestSize(size) => self.request_size(size),
            Command::SwitchSource(p) => self.switch_source(p).await,
            Command::SetPreview(cfg) => {
                let running = self.standby.as_ref().is_some_and(|(_, t)| !t.is_finished());
                // The preview uses only the capture settings: a change to codec,
                // bitrate or layers must not reopen the source.
                let same_capture = match (&cfg, &self.standby_cfg) {
                    (Some(a), Some(b)) => {
                        a.source == b.source
                            && (a.capture_w, a.capture_h, a.fps)
                                == (b.capture_w, b.capture_h, b.fps)
                    }
                    (None, None) => true,
                    _ => false,
                };
                if same_capture && (cfg.is_none() || running) {
                    self.standby_cfg = cfg;
                    return;
                }
                self.stop_standby().await;
                self.standby_cfg = cfg;
                self.start_standby();
            }
        }
    }

    fn publishing(&self) -> bool {
        self.conn.as_ref().is_some_and(|c| c.published.is_some())
    }

    /// Start the preview-only capture if a source is set and nothing is published.
    fn start_standby(&mut self) {
        if self.standby.is_some() || self.publishing() {
            return;
        }
        let Some(cfg) = self.standby_cfg.clone() else {
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let shared = self.shared.clone();
        let repaint = self.repaint.clone();
        let preview = shared.lock().local_preview.clone();
        let flag = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("preview".into())
            .spawn(move || {
                let mut last = Instant::now() - PREVIEW_INTERVAL;
                let result = capture::run(
                    &cfg.source,
                    cfg.capture_w,
                    cfg.capture_h,
                    cfg.fps,
                    &flag,
                    |i| {
                        let mut s = shared.lock();
                        s.snapshot.capture.width = i.width;
                        s.snapshot.capture.height = i.height;
                        s.snapshot.capture.note = i.note;
                    },
                    |mut buf| {
                        if last.elapsed() >= PREVIEW_INTERVAL {
                            last = Instant::now();
                            write_preview(&preview, &mut buf);
                            let mut s = shared.lock();
                            if s.preview_error.is_some() {
                                s.preview_error = None;
                            }
                        }
                    },
                );
                if let Err(e) = result {
                    log::info!("preview capture: {e:#}");
                    shared.lock().preview_error = Some(format!("{e:#}"));
                    (repaint)();
                }
            });
        match spawned {
            Ok(t) => self.standby = Some((stop, t)),
            Err(e) => log::warn!("preview capture: {e}"),
        }
    }

    /// Stop the preview-only capture (before publishing opens the source).
    async fn stop_standby(&mut self) {
        if let Some((stop, t)) = self.standby.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::task::spawn_blocking(move || t.join()),
            )
            .await;
            clear_preview(&self.shared.lock().local_preview);
        }
    }

    async fn connect(&mut self, cfg: ConnectConfig) -> Result<()> {
        if !cfg.impair {
            let conn = self.connect_rooms(cfg, None, CONNECT_TIMEOUT).await?;
            self.conn = Some(conn);
            return Ok(());
        }
        let links = self.shared.lock().links.clone();
        let mut last_err = None;
        // WB_RELAY_FIRST_TRY_MS shortens the first try, to exercise the retry in tests.
        let first = std::env::var("WB_RELAY_FIRST_TRY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(RELAY_FIRST_TRY_TIMEOUT);
        for (attempt, timeout) in [first, CONNECT_TIMEOUT].into_iter().enumerate() {
            if attempt > 0 {
                self.event("Connecting through the relay timed out; trying again with a new relay");
            }
            let relay = crate::impair::Relay::start(&cfg.url, &links).await?;
            match self.connect_rooms(cfg.clone(), Some(&relay), timeout).await {
                Ok(mut conn) => {
                    links.up.armed.store(true, Ordering::Relaxed);
                    links.down.armed.store(true, Ordering::Relaxed);
                    conn.relay = Some(relay);
                    self.conn = Some(conn);
                    return Ok(());
                }
                Err(e) => {
                    relay.close().await;
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.expect("at least one attempt"))
    }

    async fn connect_rooms(
        &mut self,
        cfg: ConnectConfig,
        relay: Option<&crate::impair::Relay>,
        timeout: Duration,
    ) -> Result<Connection> {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() % 100_000)
            .unwrap_or(0);

        let mut sub_opts = RoomOptions::default();
        if let Some(r) = relay {
            sub_opts.rtc_config = r.rtc_config(false);
        }
        sub_opts.auto_subscribe = true;
        sub_opts.adaptive_stream = false;
        sub_opts.encryption = encryption(&cfg);
        let sub_token = token(&cfg, &format!("workbench-sub-{suffix}"), false)?;
        let (sub_room, sub_events) =
            tokio::time::timeout(timeout, Room::connect(&cfg.url, &sub_token, sub_opts))
                .await
                .map_err(|_| anyhow!("Timed out connecting to {}", cfg.url))?
                .with_context(|| format!("Could not connect to {}", cfg.url))?;

        let pub_token = token(&cfg, &format!("workbench-pub-{suffix}"), true)?;
        let pub_result = tokio::time::timeout(
            timeout,
            Room::connect(&cfg.url, &pub_token, {
                let mut o = RoomOptions::default();
                o.encryption = encryption(&cfg);
                if let Some(r) = relay {
                    o.rtc_config = r.rtc_config(true);
                }
                o
            }),
        )
        .await;
        let (pub_room, mut pub_events) = match pub_result {
            Ok(Ok(x)) => x,
            Ok(Err(e)) => {
                let _ = sub_room.close().await;
                return Err(anyhow!(e)).context("Publisher could not connect");
            }
            Err(_) => {
                let _ = sub_room.close().await;
                return Err(anyhow!("Timed out connecting the publisher"));
            }
        };
        // Keep draining the publisher's events for the whole session: closing a
        // room whose event receiver is gone stalls for seconds.
        let pub_drain = tokio::spawn(async move { while pub_events.recv().await.is_some() {} });

        {
            let mut s = self.shared.lock();
            s.room = cfg.room.clone();
            s.snapshot = Snapshot::default();
        }
        self.event(format!("Connected to room \"{}\"", cfg.room));
        if relay.is_some() {
            self.event("Both connections go through the in-app impairment relay");
        }
        Ok(Connection {
            cfg,
            sub_room,
            sub_events,
            pub_room,
            pub_drain,
            remote: None,
            remote_audio: None,
            published: None,
            sink_frames: Arc::new(AtomicU64::new(0)),
            viewers: HashMap::new(),
            browser_pub: None,
            following_chrome: false,
            prev: Prev::default(),
            health_window: VecDeque::new(),
            requested: None,
            quality_pub: None,
            quality_sub: None,
            relay: None,
        })
    }

    async fn publish(&mut self, cfg: PublishConfig) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        if conn.published.is_some() {
            return;
        }
        // The preview-only capture holds the source (a camera opens once).
        self.stop_standby().await;
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        let (pub_w, pub_h) = cfg.publish_dims();
        let source = NativeVideoSource::new(
            VideoResolution {
                width: pub_w,
                height: pub_h,
            },
            cfg.detail_hint,
        );
        let track = LocalVideoTrack::create_video_track(
            "workbench",
            RtcVideoSource::Native(source.clone()),
        );
        let opts = TrackPublishOptions {
            source: if cfg.screen_share {
                TrackSource::Screenshare
            } else {
                TrackSource::Camera
            },
            simulcast: cfg.layering == Layering::Simulcast,
            scalability_mode: match &cfg.layering {
                Layering::Svc(mode) => Some(mode.clone()),
                _ => None,
            },
            video_codec: cfg.codec,
            video_encoder: cfg.encoder.backend(),
            video_encoding: Some(VideoEncoding {
                max_bitrate: cfg.bitrate_bps,
                max_framerate: cfg.max_fps as f64,
            }),
            degradation_preference: Some(cfg.degradation),
            ..Default::default()
        };
        let targets: HashMap<String, (u32, u32, u64, f64)> =
            compute_video_encodings(pub_w, pub_h, &opts)
                .into_iter()
                .map(|e| {
                    let scale = e.scale_resolution_down_by.unwrap_or(1.0).max(1.0);
                    (
                        e.rid.clone(),
                        (
                            (pub_w as f64 / scale).round() as u32,
                            (pub_h as f64 / scale).round() as u32,
                            e.max_bitrate.unwrap_or(cfg.bitrate_bps),
                            e.max_framerate.unwrap_or(cfg.max_fps as f64),
                        ),
                    )
                })
                .collect();

        let publication = match conn
            .pub_room
            .local_participant()
            .publish_track(LocalTrack::Video(track.clone()), opts)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                self.set_error(Some(format!(
                    "Could not publish: {e}. The codec, size or encoder may not be supported here."
                )));
                return;
            }
        };

        let counters = CaptureCounters::default();
        let preview = self.shared.lock().local_preview.clone();
        let thread = match spawn_capture(&cfg, (pub_w, pub_h), source.clone(), &counters, preview) {
            Ok(t) => Some(t),
            Err(e) => {
                self.set_error(Some(format!("Could not start capture: {e}")));
                None
            }
        };

        let audio = match self.conn.as_ref() {
            Some(c) if cfg.audio.input != AudioInput::Off => {
                Some(publish_audio(&c.pub_room, &cfg.audio).await)
            }
            _ => None,
        };
        let audio = match audio {
            Some(Ok(a)) => Some(a),
            Some(Err(e)) => {
                self.event(format!("Audio not published: {e}"));
                None
            }
            None => None,
        };

        let sid = publication.sid();
        let label = format!(
            "Published {} {}×{} @ {} fps, {}, cap {:.1} Mbps",
            crate::capabilities::codec_name(cfg.codec),
            pub_w,
            pub_h,
            cfg.fps.min(cfg.max_fps),
            match &cfg.layering {
                Layering::Single => "single layer".to_string(),
                Layering::Simulcast => "simulcast".to_string(),
                Layering::Svc(m) => format!("SVC {m}"),
            },
            cfg.bitrate_bps as f64 / 1e6
        );
        if let Some(conn) = self.conn.as_mut() {
            conn.prev = Prev::default();
            conn.health_window.clear();
            conn.published = Some(Published {
                audio,
                track,
                video_source: source,
                sid,
                stop: counters.stop,
                thread,
                captured: counters.captured,
                last_frame: counters.last_frame,
                info: counters.info,
                error: counters.error,
                targets,
                cfg: cfg.clone(),
            });
        }
        let audio_label = self
            .conn
            .as_ref()
            .and_then(|c| c.published.as_ref())
            .and_then(|p| p.audio.as_ref())
            .map(|a| a.label.clone());
        self.shared.lock().active_publish = Some(cfg);
        self.event(label);
        if let Some(l) = audio_label {
            self.event(l);
        }
        self.set_phase(Phase::Publishing);
    }

    async fn switch_source(&mut self, cfg: PublishConfig) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        let Some(p) = conn.published.as_mut() else {
            return;
        };
        p.stop.store(true, Ordering::SeqCst);
        if let Some(t) = p.thread.take() {
            // Capture threads check their stop flag often; never wait forever.
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::task::spawn_blocking(move || t.join()),
            )
            .await;
        }
        let dims = p.cfg.publish_dims();
        let counters = CaptureCounters::default();
        let preview = self.shared.lock().local_preview.clone();
        let spawned = spawn_capture(&cfg, dims, p.video_source.clone(), &counters, preview);
        p.stop = counters.stop;
        p.captured = counters.captured;
        p.last_frame = counters.last_frame;
        p.info = counters.info;
        p.error = counters.error;
        p.cfg.source = cfg.source.clone();
        p.cfg.capture_w = cfg.capture_w;
        p.cfg.capture_h = cfg.capture_h;
        p.cfg.fps = cfg.fps;
        conn.prev.captured = 0;
        let msg = match spawned {
            Ok(t) => {
                p.thread = Some(t);
                format!(
                    "Input switched to {} {}×{}",
                    cfg.source.label(),
                    cfg.capture_w,
                    cfg.capture_h
                )
            }
            Err(e) => format!("Could not restart capture: {e}"),
        };
        let active = p.cfg.clone();
        let restarted = p.thread.is_some();
        {
            let mut sh = self.shared.lock();
            sh.active_publish = Some(active);
            // A new input clears the previous input's failure.
            if restarted
                && sh
                    .error
                    .as_deref()
                    .is_some_and(|e| e.starts_with("Input stopped"))
            {
                sh.error = None;
            }
        }
        self.event(msg);
    }

    async fn unpublish(&mut self) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        let Some(mut p) = conn.published.take() else {
            return;
        };
        self.shared.lock().phase = Phase::Unpublishing;
        (self.repaint)();
        p.stop.store(true, Ordering::SeqCst);
        if let Some(t) = p.thread.take() {
            // Capture threads check their stop flag often; never wait forever.
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::task::spawn_blocking(move || t.join()),
            )
            .await;
        }
        let _ = conn
            .pub_room
            .local_participant()
            .unpublish_track(&p.sid)
            .await;
        if let Some(a) = p.audio.take() {
            stop_audio(&conn.pub_room, a).await;
        }
        drop(p.track);
        {
            let mut s = self.shared.lock();
            s.active_publish = None;
            clear_preview(&s.local_preview);
        }
        self.event("Unpublished");
        self.set_phase(Phase::Connected);
        self.start_standby();
    }

    async fn disconnect(&mut self) {
        let Some(mut conn) = self.conn.take() else {
            self.set_phase(Phase::Disconnected);
            return;
        };
        self.set_phase(Phase::Disconnecting);
        let mut published_sid = None;
        let mut audio_sid = None;
        if let Some(mut p) = conn.published.take() {
            p.stop.store(true, Ordering::SeqCst);
            if let Some(t) = p.thread.take() {
                // Capture threads check their stop flag often; never wait forever.
                let _ = tokio::time::timeout(
                    Duration::from_secs(3),
                    tokio::task::spawn_blocking(move || t.join()),
                )
                .await;
            }
            published_sid = Some(p.sid.clone());
            if let Some(a) = p.audio.take() {
                if let Some(t) = &a.task {
                    t.abort();
                }
                audio_sid = Some(a.sid.clone());
                drop(a.platform);
            }
        }
        conn.remote_audio = None;
        if let Some(r) = conn.remote.take() {
            if let Some(c) = r.consumer {
                c.abort();
            }
        }
        // Unpublish first: it is quick, and viewers see the track end at once.
        for sid in [published_sid, audio_sid].into_iter().flatten() {
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                conn.pub_room.local_participant().unpublish_track(&sid),
            )
            .await;
        }
        // Closing the peer connections can take seconds on machines whose network
        // filters slow down socket close, so it finishes in the background.
        // Both rooms keep their event receivers drained meanwhile (see connect).
        let (pub_room, sub_room, pub_drain) = (conn.pub_room, conn.sub_room, conn.pub_drain);
        let relay = conn.relay.take();
        {
            let links = self.shared.lock().links.clone();
            links.up.armed.store(false, Ordering::Relaxed);
            links.down.armed.store(false, Ordering::Relaxed);
        }
        let mut sub_events = conn.sub_events;
        let closing = self.shared.lock().closing.clone();
        closing.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            let sub_drain = tokio::spawn(async move { while sub_events.recv().await.is_some() {} });
            let t0 = Instant::now();
            tokio::join!(
                async {
                    let _ = pub_room.close().await;
                },
                async {
                    let _ = sub_room.close().await;
                }
            );
            log::info!("rooms closed in {} ms", t0.elapsed().as_millis());
            if let Some(r) = relay {
                r.close().await;
            }
            pub_drain.abort();
            sub_drain.abort();
            closing.fetch_sub(1, Ordering::SeqCst);
        });
        {
            let mut s = self.shared.lock();
            s.active_publish = None;
            s.e2ee_state = None;
            clear_preview(&s.local_preview);
            clear_preview(&s.remote_preview);
        }
        self.event("Disconnected");
        self.set_phase(Phase::Disconnected);
        self.start_standby();
    }

    async fn on_connection_lost(&mut self, why: &str) {
        self.set_error(Some(why.to_string()));
        self.disconnect().await;
    }

    /// Remember what the subscriber asks the SFU for and send it now if the
    /// track is subscribed; it is sent again whenever the track is re-subscribed.
    fn request_size(&mut self, size: Option<(u32, u32)>) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        conn.requested = size.map(|s| (s, Instant::now()));
        if let Some(remote) = conn.remote.as_ref() {
            apply_request(remote, size);
        }
    }

    fn on_sub_event(&mut self, ev: RoomEvent) {
        let decode = self.conn.as_ref().map(|c| c.cfg.decode).unwrap_or(false);
        let preview_on = self.conn.as_ref().map(|c| c.cfg.preview).unwrap_or(false);
        match ev {
            RoomEvent::TrackSubscribed {
                track: RemoteTrack::Video(v),
                publication,
                participant,
            } => {
                let Some(conn) = self.conn.as_mut() else {
                    return;
                };
                // Follow our own publisher, or a Chrome publisher started from
                // the app; a Chrome stream, once followed, takes precedence.
                let id = participant.identity().to_string();
                let from_chrome = id.starts_with(BROWSER_PUB_PREFIX);
                if !id.starts_with("workbench-pub-") && !from_chrome {
                    return;
                }
                if !from_chrome && conn.following_chrome && conn.remote.is_some() {
                    return;
                }
                conn.following_chrome = from_chrome;
                let consumer = decode.then(|| {
                    let frames = conn.sink_frames.clone();
                    let preview = preview_on.then(|| self.shared.lock().remote_preview.clone());
                    let mut stream = NativeVideoStream::new(v.rtc_track());
                    tokio::spawn(async move {
                        let mut last = Instant::now() - PREVIEW_INTERVAL;
                        while let Some(frame) = stream.next().await {
                            frames.fetch_add(1, Ordering::Relaxed);
                            if let Some(slot) = &preview {
                                if last.elapsed() >= PREVIEW_INTERVAL {
                                    last = Instant::now();
                                    let mut i420 = frame.buffer.to_i420();
                                    write_preview(slot, &mut i420);
                                }
                            }
                        }
                    })
                });
                let original = publication.dimension();
                if let Some(old) = conn.remote.take() {
                    if let Some(c) = old.consumer {
                        c.abort();
                    }
                }
                conn.remote = Some(Remote {
                    track: v,
                    publication,
                    original,
                    consumer,
                });
                if let (Some(r), Some((size, _))) = (conn.remote.as_ref(), conn.requested) {
                    apply_request(r, Some(size));
                    conn.requested = Some((size, Instant::now()));
                }
                self.event("Subscriber receiving the published video");
            }
            RoomEvent::TrackSubscribed {
                track: RemoteTrack::Audio(a),
                publication,
                participant,
            } => {
                if !participant
                    .identity()
                    .to_string()
                    .starts_with("workbench-pub-")
                {
                    return;
                }
                if let Some(conn) = self.conn.as_mut() {
                    conn.remote_audio = Some((a, publication.sid()));
                    conn.prev.audio_in = None;
                }
                self.event("Subscriber receiving the published audio");
            }
            RoomEvent::TrackUnsubscribed {
                track: RemoteTrack::Audio(_),
                publication,
                ..
            } => {
                if let Some(conn) = self.conn.as_mut() {
                    if conn.remote_audio.as_ref().map(|(_, sid)| sid) == Some(&publication.sid()) {
                        conn.remote_audio = None;
                    }
                }
            }
            RoomEvent::TrackUnsubscribed {
                track: RemoteTrack::Video(_),
                publication,
                ..
            } => {
                // On a republish the new track can be subscribed before the old
                // one is reported gone: only drop the track this event is about.
                let Some(conn) = self.conn.as_mut() else {
                    return;
                };
                if conn.remote.as_ref().map(|r| r.publication.sid()) != Some(publication.sid()) {
                    return;
                }
                if let Some(r) = conn.remote.take() {
                    if let Some(c) = r.consumer {
                        c.abort();
                    }
                }
                conn.following_chrome = false;
                clear_preview(&self.shared.lock().remote_preview);
            }
            RoomEvent::ParticipantConnected(p) => {
                let id = p.identity().to_string();
                if !id.starts_with("workbench-") {
                    self.event(format!("{id} joined"));
                }
            }
            RoomEvent::ParticipantDisconnected(p) => {
                let id = p.identity().to_string();
                if let Some(conn) = self.conn.as_mut() {
                    conn.viewers.remove(&id);
                }
                if !id.starts_with("workbench-") {
                    self.event(format!("{id} left"));
                }
            }
            RoomEvent::ConnectionQualityChanged {
                participant,
                quality,
            } => {
                let Some(conn) = self.conn.as_mut() else {
                    return;
                };
                let id = participant.identity().to_string();
                let (slot, who) = if matches!(participant, Participant::Local(_)) {
                    (&mut conn.quality_sub, "subscriber")
                } else if id.starts_with("workbench-pub-") {
                    (&mut conn.quality_pub, "publisher")
                } else {
                    return;
                };
                if *slot != Some(quality) {
                    if slot.is_some() {
                        let text = format!(
                            "Server rates the {who} connection {}",
                            quality_label(quality).to_lowercase()
                        );
                        *slot = Some(quality);
                        self.event(text);
                    } else {
                        *slot = Some(quality);
                    }
                }
            }
            RoomEvent::E2eeStateChanged { state, .. } => {
                let text = format!("{state:?}");
                let changed = {
                    let mut s = self.shared.lock();
                    let changed = s.e2ee_state.as_deref() != Some(text.as_str());
                    s.e2ee_state = Some(text.clone());
                    changed
                };
                if changed {
                    self.event(format!("E2EE: subscriber {text}"));
                }
            }
            RoomEvent::DataReceived {
                payload,
                topic,
                participant,
                ..
            } => {
                let who = participant
                    .map(|p| p.identity().to_string())
                    .unwrap_or_else(|| "browser".into());
                if topic.as_deref() == Some(PUBLISHER_STATS_TOPIC) {
                    if let (Some(row), Some(conn)) =
                        (parse_publisher_stats(&who, &payload), self.conn.as_mut())
                    {
                        conn.browser_pub = Some((row, Instant::now()));
                    }
                    return;
                }
                if topic.as_deref() != Some(VIEWER_STATS_TOPIC) {
                    return;
                }
                if let Some(row) = parse_viewer_stats(&who, &payload) {
                    if let Some(conn) = self.conn.as_mut() {
                        conn.viewers.insert(who, (row, Instant::now()));
                    }
                }
            }
            RoomEvent::Disconnected { reason } => {
                self.shared.lock().error =
                    Some(format!("Server closed the connection ({reason:?})"));
            }
            _ => {}
        }
    }

    async fn sample(&mut self) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        let now = Instant::now();
        let dt = conn
            .prev
            .at
            .map(|t| now.duration_since(t).as_secs_f64())
            .unwrap_or(1.0)
            .max(0.2);

        // Gather raw stats.
        let (out_stats, transport) = match &conn.published {
            Some(p) => match p.track.get_stats().await {
                Ok(s) => (
                    {
                        if std::env::var_os("WB_CODECS").is_some() {
                            let mut m: Vec<String> = s
                                .iter()
                                .filter_map(|x| match x {
                                    livekit::webrtc::stats::RtcStats::Codec(c) => {
                                        Some(c.codec.mime_type.clone())
                                    }
                                    _ => None,
                                })
                                .collect();
                            m.sort();
                            m.dedup();
                            println!("      publisher codec stats: {m:?}");
                        }
                        stats::outbound(&s)
                    },
                    Some((stats::transport(&s), stats::fec_codecs(&s))),
                ),
                Err(_) => (Vec::new(), None),
            },
            None => (Vec::new(), None),
        };
        let in_stats = match &conn.remote {
            Some(r) => r
                .track
                .get_stats()
                .await
                .ok()
                .and_then(|s| stats::inbound(&s)),
            None => None,
        };
        let (audio_out, audio_label) = match conn.published.as_ref().and_then(|p| p.audio.as_ref())
        {
            Some(a) => (
                a.track
                    .get_stats()
                    .await
                    .ok()
                    .and_then(|s| stats::audio_outbound(&s)),
                a.label.clone(),
            ),
            None => (None, String::new()),
        };
        let audio_in = match &conn.remote_audio {
            Some((t, _)) => t
                .get_stats()
                .await
                .ok()
                .and_then(|s| stats::audio_inbound(&s)),
            None => None,
        };
        let captured = conn
            .published
            .as_ref()
            .map(|p| p.captured.load(Ordering::Relaxed))
            .unwrap_or(0);

        // Capture thread failure ends the publish.
        let capture_error = conn.published.as_ref().and_then(|p| p.error.lock().take());

        let mut links_now = None;
        {
            let s_links = self.shared.lock().links.clone();
            let mut s = self.shared.lock();
            let snap = &mut s.snapshot;
            let first = conn.prev.at.is_none();

            // Capture.
            if let Some(p) = &conn.published {
                let info = p.info.lock().clone();
                snap.capture.width = info.width;
                snap.capture.height = info.height;
                snap.capture.note = info.note;
                snap.capture.fps = if first {
                    0.0
                } else {
                    (captured.saturating_sub(conn.prev.captured)) as f64 / dt
                };
            } else {
                snap.capture = Default::default();
            }

            // Send side, one row per RTP stream.
            let mut layers = Vec::new();
            let (mut total_sent, mut total_target, mut total_max, mut retrans) =
                (0.0, 0.0, 0.0, 0.0);
            let spatial = conn
                .published
                .as_ref()
                .map(|p| spatial_layers(&p.cfg.layering))
                .unwrap_or(1);
            for o in &out_stats {
                let prev = conn.prev.outs.get(&o.rid);
                let target = conn.published.as_ref().and_then(|p| {
                    p.targets
                        .get(&o.rid)
                        .copied()
                        .or_else(|| p.targets.get("").copied())
                        .or_else(|| p.targets.values().next().copied())
                });
                let layer_frames = prev
                    .map(|p| o.frames_encoded.saturating_sub(p.frames_encoded))
                    .unwrap_or(0);
                // With SVC WebRTC counts one encoded frame per spatial layer.
                let frames = layer_frames / spatial;
                let sent_mbps = prev
                    .map(|p| stats::rate_mbps(o.bytes, p.bytes, dt))
                    .unwrap_or(0.0);
                let retrans_mbps = prev
                    .map(|p| stats::rate_mbps(o.retrans_bytes, p.retrans_bytes, dt))
                    .unwrap_or(0.0);
                let total_limit = o.limit_durations.0 + o.limit_durations.1 + o.limit_durations.2;
                let elapsed = conn.published.as_ref().map(|_| 1.0).unwrap_or(1.0);
                let _ = elapsed;
                let row = LayerRow {
                    rid: o.rid.clone(),
                    active: o.active,
                    target_w: target.map(|t| t.0).unwrap_or(0),
                    target_h: target.map(|t| t.1).unwrap_or(0),
                    width: o.width,
                    height: o.height,
                    target_fps: target.map(|t| t.3).unwrap_or(0.0),
                    fps: if prev.is_some() {
                        frames as f64 / dt
                    } else {
                        0.0
                    },
                    max_mbps: target.map(|t| t.2 as f64 / 1e6).unwrap_or(0.0),
                    // For an SVC stream WebRTC's targetBitrate does not cover all
                    // layers, so it is not shown rather than shown wrong.
                    target_mbps: if spatial > 1 { 0.0 } else { o.target_bps / 1e6 },
                    sent_mbps,
                    encoder: o.encoder.clone(),
                    hardware: o.hardware,
                    encode_ms: match prev {
                        Some(p) if frames > 0 => {
                            (o.encode_time - p.encode_time) * 1000.0 / frames as f64
                        }
                        _ => 0.0,
                    },
                    qp: match prev {
                        Some(p) if layer_frames > 0 => {
                            o.qp_sum.saturating_sub(p.qp_sum) as f64 / layer_frames as f64
                        }
                        _ => 0.0,
                    },
                    limit: o.limit.clone(),
                    limit_share_bandwidth: if total_limit > 0.0 {
                        o.limit_durations.0 / total_limit
                    } else {
                        0.0
                    },
                    limit_share_cpu: if total_limit > 0.0 {
                        o.limit_durations.1 / total_limit
                    } else {
                        0.0
                    },
                    limit_share_other: if total_limit > 0.0 {
                        o.limit_durations.2 / total_limit
                    } else {
                        0.0
                    },
                    resolution_changes: o.resolution_changes,
                    keyframes: o.keyframes,
                    pli: o.pli,
                    nack: o.nack,
                    scalability_mode: o.scalability_mode.clone(),
                    codec: o.codec.clone(),
                };
                if let Some(p) = prev {
                    if p.width != 0 && (p.width, p.height) != (o.width, o.height) && o.width != 0 {
                        let name = if o.rid.is_empty() {
                            "stream".to_string()
                        } else {
                            format!("layer {}", o.rid)
                        };
                        let why = if o.limit != "None" {
                            format!(" ({})", o.limit.to_lowercase())
                        } else {
                            String::new()
                        };
                        let msg = format!(
                            "{name}: {}×{} → {}×{}{why}",
                            p.width, p.height, o.width, o.height
                        );
                        snap.events.push_front((events_time(), msg));
                    }
                    if o.pli > p.pli {
                        let msg = format!("Keyframe requested by the server (PLI {})", o.pli);
                        snap.events.push_front((events_time(), msg));
                    }
                }
                total_sent += row.sent_mbps;
                total_target += row.target_mbps;
                if row.active {
                    total_max += row.max_mbps;
                }
                retrans += retrans_mbps;
                push(
                    snap.history.layer_sent.entry(o.rid.clone()).or_default(),
                    row.sent_mbps,
                );
                layers.push(row);
            }
            layers.sort_by_key(|l| std::cmp::Reverse(l.width * l.height));
            snap.layers = layers;
            snap.total_sent_mbps = total_sent;
            snap.total_target_mbps = total_target;
            snap.total_max_mbps = total_max;
            snap.retransmit_mbps = retrans;
            snap.send_codec = out_stats
                .first()
                .map(|o| o.codec.clone())
                .unwrap_or_default();
            if let Some((t, fec)) = transport {
                snap.transport = t;
                snap.fec_negotiated = !fec.is_empty();
                snap.fec = fec;
            }

            // Receive side: this app's subscriber, then browser viewers.
            let mut receive = Vec::new();
            if let Some(i) = &in_stats {
                let p = conn.prev.inn.as_ref();
                let frames = p
                    .map(|p| i.frames_decoded.saturating_sub(p.frames_decoded))
                    .unwrap_or(0);
                let packets = p.map(|p| i.packets.saturating_sub(p.packets)).unwrap_or(0);
                let lost = p.map(|p| (i.lost - p.lost).max(0)).unwrap_or(0) as f64;
                receive.push(ReceiveRow {
                    who: "this app".into(),
                    is_browser: false,
                    width: i.width,
                    height: i.height,
                    fps: if p.is_some() { frames as f64 / dt } else { 0.0 },
                    mbps: p
                        .map(|p| stats::rate_mbps(i.bytes, p.bytes, dt))
                        .unwrap_or(0.0),
                    codec: i.codec.clone(),
                    decoder: i.decoder.clone(),
                    hardware: Some(i.hardware),
                    decode_ms: match p {
                        Some(p) if frames > 0 => {
                            (i.decode_time - p.decode_time) * 1000.0 / frames as f64
                        }
                        _ => 0.0,
                    },
                    jitter_buffer_ms: match p {
                        Some(p) if i.jitter_emitted > p.jitter_emitted => {
                            (i.jitter_delay - p.jitter_delay) * 1000.0
                                / (i.jitter_emitted - p.jitter_emitted) as f64
                        }
                        _ => 0.0,
                    },
                    freezes: i.freezes,
                    freeze_s: i.freeze_s,
                    lost: i.lost,
                    loss_ratio: if packets as f64 + lost > 0.0 {
                        lost / (packets as f64 + lost)
                    } else {
                        0.0
                    },
                    nack: i.nack,
                    pli: i.pli,
                    fec_recovered: i.fec,
                    user_agent: String::new(),
                    requested: conn.requested.map(|(s, _)| s),
                });
            }
            conn.viewers
                .retain(|_, (_, at)| at.elapsed() < Duration::from_secs(10));
            let mut viewers: Vec<ReceiveRow> =
                conn.viewers.values().map(|(r, _)| r.clone()).collect();
            viewers.sort_by(|a, b| a.who.cmp(&b.who));
            receive.extend(viewers);
            snap.receive = receive;

            // Audio, both directions.
            snap.audio = stats::audio_rates(
                audio_out
                    .as_ref()
                    .map(|o| (o, conn.prev.audio_out.as_ref())),
                audio_in.as_ref().map(|i| (i, conn.prev.audio_in.as_ref())),
                dt,
            );
            snap.audio.source = audio_label;

            // Impairment relay, both directions.
            snap.impair = conn.relay.as_ref().map(|_| {
                let up = s_links.up.counters.lock().clone();
                let down = s_links.down.counters.lock().clone();
                let prev = conn.prev.links.as_ref();
                let row = |c: &crate::impair::LinkCounters,
                           p: Option<&crate::impair::LinkCounters>,
                           set: crate::impair::Impairment| {
                    let (passed, lost, full) = match p {
                        Some(p) => (
                            c.passed.saturating_sub(p.passed),
                            c.lost.saturating_sub(p.lost),
                            c.queue_dropped.saturating_sub(p.queue_dropped),
                        ),
                        None => (0, 0, 0),
                    };
                    let total = (passed + lost + full) as f64;
                    stats::LinkStats {
                        settings: set,
                        kbps: p
                            .map(|p| stats::rate_mbps(c.bytes, p.bytes, dt) * 1000.0)
                            .unwrap_or(0.0),
                        dropped_random_pct: if total > 0.0 {
                            lost as f64 * 100.0 / total
                        } else {
                            0.0
                        },
                        dropped_queue_pct: if total > 0.0 {
                            full as f64 * 100.0 / total
                        } else {
                            0.0
                        },
                        queue_ms: c.queue_ms,
                    }
                };
                let r = stats::ImpairStats {
                    up: row(&up, prev.map(|p| &p.0), *s_links.up.settings.lock()),
                    down: row(&down, prev.map(|p| &p.1), *s_links.down.settings.lock()),
                };
                links_now = Some((up, down));
                r
            });

            // Requested tile size.
            snap.requested_size = conn.requested.map(|(s, _)| s);
            snap.server_quality_pub = conn.quality_pub.map(|q| quality_label(q).to_string());
            snap.server_quality_sub = conn.quality_sub.map(|q| quality_label(q).to_string());
            if conn
                .browser_pub
                .as_ref()
                .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(10))
            {
                conn.browser_pub = None;
            }
            snap.browser_pub = conn.browser_pub.as_ref().map(|(b, _)| b.clone());
            snap.requested_ago_s = conn.requested.map(|(_, at)| at.elapsed().as_secs_f64());

            // Histories.
            let top = snap.top_layer().cloned();
            let me = snap.self_receive().cloned();
            let cap_mbps = conn
                .published
                .as_ref()
                .map(|p| p.cfg.bitrate_bps as f64 / 1e6)
                .unwrap_or(0.0);
            let h = &mut snap.history;
            push(
                &mut h.cap,
                if snap.total_max_mbps > 0.0 {
                    snap.total_max_mbps
                } else {
                    cap_mbps
                },
            );
            push(&mut h.target, total_target);
            push(&mut h.sent, total_sent);
            push(&mut h.received, me.as_ref().map(|r| r.mbps).unwrap_or(0.0));
            push(&mut h.capture_h, snap.capture.height as f64);
            push(
                &mut h.encoded_h,
                top.as_ref().map(|t| t.height as f64).unwrap_or(0.0),
            );
            push(
                &mut h.decoded_h,
                me.as_ref().map(|r| r.height as f64).unwrap_or(0.0),
            );
            push(&mut h.capture_fps, snap.capture.fps);
            push(
                &mut h.encode_fps,
                top.as_ref().map(|t| t.fps).unwrap_or(0.0),
            );
            push(&mut h.sent_fps, top.as_ref().map(|t| t.fps).unwrap_or(0.0));
            push(&mut h.decode_fps, me.as_ref().map(|r| r.fps).unwrap_or(0.0));

            // Health over a short window.
            if let Some(p) = &conn.published {
                // A tile larger than the video asks for more than exists: cap it.
                let requested_h = conn
                    .requested
                    .map(|((_, h), _)| h.min(p.cfg.publish_dims().1))
                    .unwrap_or(0);
                let fallback = match (p.cfg.encoder, top.as_ref()) {
                    (EncoderChoice::Hardware, Some(t)) if !t.encoder.is_empty() => !t.hardware,
                    (EncoderChoice::Software, Some(t)) if !t.encoder.is_empty() => t.hardware,
                    _ => false,
                };
                let input = HealthInput {
                    publishing: true,
                    subscribed: me.is_some(),
                    target_fps: p.cfg.fps.min(p.cfg.max_fps) as f64,
                    capture_fps: snap.capture.fps,
                    screen: p.cfg.source == SourceKind::Screen,
                    seconds_since_capture_frame: p.last_frame.lock().elapsed().as_secs_f64(),
                    encode_fps: top.as_ref().map(|t| t.fps).unwrap_or(0.0),
                    encode_ms_per_frame: top.as_ref().map(|t| t.encode_ms).unwrap_or(0.0),
                    encoder_hardware: top.as_ref().map(|t| t.hardware).unwrap_or(false),
                    encoded_height: top.as_ref().map(|t| t.height).unwrap_or(0),
                    target_height: top.as_ref().map(|t| t.target_h).unwrap_or(0),
                    quality_limit: top.as_ref().map(|t| t.limit.clone()).unwrap_or_default(),
                    encoder_fallback: fallback,
                    send_mbps: total_sent,
                    send_target_mbps: total_target,
                    bitrate_cap_mbps: snap.total_max_mbps,
                    retransmit_ratio: if total_sent > 0.0 {
                        retrans / total_sent
                    } else {
                        0.0
                    },
                    seconds_since_send: if total_sent > 0.0 || first { 0.0 } else { dt },
                    requested_height: requested_h,
                    received_height: me.as_ref().map(|r| r.height).unwrap_or(0),
                    server_quality_sub: conn
                        .quality_sub
                        .map(|q| quality_label(q).to_string())
                        .unwrap_or_default(),
                    sent_fps: top.as_ref().map(|t| t.fps).unwrap_or(0.0),
                    decode_fps: me.as_ref().map(|r| r.fps).unwrap_or(0.0),
                    loss_ratio: me.as_ref().map(|r| r.loss_ratio).unwrap_or(0.0),
                    freezes_in_window: 0,
                    longest_freeze_s: 0.0,
                };
                // Rates need two samples of every stream; until then they read 0.
                let rates_ready = !first
                    && out_stats
                        .iter()
                        .all(|o| conn.prev.outs.contains_key(&o.rid))
                    && (in_stats.is_none() || conn.prev.inn.is_some());
                if rates_ready {
                    conn.health_window.push_back(input);
                }
                while conn.health_window.len() > HEALTH_WINDOW {
                    conn.health_window.pop_front();
                }
                // Skip the first samples: rates are not meaningful until two exist.
                if conn.health_window.len() >= 2 {
                    let mut avg = average(&conn.health_window);
                    if let (Some(i), Some(p)) = (&in_stats, conn.prev.inn.as_ref()) {
                        avg.freezes_in_window = i.freezes.saturating_sub(p.freezes);
                        avg.longest_freeze_s = if avg.freezes_in_window > 0 {
                            i.freeze_s - p.freeze_s
                        } else {
                            0.0
                        };
                    }
                    snap.health = health::evaluate(&avg);
                }
            } else {
                snap.health = Default::default();
                conn.health_window.clear();
            }
            snap.seq += 1;
            while snap.events.len() > 200 {
                snap.events.pop_back();
            }
        }

        conn.prev = Prev {
            at: Some(now),
            outs: out_stats.into_iter().map(|o| (o.rid.clone(), o)).collect(),
            inn: in_stats,
            captured,
            audio_out,
            audio_in,
            links: links_now,
        };
        (self.repaint)();

        // An input that fails leaves the track published, so another input
        // can be switched in live; ending the publish here made later input
        // changes silently do nothing.
        if let Some(e) = capture_error {
            self.set_error(Some(format!(
                "Input stopped: {e}. The track is still published: choose another input."
            )));
            self.event(format!("Input stopped: {e}"));
        }
    }
}

#[derive(Clone)]
struct CaptureCounters {
    stop: Arc<AtomicBool>,
    captured: Arc<AtomicU64>,
    last_frame: Arc<Mutex<Instant>>,
    info: Arc<Mutex<CaptureInfo>>,
    error: Arc<Mutex<Option<String>>>,
}

impl Default for CaptureCounters {
    fn default() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            captured: Arc::new(AtomicU64::new(0)),
            last_frame: Arc::new(Mutex::new(Instant::now())),
            info: Arc::new(Mutex::new(CaptureInfo::default())),
            error: Arc::new(Mutex::new(None)),
        }
    }
}

/// Run the source on its own thread, scaling every frame to the publish size.
/// Publish the audio track: a tone from a NativeAudioSource, or a microphone
/// through the SDK's platform audio.
async fn publish_audio(room: &Room, cfg: &AudioConfig) -> Result<PublishedAudio, String> {
    const RATE: u32 = 48_000;
    let (source, task, platform, what) = match &cfg.input {
        AudioInput::Off => return Err("audio is off".into()),
        AudioInput::Tone => {
            let src = NativeAudioSource::new(AudioSourceOptions::default(), RATE, 1, 100);
            let feed = src.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = stop.clone();
            let (aec, agc, ns) = (cfg.echo_cancellation, cfg.auto_gain, cfg.noise_suppression);
            // WebRTC's processing only runs by itself on audio the device
            // captures, so the tone goes through the same module (APM) here.
            // The module is not Send: it lives on this thread.
            std::thread::Builder::new()
                .name("audio-tone".into())
                .spawn(move || {
                    let mut apm = (aec || agc || ns).then(|| {
                        livekit::webrtc::native::apm::AudioProcessingModule::new(
                            aec, agc, false, ns,
                        )
                    });
                    let mut n: u64 = 0;
                    while !stop_flag.load(Ordering::Relaxed) {
                        let mut data = tone_frame(n, RATE);
                        if let Some(apm) = apm.as_mut() {
                            let _ = apm.process_stream(&mut data, RATE as i32, 1);
                        }
                        let frame = AudioFrame {
                            data: data.into(),
                            sample_rate: RATE,
                            num_channels: 1,
                            samples_per_channel: RATE / 100,
                        };
                        // Waits while the source's queue is full, so this runs in real time.
                        if futures::executor::block_on(feed.capture_frame(&frame)).is_err() {
                            break;
                        }
                        n += 1;
                    }
                })
                .map_err(|e| format!("starting the tone: {e}"))?;
            (
                RtcAudioSource::Native(src),
                Some(ToneTask(stop)),
                None,
                "synthetic tone".to_string(),
            )
        }
        AudioInput::Microphone(name) => {
            let audio = PlatformAudio::new().map_err(|e| format!("microphone: {e}"))?;
            let index = audio
                .recording_devices()
                .find(|d| &d.name == name)
                .map(|d| d.index)
                .ok_or_else(|| format!("microphone \"{name}\" is not connected"))?;
            audio
                .set_recording_device_by_index(index)
                .map_err(|e| format!("selecting the microphone: {e}"))?;
            for (what, r) in [
                (
                    "echo cancellation",
                    audio.set_echo_cancellation(cfg.echo_cancellation, false),
                ),
                (
                    "gain control",
                    audio.set_auto_gain_control(cfg.auto_gain, false),
                ),
                (
                    "noise suppression",
                    audio.set_noise_suppression(cfg.noise_suppression, false),
                ),
            ] {
                if let Err(e) = r {
                    log::warn!("microphone {what}: {e}");
                }
            }
            (audio.rtc_source(), None, Some(audio), name.clone())
        }
    };
    let track = LocalAudioTrack::create_audio_track("workbench-audio", source);
    let opts = TrackPublishOptions {
        source: TrackSource::Microphone,
        audio_encoding: Some(AudioEncoding {
            max_bitrate: cfg.bitrate_bps,
        }),
        dtx: cfg.dtx,
        red: cfg.red,
        ..Default::default()
    };
    match room
        .local_participant()
        .publish_track(LocalTrack::Audio(track.clone()), opts)
        .await
    {
        Ok(publication) => Ok(PublishedAudio {
            track,
            sid: publication.sid(),
            task,
            platform,
            label: format!(
                "Published audio from {what}, Opus up to {} kbps{}{}, WebRTC processing: {}",
                cfg.bitrate_bps / 1000,
                if cfg.dtx { ", DTX" } else { "" },
                if cfg.red { ", RED" } else { "" },
                processing_label(cfg)
            ),
        }),
        Err(e) => {
            if let Some(t) = task {
                t.abort();
            }
            Err(e.to_string())
        }
    }
}

/// Stops the tone thread.
struct ToneTask(Arc<AtomicBool>);
impl ToneTask {
    fn abort(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn processing_label(cfg: &AudioConfig) -> String {
    let on: Vec<&str> = [
        (cfg.echo_cancellation, "AEC"),
        (cfg.noise_suppression, "NS"),
        (cfg.auto_gain, "AGC"),
    ]
    .into_iter()
    .filter_map(|(on, n)| on.then_some(n))
    .collect();
    if on.is_empty() {
        "off".into()
    } else {
        on.join(", ")
    }
}

async fn stop_audio(room: &Room, a: PublishedAudio) {
    if let Some(t) = &a.task {
        t.abort();
    }
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        room.local_participant().unpublish_track(&a.sid),
    )
    .await;
    drop(a.platform);
}

/// 10 ms of a 440 Hz beep: 600 ms on, 400 ms off, so DTX has silence to skip.
fn tone_frame(n: u64, rate: u32) -> Vec<i16> {
    let per = (rate / 100) as u64;
    let on = (n % 100) < 60;
    (0..per)
        .map(|i| {
            if !on {
                return 0;
            }
            let t = (n * per + i) as f64 / rate as f64;
            (f64::sin(2.0 * std::f64::consts::PI * 440.0 * t) * 8000.0) as i16
        })
        .collect()
}

fn spawn_capture(
    cfg: &PublishConfig,
    (pub_w, pub_h): (u32, u32),
    source: NativeVideoSource,
    c: &CaptureCounters,
    preview: SharedPreview,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let c = c.clone();
    let cfg = cfg.clone();
    std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            let mut last_preview = Instant::now() - PREVIEW_INTERVAL;
            let result = capture::run(
                &cfg.source,
                cfg.capture_w,
                cfg.capture_h,
                cfg.fps,
                &c.stop,
                |i| *c.info.lock() = i,
                |mut buf| {
                    // Fit within the publish size keeping the frame's shape: a
                    // screen is rarely 16:9, and stretching it distorts text.
                    let (fw, fh) = capture::fit_within(buf.width(), buf.height(), pub_w, pub_h);
                    if (buf.width(), buf.height()) != (fw, fh) {
                        buf = buf.scale(fw as i32, fh as i32);
                    }
                    if last_preview.elapsed() >= PREVIEW_INTERVAL {
                        last_preview = Instant::now();
                        write_preview(&preview, &mut buf);
                    }
                    source.capture_frame(&VideoFrame::new(VideoRotation::VideoRotation0, buf));
                    c.captured.fetch_add(1, Ordering::Relaxed);
                    *c.last_frame.lock() = Instant::now();
                },
            );
            if let Err(e) = result {
                *c.error.lock() = Some(format!("{e:#}"));
            }
        })
}

/// Spatial layers in one stream: 3 for "L3T3_KEY", 1 without SVC.
fn spatial_layers(l: &Layering) -> u32 {
    match l {
        Layering::Svc(mode) => mode
            .chars()
            .nth(1)
            .and_then(|c| c.to_digit(10))
            .filter(|n| *n >= 1)
            .unwrap_or(1),
        _ => 1,
    }
}

/// Every request is an explicit size: the SFU forwards the smallest layer
/// that covers it. "Top layer" asks for the published size.
fn apply_request(remote: &Remote, size: Option<(u32, u32)>) {
    let dim = match size {
        Some((w, h)) => TrackDimension(w, h),
        None => remote.original,
    };
    remote.publication.update_video_dimensions(dim);
}

pub fn quality_label(q: ConnectionQuality) -> &'static str {
    match q {
        ConnectionQuality::Excellent => "Excellent",
        ConnectionQuality::Good => "Good",
        ConnectionQuality::Poor => "Poor",
        ConnectionQuality::Lost => "Lost",
    }
}

fn events_time() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Average the numeric inputs; keep the most recent flags and strings.
fn average(w: &VecDeque<HealthInput>) -> HealthInput {
    let n = w.len().max(1) as f64;
    let last = w.back().cloned().unwrap_or_default();
    let avg = |f: fn(&HealthInput) -> f64| w.iter().map(f).sum::<f64>() / n;
    // A quality limit counts if it held for most of the window.
    let limited = w
        .iter()
        .filter(|i| i.quality_limit != "None" && !i.quality_limit.is_empty())
        .count();
    HealthInput {
        capture_fps: avg(|i| i.capture_fps),
        encode_fps: avg(|i| i.encode_fps),
        encode_ms_per_frame: avg(|i| i.encode_ms_per_frame),
        send_mbps: avg(|i| i.send_mbps),
        send_target_mbps: avg(|i| i.send_target_mbps),
        retransmit_ratio: avg(|i| i.retransmit_ratio),
        sent_fps: avg(|i| i.sent_fps),
        decode_fps: avg(|i| i.decode_fps),
        loss_ratio: avg(|i| i.loss_ratio),
        quality_limit: if limited * 2 > w.len() {
            last.quality_limit.clone()
        } else {
            "None".into()
        },
        ..last
    }
}

fn parse_publisher_stats(who: &str, payload: &[u8]) -> Option<crate::stats::BrowserPublisher> {
    let v: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let f = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(crate::stats::BrowserPublisher {
        who: who.to_string(),
        user_agent: s("userAgent"),
        codecs: v
            .get("codecs")
            .and_then(|c| c.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        flexfec: v.get("flexfec").and_then(|x| x.as_bool()).unwrap_or(false),
        encoder: s("encoder"),
        width: f("width") as u32,
        height: f("height") as u32,
        fps: f("fps"),
        media_mbps: f("mediaMbps"),
        wire_mbps: f("wireMbps"),
        rtx_mbps: f("rtxMbps"),
        nack: f("nack") as u64,
        pli: f("pli") as u64,
        limit: s("limit"),
    })
}

/// A token for a Chrome publisher started from the app.
pub fn publisher_token(
    api_key: &str,
    api_secret: &str,
    room: &str,
    identity: &str,
) -> Result<String> {
    access_token::AccessToken::with_api_key(api_key, api_secret)
        .with_identity(identity)
        .with_name(identity)
        .with_ttl(Duration::from_secs(6 * 3600))
        .with_grants(access_token::VideoGrants {
            room_join: true,
            room: room.to_string(),
            can_publish: Some(true),
            can_subscribe: Some(false),
            can_publish_data: Some(true),
            ..Default::default()
        })
        .to_jwt()
        .context("creating publisher token")
}

fn parse_viewer_stats(who: &str, payload: &[u8]) -> Option<ReceiveRow> {
    let v: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let f = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(ReceiveRow {
        who: who.to_string(),
        is_browser: true,
        width: f("width") as u32,
        height: f("height") as u32,
        fps: f("fps"),
        mbps: f("mbps"),
        codec: s("codec"),
        decoder: s("decoder"),
        hardware: v.get("powerEfficient").and_then(|x| x.as_bool()),
        decode_ms: f("decodeMs"),
        jitter_buffer_ms: f("jitterBufferMs"),
        freezes: f("freezes") as u32,
        freeze_s: f("freezeSeconds"),
        lost: f("lost") as i64,
        loss_ratio: f("lossRatio"),
        nack: f("nack") as u32,
        pli: f("pli") as u32,
        fec_recovered: f("fecPackets") as u64,
        user_agent: s("userAgent"),
        requested: match (f("requestedWidth") as u32, f("requestedHeight") as u32) {
            (0, _) | (_, 0) => None,
            (w, h) => Some((w, h)),
        },
    })
}

/// Downscale a frame to thumbnail size and store it as RGBA for the UI.
fn write_preview(slot: &SharedPreview, buf: &mut I420Buffer) {
    let (w, h) = (buf.width(), buf.height());
    if w < 2 || h < 2 {
        return;
    }
    let want = slot.lock().want_w;
    let target = if want == 0 {
        PREVIEW_MAX_W
    } else {
        want.max(160)
    };
    let pw = target.min(w) & !1;
    let ph = (((h as u64 * pw as u64) / w as u64) as u32).max(2) & !1;
    let small = buf.scale(pw as i32, ph as i32);
    let (sy, su, sv) = small.strides();
    let (y, u, v) = small.data();
    let mut rgba = vec![0u8; (pw * ph * 4) as usize];
    // libyuv "ABGR" is R,G,B,A in memory, which is what egui wants.
    yuv_helper::i420_to_abgr(y, sy, u, su, v, sv, &mut rgba, pw * 4, pw as i32, ph as i32);
    let notify = {
        let mut p = slot.lock();
        p.w = pw as usize;
        p.h = ph as usize;
        p.rgba = rgba;
        p.seq += 1;
        p.on_update.clone()
    };
    if let Some(f) = notify {
        f();
    }
}

fn clear_preview(slot: &SharedPreview) {
    let mut p = slot.lock();
    p.w = 0;
    p.h = 0;
    p.rgba.clear();
    p.seq += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_stats_parse() {
        let json = br#"{"width":1280,"height":720,"fps":29.5,"mbps":1.6,"codec":"H264","decoder":"ExternalDecoder","powerEfficient":true,"freezes":1,"lost":3}"#;
        let r = parse_viewer_stats("browser-1", json).unwrap();
        assert_eq!((r.width, r.height), (1280, 720));
        assert_eq!(r.hardware, Some(true));
        assert_eq!(r.freezes, 1);
        assert!(parse_viewer_stats("x", b"not json").is_none());
    }

    #[test]
    fn health_average_uses_majority_for_limits() {
        let mut w = VecDeque::new();
        for limit in ["Cpu", "None", "None"] {
            w.push_back(HealthInput {
                quality_limit: limit.into(),
                capture_fps: 30.0,
                ..Default::default()
            });
        }
        let a = average(&w);
        assert_eq!(a.quality_limit, "None");
        assert_eq!(a.capture_fps, 30.0);
        w.push_back(HealthInput {
            quality_limit: "Cpu".into(),
            ..Default::default()
        });
        w.push_back(HealthInput {
            quality_limit: "Cpu".into(),
            ..Default::default()
        });
        assert_eq!(average(&w).quality_limit, "Cpu");
    }

    #[test]
    fn spatial_layer_count() {
        assert_eq!(spatial_layers(&Layering::Svc("L3T3_KEY".into())), 3);
        assert_eq!(spatial_layers(&Layering::Svc("L1T2".into())), 1);
        assert_eq!(spatial_layers(&Layering::Simulcast), 1);
    }

    #[test]
    fn phases() {
        assert!(Phase::Publishing.connected());
        assert!(!Phase::Disconnected.connected());
    }
}

#[cfg(test)]
mod apm_tests {
    use super::*;

    /// Root mean square of 2 s of tone after WebRTC's processing.
    fn processed_rms(aec: bool, agc: bool, ns: bool) -> f64 {
        let mut apm = livekit::webrtc::native::apm::AudioProcessingModule::new(aec, agc, false, ns);
        let (mut sum, mut count) = (0.0, 0.0);
        for n in 0..300u64 {
            let mut d = tone_frame(n, 48_000);
            apm.process_stream(&mut d, 48_000, 1).unwrap();
            // Skip the first second while the modules adapt; only the beeps count.
            if n >= 100 && (n % 100) < 60 {
                for s in d {
                    sum += (s as f64).powi(2);
                    count += 1.0;
                }
            }
        }
        (sum / count).sqrt()
    }

    #[test]
    fn processing_switches_change_the_tone() {
        let raw = 8000.0 / 2f64.sqrt();
        let off = processed_rms(false, false, false);
        let ns = processed_rms(false, false, true);
        let agc = processed_rms(false, true, false);
        println!("rms raw {raw:.0} off {off:.0} ns {ns:.0} agc {agc:.0}");
        assert!(
            (off - raw).abs() / raw < 0.05,
            "all off leaves the tone alone"
        );
        assert!(
            (ns - off).abs() / off > 0.03,
            "noise suppression changes the tone"
        );
        assert!(
            (agc - off).abs() / off > 0.05,
            "gain control changes the tone"
        );
    }
}
