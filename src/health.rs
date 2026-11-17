//! Health rules for the five pipeline cards.
//!
//! Each stage gets green, amber or red from simple thresholds. The thresholds are
//! the ones proposed in the design review and are documented in the help guide.
//! Rules look at a short window (the stats sampler passes 5-second averages) so a
//! single bad sample does not flash red.

/// Health of one pipeline stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, serde::Serialize)]
pub enum Health {
    /// No data yet (not publishing, or the first samples have not arrived).
    #[default]
    Unknown,
    Good,
    Degraded,
    Failing,
}

impl Health {
    /// The worse of two states. Unknown never hides a real state.
    pub fn worst(self, other: Health) -> Health {
        match (self, other) {
            (Health::Unknown, x) | (x, Health::Unknown) => x,
            (a, b) => a.max(b),
        }
    }
}

/// A health state plus the reason shown under the card.
#[derive(Clone, Debug, PartialEq, Default, serde::Serialize)]
pub struct Verdict {
    pub health: Health,
    pub reason: String,
}

impl Verdict {
    fn new(health: Health, reason: impl Into<String>) -> Self {
        Self {
            health,
            reason: reason.into(),
        }
    }

    fn merge(self, other: Verdict) -> Verdict {
        if other.health.worst(self.health) == other.health && other.health != self.health {
            other
        } else {
            self
        }
    }
}

/// Inputs to the rules, averaged over the health window.
#[derive(Clone, Debug, Default)]
pub struct HealthInput {
    pub publishing: bool,
    pub subscribed: bool,

    pub target_fps: f64,
    pub capture_fps: f64,
    /// Screen capture: macOS sends a new frame only when the screen changes,
    /// so a rate under target is normal.
    pub screen: bool,
    pub seconds_since_capture_frame: f64,

    pub encode_fps: f64,
    pub encode_ms_per_frame: f64,
    /// Hardware encoders are pipelined: their encode time is latency, not load.
    pub encoder_hardware: bool,
    /// Top layer: what the encoder produces against what it was asked for.
    pub encoded_height: u32,
    pub target_height: u32,
    /// "None", "Cpu", "Bandwidth" or "Other" as WebRTC reports it.
    pub quality_limit: String,
    /// The user asked for hardware but a software encoder is in use, or the reverse.
    pub encoder_fallback: bool,

    pub send_mbps: f64,
    pub send_target_mbps: f64,
    pub bitrate_cap_mbps: f64,
    pub retransmit_ratio: f64,
    pub seconds_since_send: f64,

    /// Height of the layer the subscriber asked for, if it asked; 0 = no request.
    pub requested_height: u32,
    pub received_height: u32,
    /// The server's rating of the subscriber connection: "Poor", "Lost"…
    pub server_quality_sub: String,

    pub sent_fps: f64,
    pub decode_fps: f64,
    pub loss_ratio: f64,
    pub freezes_in_window: u32,
    pub longest_freeze_s: f64,
}

/// Health of all five stages.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct PipelineHealth {
    pub capture: Verdict,
    pub encode: Verdict,
    pub send: Verdict,
    pub sfu: Verdict,
    pub receive: Verdict,
}

/// Bitrate still below this share of the cap counts as "ramping" (informational, not a fault).
const RAMP_SHARE: f64 = 0.9;

pub fn evaluate(i: &HealthInput) -> PipelineHealth {
    if !i.publishing {
        return PipelineHealth::default();
    }
    PipelineHealth {
        capture: capture(i),
        encode: encode(i),
        send: send(i),
        sfu: sfu(i),
        receive: receive(i),
    }
}

fn ratio(a: f64, b: f64) -> f64 {
    if b <= 0.0 {
        1.0
    } else {
        a / b
    }
}

fn capture(i: &HealthInput) -> Verdict {
    if i.seconds_since_capture_frame >= 2.0 {
        return Verdict::new(Health::Failing, "No frames for 2 s");
    }
    let r = ratio(i.capture_fps, i.target_fps);
    if i.screen {
        return Verdict::new(
            Health::Good,
            format!(
                "{:.0}% of target frame rate; a screen sends frames only when it changes",
                r * 100.0
            ),
        );
    }
    if r >= 0.95 {
        Verdict::new(Health::Good, "At target frame rate")
    } else if r >= 0.70 {
        Verdict::new(
            Health::Degraded,
            format!("{:.0}% of target frame rate", r * 100.0),
        )
    } else {
        Verdict::new(
            Health::Failing,
            format!("{:.0}% of target frame rate", r * 100.0),
        )
    }
}

