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
pub const TARGET: &str = env!("WB_TARGET");

/// Plain-text block for "Copy version info" and bug reports.
pub fn summary() -> String {
    format!(
        "{APP_NAME} {APP_VERSION}\nLiveKit Rust SDK {LIVEKIT_SDK} (libwebrtc {LIBWEBRTC}, webrtc-sys {WEBRTC_SYS})\negui {EGUI}\nTarget {TARGET}\nOS {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}
