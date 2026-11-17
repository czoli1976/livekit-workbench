//! Keeping the local livekit-server up to date.
//!
//! At start-up the app compares the installed server with LiveKit's latest
//! GitHub release. How it updates depends on how the server was installed:
//! - in the app's own folder (the build scripts put it there on Linux and
//!   Windows): download the release, check its SHA-256 against the release's
//!   checksums, and replace the binary;
//! - with Homebrew (macOS, where LiveKit publishes no release binaries):
//!   `brew upgrade livekit`;
//! - anywhere else: say where it is and leave it to the user.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};

const RELEASES: &str = "https://api.github.com/repos/livekit/livekit/releases/latest";
const USER_AGENT: &str = concat!("livekit-workbench/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug, PartialEq)]
pub enum InstallKind {
    /// In the app's data folder: the app can replace it.
    Managed,
    /// Installed with Homebrew.
    Homebrew,
    /// Somewhere the app should not touch.
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpdateInfo {
    pub binary: PathBuf,
    pub kind: InstallKind,
    pub installed: String,
    pub latest: String,
    pub tag: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate(String),
    Available(UpdateInfo),
    Updating(String),
    Updated(String),
    Failed(String),
}

/// Where the app keeps a server it installed itself.
pub fn managed_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("io", "czoli1976", "livekit-workbench")
        .map(|d| d.data_dir().join("bin"))
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "livekit-server.exe"
    } else {
        "livekit-server"
    }
}

/// "livekit-server version 1.13.11" → "1.13.11".
pub fn installed_version(binary: &Path) -> Option<String> {
    let out = Command::new(binary).arg("--version").output().ok()?;
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    parse_version(&text)
}

fn parse_version(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|w| w.trim_start_matches('v'))
        .find(|w| semver::Version::parse(w).is_ok())
        .map(str::to_string)
}

pub fn install_kind(binary: &Path) -> InstallKind {
    let resolved = std::fs::canonicalize(binary).unwrap_or_else(|_| binary.to_path_buf());
    if let Some(dir) = managed_dir() {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        if resolved.starts_with(&dir) {
            return InstallKind::Managed;
        }
    }
    let s = resolved.to_string_lossy();
    if s.contains("/Cellar/") || s.contains("/homebrew/") || s.contains("/linuxbrew/") {
        return InstallKind::Homebrew;
    }
    InstallKind::Other
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(120))
        .build()?)
}

/// The latest release's tag ("v1.13.11").
fn latest_tag() -> Result<String> {
    let release: serde_json::Value = client()?
        .get(RELEASES)
        .header("Accept", "application/vnd.github+json")
        .send()?
        .error_for_status()?
        .json()?;
    release["tag_name"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no tag in the latest release"))
}

fn newer(latest: &str, installed: &str) -> bool {
    match (
        semver::Version::parse(latest),
        semver::Version::parse(installed),
    ) {
        (Ok(l), Ok(i)) => l > i,
        _ => false,
    }
}

/// Compare the installed server with the latest release.
pub fn check(binary: &Path) -> Result<UpdateState> {
    let installed = installed_version(binary)
        .ok_or_else(|| anyhow!("could not read the version of {}", binary.display()))?;
    let tag = latest_tag().context("asking GitHub for the latest livekit-server release")?;
    let latest = tag.trim_start_matches('v').to_string();
    if !newer(&latest, &installed) {
        return Ok(UpdateState::UpToDate(installed));
    }
    Ok(UpdateState::Available(UpdateInfo {
        binary: binary.to_path_buf(),
        kind: install_kind(binary),
        installed,
        latest,
        tag,
    }))
}

/// The release asset for this machine, if LiveKit publishes one.
fn asset_name(version: &str) -> Option<String> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return None;
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "armv7",
        _ => return None,
    };
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    Some(format!("livekit_{version}_{os}_{arch}.{ext}"))
}

/// Run the update. `progress` receives short status lines.
pub fn update(info: &UpdateInfo, progress: &dyn Fn(String)) -> Result<String> {
    match info.kind {
        InstallKind::Managed => update_managed(info, progress),
        InstallKind::Homebrew => update_homebrew(info, progress),
        InstallKind::Other => bail!(
            "livekit-server at {} was not installed by this app or Homebrew; update it the way it was installed",
            info.binary.display()
        ),
    }
}

fn update_homebrew(info: &UpdateInfo, progress: &dyn Fn(String)) -> Result<String> {
    progress("Updating with Homebrew (brew upgrade livekit)…".into());
    let out = Command::new("brew")
        .args(["upgrade", "livekit"])
        .output()
        .context("running brew")?;
    let now = installed_version(&info.binary).unwrap_or_default();
    if !out.status.success() && now == info.installed {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "brew upgrade failed: {}",
            err.lines().last().unwrap_or("unknown error")
        );
    }
    if now == info.installed {
        bail!(
            "Homebrew has no {} yet (its formula can lag GitHub by a few days); still {now}",
            info.latest
        );
    }
    Ok(now)
}