fn encode(i: &HealthInput) -> Verdict {
    let mut v = Verdict::new(Health::Good, "Keeping up");
    let r = ratio(i.encode_fps, i.capture_fps);
    if r < 0.5 {
        v = v.merge(Verdict::new(
            Health::Failing,
            format!("Encoding {:.0}% of captured frames", r * 100.0),
        ));
    } else if r < 0.9 {
        v = v.merge(Verdict::new(
            Health::Degraded,
            format!("Encoding {:.0}% of captured frames", r * 100.0),
        ));
    }

    // WebRTC reports a limitation reason while it holds the output below
    // target. It only matters to the viewer when something was actually cut.
    let lowered_size =
        i.target_height > 0 && i.encoded_height > 0 && i.encoded_height < i.target_height;
    let lowered_rate = i.target_fps > 0.0 && i.encode_fps < i.target_fps * 0.9;
    let why = match i.quality_limit.as_str() {
        "Cpu" if i.encoder_hardware => Some(
            "CPU (WebRTC's overuse detector, which for hardware encoders reacts to encode latency)",
        ),
        "Cpu" => Some("CPU"),
        "Bandwidth" => Some("bandwidth"),
        "Other" => Some("another reason"),
        _ => None,
    };
    if let Some(why) = why {
        if lowered_size {
            v = v.merge(Verdict::new(
                Health::Degraded,
                format!(
                    "WebRTC lowered the resolution to {}p (target {}p): limited by {why}",
                    i.encoded_height, i.target_height
                ),
            ));
        } else if lowered_rate {
            v = v.merge(Verdict::new(
                Health::Degraded,
                format!(
                    "WebRTC lowered the frame rate to {:.0} fps: limited by {why}",
                    i.encode_fps
                ),
            ));
        } else if v.health == Health::Good {
            v.reason =
                format!("Keeping up; WebRTC reports a {why} limit but the output is at target");
        }
    }

    // A software encoder works on one frame at a time, so its encode time is
    // its load. Hardware encoders pipeline frames: the same number is latency.
    if !i.encoder_hardware && i.target_fps > 0.0 {
        let budget_ms = 1000.0 / i.target_fps;
        if i.encode_ms_per_frame > budget_ms * 0.8 {
            v = v.merge(Verdict::new(
                Health::Degraded,
                format!(
                    "{:.0} ms per frame, budget {:.0} ms",
                    i.encode_ms_per_frame, budget_ms
                ),
            ));
        }
    }
    if i.encoder_fallback {
        v = v.merge(Verdict::new(
            Health::Failing,
            "Requested encoder not in use",
        ));
    }
    v
}

fn send(i: &HealthInput) -> Verdict {
    if i.seconds_since_send >= 2.0 {
        return Verdict::new(Health::Failing, "Nothing sent for 2 s");
    }
    let mut v = Verdict::new(Health::Good, "Sending at target");
    if i.retransmit_ratio > 0.15 {
        v = v.merge(Verdict::new(
            Health::Failing,
            format!("{:.0}% retransmitted", i.retransmit_ratio * 100.0),
        ));
    } else if i.retransmit_ratio > 0.05 {
        v = v.merge(Verdict::new(
            Health::Degraded,
            format!("{:.0}% retransmitted", i.retransmit_ratio * 100.0),
        ));
    }
    if v.health == Health::Good
        && i.bitrate_cap_mbps > 0.0
        && i.send_target_mbps > 0.0
        && i.send_target_mbps < i.bitrate_cap_mbps * RAMP_SHARE
    {
        // Ramping is expected WebRTC behaviour, so it stays green with an explanation.
        v.reason = format!(
            "Ramping: allowed {:.1} of {:.1} Mbps",
            i.send_target_mbps, i.bitrate_cap_mbps
        );
    }
    v
}

