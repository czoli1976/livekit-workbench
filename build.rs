//! Build script:
//! - records the exact LiveKit SDK versions resolved in Cargo.lock, so the About
//!   dialog shows what was really compiled in, not what Cargo.toml allows;
//! - links with -ObjC on macOS, which WebRTC's Objective-C categories need.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=build.rs");

    let lock_path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("Cargo.lock");
    let lock = std::fs::read_to_string(&lock_path).unwrap_or_default();
    let parsed: toml::Table = lock.parse().unwrap_or_default();

    let version_of = |name: &str| -> String {
        parsed
            .get("package")
            .and_then(|p| p.as_array())
            .and_then(|pkgs| {
                pkgs.iter()
                    .find(|p| p.get("name").and_then(|n| n.as_str()) == Some(name))
            })
            .and_then(|p| p.get("version"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string()
    };

    println!(
        "cargo:rustc-env=WB_LIVEKIT_VERSION={}",
        version_of("livekit")
    );
    println!(
        "cargo:rustc-env=WB_LIBWEBRTC_VERSION={}",
        version_of("libwebrtc")
    );
    println!(
        "cargo:rustc-env=WB_WEBRTC_SYS_VERSION={}",
        version_of("webrtc-sys")
    );
    println!("cargo:rustc-env=WB_EGUI_VERSION={}", version_of("egui"));

    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=WB_TARGET={target}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-ObjC");
    }
}
