//! The stats model shown in the main window and the Stats & Telemetry window,
//! and the code that turns raw WebRTC stats into it.

use std::collections::{BTreeMap, VecDeque};

use livekit::webrtc::stats::RtcStats;

use crate::health::PipelineHealth;

/// Seconds of history kept for the charts.
pub const HISTORY: usize = 120;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct CaptureStats {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub note: String,
}

/// One sent RTP stream: a simulcast layer, or the single stream (SVC included).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct LayerRow {
    pub rid: String,
    pub active: bool,
    pub target_w: u32,
    pub target_h: u32,
    pub width: u32,
    pub height: u32,
    pub target_fps: f64,
    pub fps: f64,
    /// Maximum allowed bitrate for this layer (the per-layer cap).
    pub max_mbps: f64,
    /// What WebRTC's rate control currently allows.
    pub target_mbps: f64,
    pub sent_mbps: f64,
    pub encoder: String,
    pub hardware: bool,
    pub encode_ms: f64,
    pub qp: f64,
    pub limit: String,
    pub limit_share_bandwidth: f64,
    pub limit_share_cpu: f64,
    pub limit_share_other: f64,
    pub resolution_changes: u32,
    pub keyframes: u32,
    pub pli: u32,
    pub nack: u32,
    pub scalability_mode: String,
    pub codec: String,
}

