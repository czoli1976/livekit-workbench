//! Scripted runs without a window, for testing the engine end to end.
//!
//!   livekit-workbench --headless [options]
//!     --codec h264|h265|av1|vp9|vp8   (default h264)
//!     --size WxH                       (default 1280x720)
//!     --fps N                          (default 30)
//!     --mbps N                         (default 3)
//!     --encoder auto|hw|sw             (default auto)
//!     --layers single|simulcast|L3T3_KEY…  (default single)
//!     --source detail|pattern          (default detail)
//!     --seconds N                      (default 10)
//!     --ask WxH                        ask the SFU for a layer that fits a WxH tile
//!     --e2ee PASSPHRASE                end-to-end encryption with a shared key
//!                                      (or LIVEKIT_E2EE_KEY, kept out of process listings)
//!     --detail-hint                    content hint Detail / text (screencast flag)
//!     --audio tone|MICROPHONE-NAME     also publish audio, and require it to arrive
//!     --audio-kbps N                   Opus bitrate cap (default 32)
//!     --no-dtx, --no-red               turn off Opus DTX or redundant audio
//!     --no-aec, --no-ns, --no-agc      turn off WebRTC's audio processing
//!     --up LOSS%,DELAY,JITTER,KBPS     impair the publisher's uplink (in-app relay)
//!     --down LOSS%,DELAY,JITTER,KBPS   impair the subscriber's downlink
//!     --lifecycle                      also exercise unpublish, publish, republish, switch source
//!
//! Connects with LIVEKIT_URL, LIVEKIT_API_KEY and LIVEKIT_API_SECRET, or the
//! local dev server defaults. Exits 0 when video made it through and back.

use std::sync::Arc;
use std::time::{Duration, Instant};

use livekit::options::{DegradationPreference, VideoCodec};

use crate::capabilities::EncoderChoice;
use crate::capture::SourceKind;
use crate::session::{
    AudioConfig, AudioInput, ConnectConfig, Layering, Phase, PublishConfig, Session,
};

/// "detail", "pattern", "screen" or "camera" (the first camera).
fn parse_source(s: &str) -> Result<SourceKind, String> {
    match s {
        "pattern" => Ok(SourceKind::Pattern),
        "detail" => Ok(SourceKind::Detail),
        "screen" => Ok(SourceKind::Screen),
        // An explicit camera request must test a camera, never fall back.
        "camera" => crate::capture::list_cameras()
            .map_err(|e| format!("--source camera: {e:#}"))?
            .first()
            .map(|c| SourceKind::Camera(c.index))
            .ok_or_else(|| "--source camera: no camera found".to_string()),
        other => Err(format!(
            "unknown source \"{other}\": detail, pattern, camera or screen"
        )),
    }
}

