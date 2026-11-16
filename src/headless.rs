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
//!     --lifecycle                      also exercise unpublish, publish, republish, switch source
//!
//! Connects with LIVEKIT_URL, LIVEKIT_API_KEY and LIVEKIT_API_SECRET, or the
//! local dev server defaults. Exits 0 when video made it through and back.

use std::sync::Arc;
use std::time::{Duration, Instant};

use livekit::options::{DegradationPreference, VideoCodec};

use crate::capabilities::EncoderChoice;
use crate::capture::SourceKind;
use crate::session::{ConnectConfig, Layering, Phase, PublishConfig, Session};

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

pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help") {
        println!(
            "{}",
            include_str!("headless.rs")
                .lines()
                .take(17)
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
    let source = if opt(args, "--source") == Some("pattern") {
        SourceKind::Pattern
    } else {
        SourceKind::Detail
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
        detail_hint: false,
    };
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
                println!("viewer: {}", v.link(&url, &tok, true));
                Some(v)
            }
            _ => None,
        }
    } else {
        None
    };
    session.connect(
        ConnectConfig {
            url,
            api_key: key,
            api_secret: secret,
            room,
            decode: true,
            preview: false,
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
        if rh > 0 && rfps > 0.0 {
            received = true;
        }
        if let Some(e) = session.shared.lock().error.clone() {
            eprintln!("error: {e}");
        }
    }

    let mut ok = received;
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