impl LayerRow {
    pub fn label(&self) -> &'static str {
        match self.rid.as_str() {
            "f" => "full",
            "h" => "half",
            "q" => "quarter",
            _ => "single",
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ReceiveRow {
    pub who: String,
    pub is_browser: bool,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub mbps: f64,
    pub codec: String,
    pub decoder: String,
    pub hardware: Option<bool>,
    pub decode_ms: f64,
    pub jitter_buffer_ms: f64,
    pub freezes: u32,
    pub freeze_s: f64,
    pub lost: i64,
    pub loss_ratio: f64,
    pub nack: u32,
    pub pli: u32,
    pub fec_recovered: u64,
    pub user_agent: String,
    /// Size this receiver asked the SFU for, when it asks.
    pub requested: Option<(u32, u32)>,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct TransportStats {
    pub local_candidate: String,
    pub remote_candidate: String,
    pub rtt_ms: f64,
    pub available_out_mbps: f64,
    pub dtls: String,
    pub srtp: String,
    pub pair_changes: u32,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct History {
    pub cap: VecDeque<f32>,
    pub target: VecDeque<f32>,
    pub sent: VecDeque<f32>,
    pub received: VecDeque<f32>,
    pub capture_h: VecDeque<f32>,
    pub encoded_h: VecDeque<f32>,
    pub decoded_h: VecDeque<f32>,
    pub capture_fps: VecDeque<f32>,
    pub encode_fps: VecDeque<f32>,
    pub sent_fps: VecDeque<f32>,
    pub decode_fps: VecDeque<f32>,
    pub layer_sent: BTreeMap<String, VecDeque<f32>>,
}

pub fn push(h: &mut VecDeque<f32>, v: f64) {
    h.push_back(v as f32);
    while h.len() > HISTORY {
        h.pop_front();
    }
}

/// Everything the UI shows, refreshed once a second.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Snapshot {
    pub capture: CaptureStats,
    pub layers: Vec<LayerRow>,
    pub total_sent_mbps: f64,
    pub total_max_mbps: f64,
    pub total_target_mbps: f64,
    pub retransmit_mbps: f64,
    pub send_codec: String,
    pub receive: Vec<ReceiveRow>,
    pub transport: TransportStats,
    pub health: PipelineHealth,
    /// Tile size last sent with "Ask optimal resolution".
    pub requested_size: Option<(u32, u32)>,
    pub requested_ago_s: Option<f64>,
    /// The server's rating of the publisher's and the subscriber's connection.
    pub server_quality_pub: Option<String>,
    pub server_quality_sub: Option<String>,
    /// A Chrome publisher started from the app, as its page reports itself.
    pub browser_pub: Option<BrowserPublisher>,
    pub fec_negotiated: bool,
    /// Which FEC is in use on the publisher connection: "FlexFEC", "ULPFEC", "RED".
    pub fec: Vec<String>,
    pub audio: AudioStats,
    /// Increments on every sample, so viewers can tell a new one arrived.
    pub seq: u64,
    pub history: History,
    pub events: VecDeque<(String, String)>,
}

impl Snapshot {
    pub fn top_layer(&self) -> Option<&LayerRow> {
        self.layers
            .iter()
            .filter(|l| l.active)
            .max_by_key(|l| l.width * l.height)
    }
    pub fn self_receive(&self) -> Option<&ReceiveRow> {
        self.receive.iter().find(|r| !r.is_browser)
    }
    pub fn event(&mut self, text: impl Into<String>) {
        let ts = chrono_like_now();
        self.events.push_front((ts, text.into()));
        while self.events.len() > 200 {
            self.events.pop_back();
        }
    }
}

/// HH:MM:SS of local wall time without pulling in a date crate.
fn chrono_like_now() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

// ---------------------------------------------------------------------------
// Raw counters, kept between samples to compute rates
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RawOut {
    pub rid: String,
    pub active: bool,
    pub bytes: u64,
    pub retrans_bytes: u64,
    pub frames_encoded: u32,
    pub frames_sent: u32,
    pub encode_time: f64,
    pub qp_sum: u64,
    pub width: u32,
    pub height: u32,
    pub target_bps: f64,
    pub encoder: String,
    pub hardware: bool,
    pub limit: String,
    pub limit_durations: (f64, f64, f64),
    pub resolution_changes: u32,
    pub keyframes: u32,
    pub pli: u32,
    pub nack: u32,
    pub scalability_mode: String,
    pub codec: String,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RawIn {
    pub bytes: u64,
    pub packets: u64,
    pub lost: i64,
    pub frames_decoded: u32,
    pub decode_time: f64,
    pub width: u32,
    pub height: u32,
    pub decoder: String,
    pub hardware: bool,
    pub codec: String,
    pub jitter_delay: f64,
    pub jitter_emitted: u64,
    pub freezes: u32,
    pub freeze_s: f64,
    pub nack: u32,
    pub pli: u32,
    pub fec: u64,
}

/// "H265" or "H264 (profile-level-id=42e01f)" for a stream's codec id.
pub fn codec_label(stats: &[RtcStats], codec_id: &str) -> String {
    for s in stats {
        let RtcStats::Codec(c) = s else { continue };
        if c.rtc.id != codec_id {
            continue;
        }
        let name = c
            .codec
            .mime_type
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        let profile = c
            .codec
            .sdp_fmtp_line
            .split(';')
            .map(str::trim)
            .find(|kv| {
                kv.starts_with("profile-id=")
                    || kv.starts_with("profile-level-id=")
                    || kv.starts_with("profile=")
            })
            .map(|kv| kv.to_string());
        return match profile {
            Some(p) => format!("{name} ({p})"),
            None => name,
        };
    }
    String::new()
}

/// Forward error correction in use on this connection, from its codec stats:
/// "FlexFEC", "ULPFEC", "RED".
pub fn fec_codecs(stats: &[RtcStats]) -> Vec<String> {
    let mut out: Vec<String> = stats
        .iter()
        .filter_map(|s| match s {
            RtcStats::Codec(c) => {
                let m = c.codec.mime_type.to_ascii_lowercase();
                if m.contains("flexfec") {
                    Some("FlexFEC".to_string())
                } else if m.contains("ulpfec") {
                    Some("ULPFEC".to_string())
                } else if m.ends_with("/red") && m.starts_with("video") {
                    Some("RED".to_string())
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn outbound(stats: &[RtcStats]) -> Vec<RawOut> {
    let mut out = Vec::new();
    for s in stats {
        let RtcStats::OutboundRtp(o) = s else {
            continue;
        };
        if o.stream.kind != "video" {
            continue;
        }
        let d = &o.outbound.quality_limitation_durations;
        out.push(RawOut {
            rid: o.outbound.rid.clone(),
            active: o.outbound.active,
            bytes: o.sent.bytes_sent,
            retrans_bytes: o.outbound.retransmitted_bytes_sent,
            frames_encoded: o.outbound.frames_encoded,
            frames_sent: o.outbound.frames_sent,
            encode_time: o.outbound.total_encode_time,
            qp_sum: o.outbound.qp_sum,
            width: o.outbound.frame_width,
            height: o.outbound.frame_height,
            target_bps: o.outbound.target_bitrate,
            encoder: o.outbound.encoder_implementation.clone(),
            hardware: o.outbound.power_efficient_encoder
                || is_platform_codec(&o.outbound.encoder_implementation),
            limit: format!("{:?}", o.outbound.quality_limitation_reason),
            limit_durations: (
                d.get("bandwidth").copied().unwrap_or(0.0),
                d.get("cpu").copied().unwrap_or(0.0),
                d.get("other").copied().unwrap_or(0.0),
            ),
            resolution_changes: o.outbound.quality_limitation_resolution_changes,
            keyframes: o.outbound.key_frames_encoded,
            pli: o.outbound.pli_count,
            nack: o.outbound.nack_count,
            scalability_mode: o.outbound.scalibility_mode.clone(),
            codec: codec_label(stats, &o.stream.codec_id),
        });
    }
    out.sort_by(|a, b| a.rid.cmp(&b.rid));
    out
}

pub fn inbound(stats: &[RtcStats]) -> Option<RawIn> {
    stats.iter().find_map(|s| {
        let RtcStats::InboundRtp(i) = s else {
            return None;
        };
        if i.stream.kind != "video" {
            return None;
        }
        Some(RawIn {
            bytes: i.inbound.bytes_received,
            packets: i.received.packets_received,
            lost: i.received.packets_lost,
            frames_decoded: i.inbound.frames_decoded,
            decode_time: i.inbound.total_decode_time,
            width: i.inbound.frame_width,
            height: i.inbound.frame_height,
            decoder: i.inbound.decoder_implementation.clone(),
            // Native decoders built on platform APIs do not always set the
            // power-efficient flag; their names say what they are.
            hardware: i.inbound.power_efficient_decoder
                || is_platform_codec(&i.inbound.decoder_implementation),
            codec: codec_label(stats, &i.stream.codec_id),
            jitter_delay: i.inbound.jitter_buffer_delay,
            jitter_emitted: i.inbound.jitter_buffer_emitted_count,
            freezes: i.inbound.freeze_count,
            freeze_s: i.inbound.total_freeze_duration,
            nack: i.inbound.nack_count,
            pli: i.inbound.pli_count,
            fec: i.inbound.fec_packets_received,
        })
    })
}

pub fn transport(stats: &[RtcStats]) -> TransportStats {
    let mut t = TransportStats::default();
    let mut pair_id = String::new();
    for s in stats {
        if let RtcStats::Transport(tr) = s {
            pair_id = tr.transport.selected_candidate_pair_id.clone();
            t.dtls = tr.transport.tls_version.clone();
            t.srtp = tr.transport.srtp_cipher.clone();
            t.pair_changes = tr.transport.selected_candidate_pair_changes;
        }
    }
    let mut local_id = String::new();
    let mut remote_id = String::new();
    for s in stats {
        if let RtcStats::CandidatePair(p) = s {
            if pair_id.is_empty() || p.rtc.id == pair_id {
                t.rtt_ms = p.candidate_pair.current_round_trip_time * 1000.0;
                t.available_out_mbps = p.candidate_pair.available_outgoing_bitrate / 1e6;
                local_id = p.candidate_pair.local_candidate_id.clone();
                remote_id = p.candidate_pair.remote_candidate_id.clone();
                if !pair_id.is_empty() {
                    break;
                }
            }
        }
    }
    for s in stats {
        match s {
            RtcStats::LocalCandidate(c) if c.rtc.id == local_id => {
                t.local_candidate = candidate_label(
                    c.local_candidate
                        .candidate_type
                        .as_ref()
                        .map(|k| format!("{k:?}")),
                    &c.local_candidate.protocol,
                );
            }
            RtcStats::RemoteCandidate(c) if c.rtc.id == remote_id => {
                t.remote_candidate = candidate_label(
                    c.remote_candidate
                        .candidate_type
                        .as_ref()
                        .map(|k| format!("{k:?}")),
                    &c.remote_candidate.protocol,
                );
            }
            _ => {}
        }
    }
    t
}

/// Rates between two raw samples `dt` seconds apart.
/// Decoder or encoder names that are platform (hardware) implementations.
pub fn is_platform_codec(name: &str) -> bool {
    [
        "VideoToolbox",
        "MediaCodec",
        "NVDEC",
        "NVENC",
        // Implementation names of the SDK's Linux encoders and decoders.
        "NVIDIA",
        "Jetson",
        "VAAPI",
        "MediaFoundation",
        "D3D11",
    ]
    .iter()
    .any(|n| name.contains(n))
}

/// "host/udp", "relay/tcp"…: the candidate type and transport, lower case.
fn candidate_label(kind: Option<String>, protocol: &str) -> String {
    let kind = kind.unwrap_or_else(|| "unknown".into()).to_lowercase();
    if protocol.is_empty() {
        kind
    } else {
        format!("{kind}/{}", protocol.to_lowercase())
    }
}

/// What the Chrome publisher page reports about its own sending.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct BrowserPublisher {
    pub who: String,
    pub user_agent: String,
    pub codecs: Vec<String>,
    pub flexfec: bool,
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub media_mbps: f64,
    pub wire_mbps: f64,
    pub rtx_mbps: f64,
    pub nack: u64,
    pub pli: u64,
    pub limit: String,
}

/// The audio track in both directions, refreshed once a second.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct AudioStats {
    /// "Synthetic tone" or the microphone's name; empty when no audio is published.
    pub source: String,
    pub send_codec: String,
    pub send_kbps: f64,
    /// Input level 0..1 at the sender (WebRTC's audioLevel).
    pub send_level: f64,
    /// Loss and round trip the server reports back for our audio (RTCP).
    pub server_loss: f64,
    pub rtt_ms: f64,
    /// Echo cancellation's effect on the microphone, in dB (0 when unknown).
    pub echo_return_loss_db: f64,
    pub receiving: bool,
    pub recv_codec: String,
    pub recv_kbps: f64,
    pub recv_level: f64,
    pub loss_ratio: f64,
    pub lost: i64,
    pub jitter_ms: f64,
    pub jitter_buffer_ms: f64,
    /// Share of played samples that NetEq invented to hide gaps, last second.
    pub concealed_pct: f64,
    pub concealment_events: u64,
    pub fec_packets: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RawAudioOut {
    pub bytes: u64,
    pub codec: String,
    pub level: f64,
    pub fraction_lost: f64,
    pub rtt_s: f64,
    pub echo_return_loss: f64,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RawAudioIn {
    pub bytes: u64,
    pub packets: u64,
    pub lost: i64,
    pub jitter_s: f64,
    pub jitter_delay: f64,
    pub jitter_emitted: u64,
    pub samples: u64,
    pub concealed: u64,
    pub concealment_events: u64,
    pub level: f64,
    pub fec: u64,
    pub codec: String,
}

pub fn audio_outbound(stats: &[RtcStats]) -> Option<RawAudioOut> {
    let mut out: Option<RawAudioOut> = None;
    for s in stats {
        if let RtcStats::OutboundRtp(o) = s {
            if o.stream.kind == "audio" {
                let r = out.get_or_insert_with(Default::default);
                r.bytes += o.sent.bytes_sent;
                r.codec = codec_label(stats, &o.stream.codec_id);
            }
        }
    }
    let r = out.as_mut()?;
    for s in stats {
        match s {
            RtcStats::RemoteInboundRtp(ri) if ri.stream.kind == "audio" => {
                r.fraction_lost = ri.remote_inbound.fraction_lost;
                r.rtt_s = ri.remote_inbound.round_trip_time;
            }
            RtcStats::MediaSource(m) if m.source.kind == "audio" => {
                r.level = m.audio.audio_level;
                r.echo_return_loss = m.audio.echo_return_loss;
            }
            _ => {}
        }
    }
    out
}

pub fn audio_inbound(stats: &[RtcStats]) -> Option<RawAudioIn> {
    stats.iter().find_map(|s| {
        let RtcStats::InboundRtp(i) = s else {
            return None;
        };
        if i.stream.kind != "audio" {
            return None;
        }
        Some(RawAudioIn {
            bytes: i.inbound.bytes_received,
            packets: i.received.packets_received,
            lost: i.received.packets_lost,
            jitter_s: i.received.jitter,
            jitter_delay: i.inbound.jitter_buffer_delay,
            jitter_emitted: i.inbound.jitter_buffer_emitted_count,
            samples: i.inbound.total_samples_received,
            concealed: i.inbound.concealed_samples,
            concealment_events: i.inbound.concealment_events,
            level: i.inbound.audio_level,
            fec: i.inbound.fec_packets_received,
            codec: codec_label(stats, &i.stream.codec_id),
        })
    })
}

/// Turn two samples of raw audio counters into rates for the UI.
pub fn audio_rates(
    out: Option<(&RawAudioOut, Option<&RawAudioOut>)>,
    inn: Option<(&RawAudioIn, Option<&RawAudioIn>)>,
    dt: f64,
) -> AudioStats {
    let mut a = AudioStats::default();
    if let Some((o, prev)) = out {
        a.send_codec = o.codec.clone();
        a.send_kbps = prev
            .map(|p| rate_mbps(o.bytes, p.bytes, dt) * 1000.0)
            .unwrap_or(0.0);
        a.send_level = o.level;
        a.server_loss = o.fraction_lost;
        a.rtt_ms = o.rtt_s * 1000.0;
        a.echo_return_loss_db = o.echo_return_loss;
    }
    if let Some((i, prev)) = inn {
        a.receiving = true;
        a.recv_codec = i.codec.clone();
        a.recv_level = i.level;
        a.lost = i.lost;
        a.jitter_ms = i.jitter_s * 1000.0;
        a.concealment_events = i.concealment_events;
        a.fec_packets = i.fec;
        if let Some(p) = prev {
            a.recv_kbps = rate_mbps(i.bytes, p.bytes, dt) * 1000.0;
            let packets = i.packets.saturating_sub(p.packets) as f64;
            let lost = (i.lost - p.lost).max(0) as f64;
            if packets + lost > 0.0 {
                a.loss_ratio = lost / (packets + lost);
            }
            let emitted = i.jitter_emitted.saturating_sub(p.jitter_emitted);
            if emitted > 0 {
                a.jitter_buffer_ms = (i.jitter_delay - p.jitter_delay) * 1000.0 / emitted as f64;
            }
            let samples = i.samples.saturating_sub(p.samples);
            if samples > 0 {
                a.concealed_pct =
                    i.concealed.saturating_sub(p.concealed) as f64 * 100.0 / samples as f64;
            }
        }
    }
    a
}

pub fn rate_mbps(now: u64, before: u64, dt: f64) -> f64 {
    now.saturating_sub(before) as f64 * 8.0 / dt.max(1e-3) / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_rates_from_two_samples() {
        let before = RawAudioIn {
            bytes: 0,
            packets: 0,
            samples: 0,
            ..Default::default()
        };
        let now = RawAudioIn {
            bytes: 4000,
            packets: 48,
            lost: 2,
            jitter_delay: 2880.0,
            jitter_emitted: 48_000,
            samples: 48_000,
            concealed: 960,
            ..Default::default()
        };
        let a = audio_rates(None, Some((&now, Some(&before))), 1.0);
        assert!((a.recv_kbps - 32.0).abs() < 1e-9);
        assert!((a.loss_ratio - 0.04).abs() < 1e-9);
        assert!((a.jitter_buffer_ms - 60.0).abs() < 1e-9);
        assert!((a.concealed_pct - 2.0).abs() < 1e-9);
        assert!(a.receiving);
        // The first sample has no rates yet.
        let first = audio_rates(None, Some((&now, None)), 1.0);
        assert_eq!(first.recv_kbps, 0.0);
    }

    #[test]
    fn platform_codec_names() {
        assert!(is_platform_codec("NVIDIA H264 Decoder"));
        assert!(is_platform_codec("VAAPI H264 Encoder"));
        assert!(is_platform_codec(
            "SimulcastEncoderAdapter (VideoToolbox, VideoToolbox, VideoToolbox)"
        ));
        assert!(!is_platform_codec("libvpx"));
        assert!(!is_platform_codec("OpenH264"));
    }

    #[test]
    fn candidate_labels() {
        assert_eq!(candidate_label(Some("Host".into()), "UDP"), "host/udp");
        assert_eq!(candidate_label(None, "tcp"), "unknown/tcp");
    }

    #[test]
    fn rates() {
        assert!((rate_mbps(1_250_000, 0, 1.0) - 10.0).abs() < 1e-9);
        assert_eq!(
            rate_mbps(10, 30, 1.0),
            0.0,
            "counter resets never go negative"
        );
    }

    #[test]
    fn history_is_bounded() {
        let mut h = VecDeque::new();
        for i in 0..(HISTORY + 10) {
            push(&mut h, i as f64);
        }
        assert_eq!(h.len(), HISTORY);
        assert_eq!(h[0], 10.0);
    }
}
