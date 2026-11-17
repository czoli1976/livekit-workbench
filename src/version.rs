//! Version information baked in at build time (see build.rs).

pub const APP_NAME: &str = "Workbench for LiveKit";
pub const APP_VERSION: &str = "1.0";
pub const AUTHOR_HANDLE: &str = "czoli1976";
pub const AUTHOR_GITHUB: &str = "https://github.com/czoli1976";
pub const AUTHOR_LINKEDIN: &str = "https://www.linkedin.com/in/ckristianzoli";
pub const REPO_URL: &str = "https://github.com/czoli1976/livekit-workbench";
pub const LICENSE: &str = "Apache 2.0";

/// Exact `livekit` crate version resolved in Cargo.lock for this build.
pub const LIVEKIT_SDK: &str = env!("WB_LIVEKIT_VERSION");
pub const LIBWEBRTC: &str = env!("WB_LIBWEBRTC_VERSION");
pub const WEBRTC_SYS: &str = env!("WB_WEBRTC_SYS_VERSION");
pub const EGUI: &str = env!("WB_EGUI_VERSION");
/// The prebuilt WebRTC inside the SDK: Chromium milestone and LiveKit build tag.
pub const WEBRTC_MILESTONE: &str = env!("WB_WEBRTC_MILESTONE");
pub const WEBRTC_TAG: &str = env!("WB_WEBRTC_TAG");

/// WebRTC's own "source stamp" (the date of its source), which it compiles
/// into the binary as a string. Found by scanning this executable once.
pub fn webrtc_source_stamp() -> &'static str {
    static STAMP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    STAMP.get_or_init(|| {
        const KEY: &[u8] = b"WebRTC source stamp ";
        let Ok(exe) = std::env::current_exe().and_then(std::fs::read) else {
            return String::new();
        };
        exe.windows(KEY.len())
            .position(|w| w == KEY)
            .map(|i| {
                exe[i + KEY.len()..]
                    .iter()
                    .take_while(|b| b.is_ascii_graphic())
                    .map(|b| *b as char)
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// "WebRTC m150 (webrtc-a545de4, source 2026-06-01)".
pub fn webrtc() -> String {
    let stamp = webrtc_source_stamp();
    let date = stamp.split('T').next().unwrap_or("");
    let mut s = format!("WebRTC {WEBRTC_MILESTONE} ({WEBRTC_TAG}");
    if !date.is_empty() {
        s.push_str(&format!(", source {date}"));
    }
    s.push(')');
    s
}
pub const TARGET: &str = env!("WB_TARGET");

/// Plain-text block for "Copy version info" and bug reports.
pub fn summary() -> String {
    format!(
        "{APP_NAME} {APP_VERSION}\nLiveKit Rust SDK {LIVEKIT_SDK} (libwebrtc {LIBWEBRTC}, webrtc-sys {WEBRTC_SYS})\n{}\negui {EGUI}\nTarget {TARGET}\nOS {} {}",
        webrtc(),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}