fn update_managed(info: &UpdateInfo, progress: &dyn Fn(String)) -> Result<String> {
    let asset = asset_name(&info.latest)
        .ok_or_else(|| anyhow!("LiveKit publishes no livekit-server build for this machine"))?;
    let base = format!(
        "https://github.com/livekit/livekit/releases/download/{}",
        info.tag
    );
    let client = client()?;
    progress(format!("Downloading {asset}…"));
    let bytes = client
        .get(format!("{base}/{asset}"))
        .send()?
        .error_for_status()?
        .bytes()?;
    let sums = client
        .get(format!("{base}/checksums.txt"))
        .send()?
        .error_for_status()?
        .text()?;
    progress("Checking the download's SHA-256…".into());
    verify_checksum(&bytes, &sums, &asset)?;

    let dir = info
        .binary
        .parent()
        .ok_or_else(|| anyhow!("no folder for {}", info.binary.display()))?;
    let work = dir.join(".update");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let archive = work.join(&asset);
    std::fs::write(&archive, &bytes)?;
    progress("Unpacking…".into());
    // bsdtar (macOS, Windows 10+) and GNU tar both unpack .tar.gz; bsdtar also .zip.
    let status = Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&work)
        .status()
        .context("running tar")?;
    if !status.success() {
        bail!("could not unpack {asset}");
    }
    let new_bin = work.join(exe_name());
    if !new_bin.is_file() {
        bail!("{asset} has no {}", exe_name());
    }
    // Keep the old binary until the new one is in place.
    let backup = dir.join(format!("{}.old", exe_name()));
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(&info.binary, &backup)
        .context("moving the old server aside (is it still running?)")?;
    if let Err(e) = std::fs::rename(&new_bin, &info.binary) {
        let _ = std::fs::rename(&backup, &info.binary);
        return Err(e).context("putting the new server in place");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&info.binary, std::fs::Permissions::from_mode(0o755));
    }
    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::remove_file(&backup);
    installed_version(&info.binary).ok_or_else(|| anyhow!("the new server does not run"))
}

fn verify_checksum(bytes: &[u8], sums: &str, asset: &str) -> Result<()> {
    let expected = sums
        .lines()
        .find_map(|l| {
            let mut parts = l.split_whitespace();
            let hash = parts.next()?;
            (parts.next()? == asset).then(|| hash.to_ascii_lowercase())
        })
        .ok_or_else(|| anyhow!("{asset} is not in the release's checksums"))?;
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected {
        bail!("checksum mismatch for {asset}: the download is not what LiveKit published");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(
            parse_version("livekit-server version 1.13.11\n").as_deref(),
            Some("1.13.11")
        );
        assert_eq!(parse_version("v1.2.3").as_deref(), Some("1.2.3"));
        assert!(newer("1.13.12", "1.13.11"));
        assert!(!newer("1.13.11", "1.13.11"));
        assert!(!newer("1.9.0", "1.13.0"));
    }

    #[test]
    fn checksum() {
        let data = b"hello";
        let sums = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  livekit_1_linux_amd64.tar.gz\n";
        assert!(verify_checksum(data, sums, "livekit_1_linux_amd64.tar.gz").is_ok());
        assert!(verify_checksum(b"other", sums, "livekit_1_linux_amd64.tar.gz").is_err());
        assert!(verify_checksum(data, sums, "missing.zip").is_err());
    }

    #[test]
    fn homebrew_paths() {
        assert_eq!(
            install_kind(Path::new(
                "/opt/homebrew/Cellar/livekit/1.13.11/bin/livekit-server"
            )),
            InstallKind::Homebrew
        );
        assert_eq!(
            install_kind(Path::new("/usr/bin/livekit-server-nonexistent")),
            InstallKind::Other
        );
    }

    /// Live: an older server is reported as updatable.
    #[test]
    #[ignore]
    fn check_live_detects_older() {
        let dir = std::env::temp_dir().join("wb-fake-server");
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("livekit-server");
        std::fs::write(&bin, "#!/bin/sh\necho livekit-server version 1.0.0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        match check(&bin).unwrap() {
            UpdateState::Available(i) => {
                assert_eq!(i.installed, "1.0.0");
                assert_eq!(i.kind, InstallKind::Other);
                println!("latest {}", i.latest);
            }
            other => panic!("{other:?}"),
        }
    }

    /// Live: downloads the latest Linux release and checks it (no install).
    #[test]
    #[ignore]
    fn download_and_verify_live() {
        let tag = latest_tag().unwrap();
        let version = tag.trim_start_matches('v');
        let asset = format!("livekit_{version}_linux_amd64.tar.gz");
        let base = format!("https://github.com/livekit/livekit/releases/download/{tag}");
        let c = client().unwrap();
        let bytes = c
            .get(format!("{base}/{asset}"))
            .send()
            .unwrap()
            .bytes()
            .unwrap();
        let sums = c
            .get(format!("{base}/checksums.txt"))
            .send()
            .unwrap()
            .text()
            .unwrap();
        verify_checksum(&bytes, &sums, &asset).unwrap();
    }
}