fn sfu(i: &HealthInput) -> Verdict {
    if !i.subscribed {
        return Verdict::new(Health::Unknown, "No subscriber");
    }
    // The SFU chooses from a ladder of layers (full, half, quarter), so it may
    // round to the nearest; only a clearly smaller layer counts.
    if i.requested_height > 0
        && i.received_height > 0
        && (i.received_height as f64) < i.requested_height as f64 * 0.75
    {
        return Verdict::new(
            Health::Degraded,
            format!(
                "Sending {}p, asked for {}p",
                i.received_height, i.requested_height
            ),
        );
    }
    match i.server_quality_sub.as_str() {
        "Lost" => {
            return Verdict::new(
                Health::Failing,
                "Server rates the subscriber connection lost",
            )
        }
        "Poor" => {
            return Verdict::new(
                Health::Degraded,
                "Server rates the subscriber connection poor",
            )
        }
        _ => {}
    }
    Verdict::new(Health::Good, "Forwarding")
}

fn receive(i: &HealthInput) -> Verdict {
    if !i.subscribed {
        return Verdict::new(Health::Unknown, "No subscriber");
    }
    let r = ratio(i.decode_fps, i.sent_fps);
    let mut v = Verdict::new(Health::Good, "Smooth");
    if i.loss_ratio > 0.05 {
        v = v.merge(Verdict::new(
            Health::Failing,
            format!("{:.1}% loss", i.loss_ratio * 100.0),
        ));
    } else if i.loss_ratio >= 0.01 {
        v = v.merge(Verdict::new(
            Health::Degraded,
            format!("{:.1}% loss", i.loss_ratio * 100.0),
        ));
    }
    if r < 0.70 {
        v = v.merge(Verdict::new(
            Health::Failing,
            format!("Decoding {:.0}% of sent frames", r * 100.0),
        ));
    } else if r < 0.95 {
        v = v.merge(Verdict::new(
            Health::Degraded,
            format!("Decoding {:.0}% of sent frames", r * 100.0),
        ));
    }
    if i.longest_freeze_s > 1.0 {
        v = v.merge(Verdict::new(
            Health::Failing,
            format!("Freeze of {:.1} s", i.longest_freeze_s),
        ));
    } else if i.freezes_in_window > 0 {
        v = v.merge(Verdict::new(
            Health::Degraded,
            format!("{} freeze(s)", i.freezes_in_window),
        ));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady(hw: bool) -> HealthInput {
        HealthInput {
            publishing: true,
            target_fps: 30.0,
            capture_fps: 30.0,
            encode_fps: 30.0,
            encoder_hardware: hw,
            encoded_height: 1080,
            target_height: 1080,
            quality_limit: "None".into(),
            ..Default::default()
        }
    }

    #[test]
    fn hardware_latency_is_not_load() {
        let mut i = steady(true);
        i.encode_ms_per_frame = 100.0;
        assert_eq!(encode(&i).health, Health::Good);
        let mut s = steady(false);
        s.encode_ms_per_frame = 100.0;
        assert_eq!(encode(&s).health, Health::Degraded);
    }

    #[test]
    fn limit_counts_only_when_output_is_cut() {
        let mut i = steady(true);
        i.quality_limit = "Cpu".into();
        let v = encode(&i);
        assert_eq!(v.health, Health::Good);
        assert!(v.reason.contains("at target"));
        i.encoded_height = 720;
        let v = encode(&i);
        assert_eq!(v.health, Health::Degraded);
        assert!(v.reason.contains("720p"));
    }

    fn healthy() -> HealthInput {
        HealthInput {
            publishing: true,
            subscribed: true,
            target_fps: 30.0,
            capture_fps: 30.0,
            encode_fps: 30.0,
            encode_ms_per_frame: 10.0,
            quality_limit: "None".into(),
            send_mbps: 8.0,
            send_target_mbps: 8.0,
            bitrate_cap_mbps: 8.0,
            sent_fps: 30.0,
            decode_fps: 30.0,
            ..Default::default()
        }
    }

    #[test]
    fn all_green_when_healthy() {
        let h = evaluate(&healthy());
        for v in [&h.capture, &h.encode, &h.send, &h.sfu, &h.receive] {
            assert_eq!(v.health, Health::Good, "{v:?}");
        }
    }

    #[test]
    fn nothing_when_not_publishing() {
        let i = HealthInput {
            publishing: false,
            ..healthy()
        };
        assert_eq!(evaluate(&i), PipelineHealth::default());
    }

    #[test]
    fn capture_thresholds() {
        let mut i = healthy();
        i.capture_fps = 25.0;
        assert_eq!(evaluate(&i).capture.health, Health::Degraded);
        i.capture_fps = 15.0;
        assert_eq!(evaluate(&i).capture.health, Health::Failing);
        i.capture_fps = 30.0;
        i.seconds_since_capture_frame = 3.0;
        assert_eq!(evaluate(&i).capture.health, Health::Failing);
    }

    #[test]
    fn encode_cpu_limit_is_amber_and_fallback_is_red() {
        let mut i = healthy();
        i.quality_limit = "Cpu".into();
        i.target_height = 1080;
        i.encoded_height = 540;
        let v = evaluate(&i).encode;
        assert_eq!(v.health, Health::Degraded);
        assert!(v.reason.contains("limited by CPU"), "{}", v.reason);
        i.encoder_fallback = true;
        assert_eq!(evaluate(&i).encode.health, Health::Failing);
    }

    #[test]
    fn encode_time_over_budget() {
        let mut i = healthy();
        i.encode_ms_per_frame = 30.0; // budget 33 ms, 80% = 26.7 ms
        assert_eq!(evaluate(&i).encode.health, Health::Degraded);
    }

    #[test]
    fn ramping_stays_green_with_reason() {
        let mut i = healthy();
        i.bitrate_cap_mbps = 20.0;
        i.send_target_mbps = 6.0;
        let v = evaluate(&i).send;
        assert_eq!(v.health, Health::Good);
        assert!(v.reason.starts_with("Ramping"), "{}", v.reason);
    }

    #[test]
    fn retransmission_thresholds() {
        let mut i = healthy();
        i.retransmit_ratio = 0.08;
        assert_eq!(evaluate(&i).send.health, Health::Degraded);
        i.retransmit_ratio = 0.2;
        assert_eq!(evaluate(&i).send.health, Health::Failing);
    }

    #[test]
    fn sfu_lower_layer_than_requested() {
        let mut i = healthy();
        i.requested_height = 1080;
        i.received_height = 540;
        assert_eq!(evaluate(&i).sfu.health, Health::Degraded);
        i.received_height = 900;
        assert_eq!(
            evaluate(&i).sfu.health,
            Health::Good,
            "near the ask is fine"
        );
    }

    #[test]
    fn sfu_server_rating() {
        let mut i = healthy();
        i.server_quality_sub = "Poor".into();
        assert_eq!(evaluate(&i).sfu.health, Health::Degraded);
        i.server_quality_sub = "Lost".into();
        assert_eq!(evaluate(&i).sfu.health, Health::Failing);
        i.server_quality_sub = "Excellent".into();
        assert_eq!(evaluate(&i).sfu.health, Health::Good);
    }

    #[test]
    fn receive_loss_freeze_and_rate() {
        let mut i = healthy();
        i.loss_ratio = 0.02;
        assert_eq!(evaluate(&i).receive.health, Health::Degraded);
        i.loss_ratio = 0.0;
        i.longest_freeze_s = 1.5;
        assert_eq!(evaluate(&i).receive.health, Health::Failing);
        i.longest_freeze_s = 0.0;
        i.decode_fps = 20.0;
        assert_eq!(evaluate(&i).receive.health, Health::Failing);
    }

    #[test]
    fn no_subscriber_is_unknown_not_red() {
        let mut i = healthy();
        i.subscribed = false;
        let h = evaluate(&i);
        assert_eq!(h.sfu.health, Health::Unknown);
        assert_eq!(h.receive.health, Health::Unknown);
    }

    #[test]
    fn worst_ignores_unknown() {
        assert_eq!(Health::Unknown.worst(Health::Good), Health::Good);
        assert_eq!(Health::Degraded.worst(Health::Failing), Health::Failing);
        assert_eq!(Health::Good.worst(Health::Degraded), Health::Degraded);
    }
}