/// "LOSS%,DELAY,JITTER,KBPS", trailing fields optional: "2" or "1,40,10,1500".
/// A field that is given must be a valid number.
fn parse_impairment(s: &str) -> Result<crate::impair::Impairment, String> {
    let names = ["loss %", "delay ms", "jitter ms", "rate kbps"];
    let mut v = [0.0f64; 4];
    let fields: Vec<&str> = s.split(',').map(str::trim).collect();
    if fields.len() > 4 {
        return Err(format!(
            "\"{s}\": at most 4 fields, LOSS%,DELAY,JITTER,KBPS"
        ));
    }
    for (i, f) in fields.iter().enumerate() {
        v[i] = f
            .parse::<f64>()
            .ok()
            .filter(|x| x.is_finite() && *x >= 0.0)
            .ok_or_else(|| format!("\"{s}\": {} must be a number ≥ 0, got \"{f}\"", names[i]))?;
    }
    if v[0] > 100.0 {
        return Err(format!("\"{s}\": loss must be at most 100 %"));
    }
    Ok(crate::impair::Impairment {
        loss_pct: v[0] as f32,
        delay_ms: v[1] as u32,
        jitter_ms: v[2] as u32,
        rate_kbps: v[3] as u32,
    })
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn wait_phase(s: &Session, want: Phase, limit: Duration) -> Result<Duration, String> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if s.phase() == want {
            return Ok(start.elapsed());
        }
        // Take the error out first: the guard must be gone before phase() locks again.
        let error = s.shared.lock().error.clone();
        if let Some(e) = error {
            if s.phase() == Phase::Disconnected {
                return Err(e);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(format!(
        "timed out waiting for {want:?} (now {:?})",
        s.phase()
    ))
}

fn line(s: &Session, sec: u64) -> (u32, f64) {
    let snap = s.shared.lock().snapshot.clone();
    let top = snap.top_layer().cloned().unwrap_or_default();
    let me = snap.self_receive().cloned().unwrap_or_default();
    let h = &snap.health;
    println!(
        "{sec:>3}s cap {}x{}@{:.0} | enc {} [{}] {}x{}@{:.1} {:.1}ms qp {:.0} limit={} | send {:.2}/{:.2} Mbps (cap {:.2}) | recv {} {}x{}@{:.1} {:.2} Mbps | health {:?}/{:?}/{:?}/{:?}/{:?}",
        snap.capture.width, snap.capture.height, snap.capture.fps,
        top.codec, top.encoder, top.width, top.height, top.fps, top.encode_ms, top.qp, top.limit,
        snap.total_sent_mbps, snap.total_target_mbps, snap.total_max_mbps,
        me.decoder, me.width, me.height, me.fps, me.mbps,
        h.capture.health, h.encode.health, h.send.health, h.sfu.health, h.receive.health,
    );
    if std::env::var("WB_REASONS").is_ok() {
        for (n, v) in [
            ("capture", &h.capture),
            ("encode", &h.encode),
            ("send", &h.send),
            ("sfu", &h.sfu),
            ("receive", &h.receive),
        ] {
            if !v.reason.is_empty() {
                println!("      {n}: {:?} {}", v.health, v.reason);
            }
        }
    }
    if let Some(i) = &snap.impair {
        for (name, l) in [("up", &i.up), ("down", &i.down)] {
            println!(
                "      relay {name}: {:.0} kbps out, dropped {:.1}% random + {:.1}% queue, queue {:.0} ms",
                l.kbps, l.dropped_random_pct, l.dropped_queue_pct, l.queue_ms
            );
        }
    }
    let a = &snap.audio;
    if !a.source.is_empty() || a.receiving {
        println!(
            "      audio send {} {:.1} kbps level {:.2} rtt {:.0} ms | recv {} {:.1} kbps loss {:.1}% jitter {:.1} ms buffer {:.0} ms concealed {:.1}%",
            a.send_codec, a.send_kbps, a.send_level, a.rtt_ms,
            a.recv_codec, a.recv_kbps, a.loss_ratio * 100.0, a.jitter_ms, a.jitter_buffer_ms, a.concealed_pct,
        );
    }
    if std::env::var("WB_QUALITY").is_ok() {
        println!(
            "      server quality: pub {:?} sub {:?}",
            snap.server_quality_pub, snap.server_quality_sub
        );
    }
    if std::env::var("WB_LAYERS").is_ok() {
        for l in &snap.layers {
            println!(
                "      layer rid={:?} active={} {}x{}@{:.1} target {}x{} sent {:.2} Mbps enc={} sm={:?}",
                l.rid, l.active, l.width, l.height, l.fps, l.target_w, l.target_h, l.sent_mbps, l.encoder, l.scalability_mode
            );
        }
    }
    for r in snap.receive.iter().filter(|r| r.is_browser) {
        println!(
            "      browser {} ({}): {}x{}@{:.1} {:.2} Mbps, asks for {:?}",
            r.who, r.user_agent, r.width, r.height, r.fps, r.mbps, r.requested
        );
    }
    (me.height, me.fps)
}

fn chrome_pub(
    url: &str,
    key: &str,
    secret: &str,
    room: &str,
    publish: &PublishConfig,
    seconds: u64,
    rt: &tokio::runtime::Handle,
) -> i32 {
    let session = Session::start(rt, Arc::new(|| {}));
    session.connect(
        ConnectConfig {
            url: url.into(),
            api_key: key.into(),
            api_secret: secret.into(),
            room: room.into(),
            decode: true,
            preview: false,
            e2ee_key: None,
            impair: false,
        },
        None,
    );
    if let Err(e) = wait_phase(&session, Phase::Connected, Duration::from_secs(20)) {
        eprintln!("FAILED to connect: {e}");
        return 2;
    }
    let viewer = crate::viewer::ViewerServer::start().expect("viewer server");
    let token = crate::session::publisher_token(key, secret, room, "chrome-pub-1").expect("token");
    let (w, h) = publish.publish_dims();
    let codec = format!("{:?}", publish.codec).to_lowercase();
    let layers = match &publish.layering {
        Layering::Single => "single".to_string(),
        Layering::Simulcast => "simulcast".to_string(),
        Layering::Svc(m) => m.clone(),
    };
    let link = viewer.publisher_link(
        url,
        &token,
        &crate::viewer::PublisherParams {
            codec: &codec,
            width: w,
            height: h,
            fps: publish.fps,
            mbps: publish.bitrate_bps as f64 / 1e6,
            layers: &layers,
        },
    );
    if let Err(e) = crate::viewer::launch_chrome_with_flexfec(&link) {
        eprintln!("FAILED to start Chrome: {e}");
        return 2;
    }
    let prom_port = url
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .map(|p| p + 1000);
    let mut flexfec = false;
    for sec in 1..=seconds {
        std::thread::sleep(Duration::from_secs(1));
        let snap = session.shared.lock().snapshot.clone();
        let me = snap.self_receive().cloned().unwrap_or_default();
        if let Some(b) = &snap.browser_pub {
            flexfec |= b.flexfec;
            println!(
                "{sec:>3}s chrome: {:?} flexfec={} {}x{}@{:.0} media {:.2} wire {:.2} rtx {:.2} nack {} | app recv {}x{}@{:.1} {}",
                b.codecs, b.flexfec, b.width, b.height, b.fps, b.media_mbps, b.wire_mbps, b.rtx_mbps, b.nack, me.width, me.height, me.fps, me.decoder
            );
        } else {
            println!("{sec:>3}s waiting for the Chrome publisher…");
        }
    }
    if let Some(port) = prom_port {
        if let Ok(text) = reqwest::blocking::get(format!("http://127.0.0.1:{port}/metrics"))
            .and_then(|r| r.text())
        {
            let m = crate::local_server::parse_prometheus(&text);
            println!(
                "server fec: received {:?} recovered {:?} discarded {:?}",
                m.get("livekit_fec_total#received"),
                m.get("livekit_fec_total#recovered"),
                m.get("livekit_fec_total#discarded")
            );
        }
    }
    session.disconnect();
    std::thread::sleep(Duration::from_millis(500));
    println!(
        "RESULT: {}",
        if flexfec {
            "flexfec negotiated"
        } else {
            "flexfec NOT negotiated"
        }
    );
    0
}

pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help") {
        println!(
            "{}",
            include_str!("headless.rs")
                .lines()
                .take_while(|l| l.starts_with("//!"))
                .map(|l| l.trim_start_matches("//!"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        return 0;
    }
    let codec = match opt(args, "--codec")
        .unwrap_or("h264")
        .to_ascii_lowercase()
        .as_str()
    {
        "h265" | "hevc" => VideoCodec::H265,
        "av1" => VideoCodec::AV1,
        "vp9" => VideoCodec::VP9,
        "vp8" => VideoCodec::VP8,
        _ => VideoCodec::H264,
    };
    let (w, h) = opt(args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
        .unwrap_or((1280, 720));
    let fps: u32 = opt(args, "--fps")
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let mbps: f64 = opt(args, "--mbps")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3.0);
    let encoder = match opt(args, "--encoder").unwrap_or("auto") {
        "hw" => EncoderChoice::Hardware,
        "sw" => EncoderChoice::Software,
        _ => EncoderChoice::Auto,
    };
    let layering = match opt(args, "--layers").unwrap_or("single") {
        "single" => Layering::Single,
        "simulcast" => Layering::Simulcast,
        m => Layering::Svc(m.to_string()),
    };
    let source = match opt(args, "--source").map(parse_source) {
        None => SourceKind::Detail,
        Some(Ok(s)) => s,
        Some(Err(e)) => {
            eprintln!("{e}");
            return 2;
        }
    };
    // Check --switch up front, so a typo fails before connecting.
    let switches: Vec<(String, SourceKind)> = match opt(args, "--switch")
        .map(|l| {
            l.split(',')
                .map(|n| parse_source(n).map(|s| (n.to_string(), s)))
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()
    {
        Ok(v) => v.unwrap_or_default(),
        Err(e) => {
            eprintln!("--switch: {e}");
            return 2;
        }
    };
    let seconds: u64 = opt(args, "--seconds")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let lifecycle = args.iter().any(|a| a == "--lifecycle");

    let url = std::env::var("LIVEKIT_URL").unwrap_or_else(|_| "ws://127.0.0.1:7880".into());
    let key =
        std::env::var("LIVEKIT_API_KEY").unwrap_or_else(|_| crate::profiles::LOCAL_DEV_KEY.into());
    let secret = std::env::var("LIVEKIT_API_SECRET")
        .unwrap_or_else(|_| crate::profiles::LOCAL_DEV_SECRET.into());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let session = Session::start(rt.handle(), Arc::new(|| {}));
    let publish = PublishConfig {
        source: source.clone(),
        capture_w: w,
        capture_h: h,
        fps,
        max_fps: fps,
        publish_size: Some((w, h)),
        codec,
        encoder,
        bitrate_bps: (mbps * 1e6) as u64,
        degradation: DegradationPreference::Balanced,
        layering,
        screen_share: false,
        detail_hint: args.iter().any(|a| a == "--detail-hint"),
        audio: AudioConfig {
            input: match opt(args, "--audio") {
                None => AudioInput::Off,
                Some("tone") => AudioInput::Tone,
                Some(name) => AudioInput::Microphone(name.to_string()),
            },
            bitrate_bps: opt(args, "--audio-kbps")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(32)
                * 1000,
            dtx: !args.iter().any(|a| a == "--no-dtx"),
            red: !args.iter().any(|a| a == "--no-red"),
            echo_cancellation: !args.iter().any(|a| a == "--no-aec"),
            noise_suppression: !args.iter().any(|a| a == "--no-ns"),
            auto_gain: !args.iter().any(|a| a == "--no-agc"),
        },
    };
    let parsed = (
        opt(args, "--up").map(parse_impairment).transpose(),
        opt(args, "--down").map(parse_impairment).transpose(),
    );
    let (impair_up, impair_down) = match parsed {
        (Ok(u), Ok(d)) => (u, d),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    {
        let links = session.shared.lock().links.clone();
        *links.up.settings.lock() = impair_up.unwrap_or_default();
        *links.down.settings.lock() = impair_down.unwrap_or_default();
    }
    // The passphrase: LIVEKIT_E2EE_KEY keeps it out of process listings and
    // shell history; --e2ee still works for quick tests.
    let e2ee_key = std::env::var("LIVEKIT_E2EE_KEY")
        .ok()
        .or_else(|| opt(args, "--e2ee").map(str::to_string))
        .filter(|k| !k.is_empty());
    let room = format!("headless-{}", std::process::id());
    println!(
        "{} · room {room} · {url}",
        crate::version::summary().lines().next().unwrap_or_default()
    );
    // --viewer: serve the browser viewer page and print an adaptive-stream link.
    let _viewer = if args.iter().any(|a| a == "--viewer") {
        match (
            crate::viewer::ViewerServer::start(),
            crate::session::viewer_token(&key, &secret, &room, "browser-1"),
        ) {
            (Ok(v), Ok(tok)) => {
                println!("viewer: {}", v.link(&url, &tok, true, e2ee_key.as_deref()));
                Some(v)
            }
            _ => None,
        }
    } else {
        None
    };
    // --chrome-pub: receive a Chrome publisher (FlexFEC field trials) instead of publishing.
    if args.iter().any(|a| a == "--chrome-pub") {
        return chrome_pub(&url, &key, &secret, &room, &publish, seconds, rt.handle());
    }
    session.connect(
        ConnectConfig {
            url,
            api_key: key,
            api_secret: secret,
            room,
            decode: true,
            preview: false,
            e2ee_key: e2ee_key.clone(),
            impair: impair_up.is_some() || impair_down.is_some(),
        },
        Some(publish.clone()),
    );
    match wait_phase(&session, Phase::Publishing, Duration::from_secs(20)) {
        Ok(t) => println!("connected and publishing in {} ms", t.as_millis()),
        Err(e) => {
            eprintln!("FAILED to publish: {e}");
            return 2;
        }
    }

    let mut received = false;
    for sec in 1..=seconds {
        std::thread::sleep(Duration::from_secs(1));
        let (rh, rfps) = line(&session, sec);
        // Frames must be captured too: with a failed input WebRTC keeps
        // repeating the last frame, which would otherwise look received.
        let captured = session.shared.lock().snapshot.capture.fps > 0.0;
        if rh > 0 && rfps > 0.0 && captured {
            received = true;
        }
        if let Some(e) = session.shared.lock().error.clone() {
            eprintln!("error: {e}");
        }
    }

    let input_failed = session
        .shared
        .lock()
        .error
        .as_deref()
        .is_some_and(|e| e.starts_with("Input stopped"));
    if input_failed {
        println!("input failed during the run");
    }
    let mut ok = received && !input_failed;
    if publish.audio.input != AudioInput::Off {
        let a = session.shared.lock().snapshot.audio.clone();
        let heard = a.receiving && a.recv_kbps > 0.0;
        println!("audio: {}", if heard { "received" } else { "NOT received" });
        ok &= heard;
    }
    // --ask WxH: ask the SFU for a layer that fits a tile of that size.
    if let Some((aw, ah)) = opt(args, "--ask")
        .and_then(|s| s.split_once('x'))
        .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)))
    {
        session.request_size(Some((aw, ah)));
        std::thread::sleep(Duration::from_secs(5));
        let (rh, _) = line(&session, 0);
        println!("asked for {aw}x{ah}: now receiving {rh}p");
        ok &= rh > 0 && rh < h;
        // The request must survive a republish (a new track is subscribed).
        session.republish(publish.clone());
        std::thread::sleep(Duration::from_secs(6));
        let (rh, _) = line(&session, 0);
        println!("after republish: receiving {rh}p");
        ok &= rh > 0 && rh < h;
        session.request_size(None);
        std::thread::sleep(Duration::from_secs(5));
        let (rh, _) = line(&session, 0);
        println!("stopped asking: now receiving {rh}p");
        ok &= rh == h;
    }
    if lifecycle {
        let step = |name: &str, f: &dyn Fn(), want: Phase| -> bool {
            f();
            match wait_phase(&session, want, Duration::from_secs(15)) {
                Ok(t) => {
                    println!("{name}: {} ms", t.as_millis());
                    true
                }
                Err(e) => {
                    eprintln!("{name}: FAILED {e}");
                    false
                }
            }
        };
        ok &= step("unpublish", &|| session.unpublish(), Phase::Connected);
        std::thread::sleep(Duration::from_millis(500));
        ok &= step(
            "publish",
            &|| session.publish(publish.clone()),
            Phase::Publishing,
        );
        std::thread::sleep(Duration::from_secs(3));
        let mut changed = publish.clone();
        changed.bitrate_bps /= 2;
        session.republish(changed.clone());
        std::thread::sleep(Duration::from_secs(4));
        let bps = session
            .shared
            .lock()
            .active_publish
            .as_ref()
            .map(|p| p.bitrate_bps);
        println!(
            "republish: active cap now {:?} bps (wanted {})",
            bps, changed.bitrate_bps
        );
        ok &= bps == Some(changed.bitrate_bps);
        let mut other = changed.clone();
        other.source = if source == SourceKind::Pattern {
            SourceKind::Detail
        } else {
            SourceKind::Pattern
        };
        session.switch_source(other);
        std::thread::sleep(Duration::from_secs(3));
        let (rh, rfps) = line(&session, 0);
        println!("after source switch: receiving {rh}p at {rfps:.1} fps");
        ok &= rh > 0 && rfps > 0.0;
    }

    // --switch screen,camera: switch the live input in turn, 8 s each.
    for (name, kind) in &switches {
        {
            let mut next = publish.clone();
            next.source = kind.clone();
            session.switch_source(next.clone());
            let mut captured = false;
            for sec in 1..=8 {
                std::thread::sleep(Duration::from_secs(1));
                let snap = session.shared.lock().snapshot.clone();
                let err = session.shared.lock().error.clone();
                println!(
                    "switch {name} {sec}s: capture {}x{}@{:.1} ({}) phase {:?} error {:?}",
                    snap.capture.width,
                    snap.capture.height,
                    snap.capture.fps,
                    snap.capture.note,
                    session.phase(),
                    err
                );
                captured |= sec >= 3 && snap.capture.fps > 0.0 && err.is_none();
            }
            // Each switch must end up capturing the requested input.
            let active = session
                .shared
                .lock()
                .active_publish
                .as_ref()
                .map(|p| p.source.clone());
            let switched = captured && active.as_ref() == Some(kind);
            println!("switch {name}: {}", if switched { "ok" } else { "FAILED" });
            ok &= switched;
        }
    }

    let started = Instant::now();
    session.disconnect();
    match wait_phase(&session, Phase::Disconnected, Duration::from_secs(10)) {
        Ok(_) => {
            println!("disconnect: {} ms", started.elapsed().as_millis());
            let closing = session.shared.lock().closing.clone();
            while closing.load(std::sync::atomic::Ordering::SeqCst) > 0
                && started.elapsed() < Duration::from_secs(15)
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            println!(
                "connections closed in background after {} ms",
                started.elapsed().as_millis()
            );
        }
        Err(e) => {
            eprintln!("disconnect FAILED: {e}");
            ok = false;
        }
    }
    if ok {
        println!("RESULT: ok");
        0
    } else {
        println!("RESULT: FAILED (received video: {received})");
        1
    }
}

#[cfg(test)]
mod tests {
    use super::parse_impairment;

    #[test]
    fn impairment_fields() {
        let i = parse_impairment("2").unwrap();
        assert_eq!((i.loss_pct, i.delay_ms, i.rate_kbps), (2.0, 0, 0));
        let i = parse_impairment("1, 40,10,1500").unwrap();
        assert_eq!((i.delay_ms, i.jitter_ms, i.rate_kbps), (40, 10, 1500));
        assert!(parse_impairment("0,0,0,1O00").is_err());
        assert!(parse_impairment("0,,5").is_err());
        assert!(parse_impairment("-1").is_err());
        assert!(parse_impairment("1,2,3,4,5").is_err());
    }
}
