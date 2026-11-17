//! Build script:
//! - records the exact LiveKit SDK versions resolved in Cargo.lock, so the About
//!   dialog shows what was really compiled in, not what Cargo.toml allows;
//! - links with -ObjC on macOS, which WebRTC's Objective-C categories need;
//! - links GLib/GIO on Linux, which WebRTC's screen capture (the PipeWire
//!   portal) calls; webrtc-sys compiles against their headers only.

use std::path::PathBuf;

/// A file from a crates.io dependency's source in Cargo's registry.
fn crate_file(name: &str, version: &str, file: &str) -> Option<String> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".cargo")))?;
    let src = home.join("registry").join("src");
    for index in std::fs::read_dir(src).ok()?.flatten() {
        let path = index.path().join(format!("{name}-{version}")).join(file);
        if let Ok(text) = std::fs::read_to_string(&path) {
            return Some(text);
        }
    }
    None
}

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

    // The prebuilt WebRTC the SDK downloads: its build tag comes from the
    // webrtc-sys-build crate, its Chromium milestone from webrtc-sys's
    // changelog (the newest "upgrade libwebrtc to mNNN" entry).
    let tag = crate_file(
        "webrtc-sys-build",
        &version_of("webrtc-sys-build"),
        "src/lib.rs",
    )
    .and_then(|src| {
        src.lines()
            .find(|l| l.contains("WEBRTC_TAG: &str"))
            .and_then(|l| l.split('"').nth(1).map(str::to_string))
    })
    .unwrap_or_else(|| "unknown".into());
    let milestone = crate_file("webrtc-sys", &version_of("webrtc-sys"), "CHANGELOG.md")
        .and_then(|log| {
            log.lines().find_map(|l| {
                let i = l.find("upgrade libwebrtc to m")? + "upgrade libwebrtc to ".len();
                let m: String = l[i..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect();
                (m.len() > 1).then_some(m)
            })
        })
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=WB_WEBRTC_TAG={tag}");
    println!("cargo:rustc-env=WB_WEBRTC_MILESTONE={milestone}");

    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=WB_TARGET={target}");

    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => println!("cargo:rustc-link-arg=-ObjC"),
        Ok("linux") => {
            for lib in ["gio-2.0", "gobject-2.0", "glib-2.0"] {
                println!("cargo:rustc-link-lib=dylib={lib}");
            }
        }
        _ => {}
    }
}
