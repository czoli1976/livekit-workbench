//! Server profiles: where to connect and with which credentials.
//!
//! Profiles live in a JSON file in the user's config directory. API secrets for
//! non-local profiles go to the operating system's credential store (macOS
//! Keychain, Windows Credential Manager, Secret Service on Linux), never the file.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const KEYRING_SERVICE: &str = "livekit-workbench";
pub const LOCAL_DEV_KEY: &str = "devkey";
pub const LOCAL_DEV_SECRET: &str = "secret";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileKind {
    /// A livekit-server started and owned by the app, in dev mode.
    LocalDev,
    /// LiveKit Cloud, or any server reached by URL with an API key and secret.
    Remote,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalServerOptions {
    /// Start the server when the app starts (otherwise from the status line).
    pub start_with_app: bool,
    /// Empty = find `livekit-server` on PATH or in the usual install locations.
    pub binary_path: String,
    pub port: u16,
    /// Listen on all interfaces so other devices on the network can join.
    pub bind_all: bool,
    pub flexfec_upstream: bool,
}

impl Default for LocalServerOptions {
    fn default() -> Self {
        Self {
            start_with_app: true,
            binary_path: String::new(),
            port: 7880,
            bind_all: false,
            flexfec_upstream: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub kind: ProfileKind,
    pub url: String,
    pub api_key: String,
    pub default_room: String,
    #[serde(default)]
    pub local: LocalServerOptions,
}

impl Profile {
    pub fn local_dev() -> Self {
        Self {
            id: "local-dev".into(),
            name: "Local dev".into(),
            kind: ProfileKind::LocalDev,
            url: "ws://127.0.0.1:7880".into(),
            api_key: LOCAL_DEV_KEY.into(),
            default_room: "workbench".into(),
            local: LocalServerOptions::default(),
        }
    }

    pub fn new_remote(id: String) -> Self {
        Self {
            id,
            name: "LiveKit Cloud".into(),
            kind: ProfileKind::Remote,
            url: "wss://your-project.livekit.cloud".into(),
            api_key: String::new(),
            default_room: "workbench".into(),
            local: LocalServerOptions::default(),
        }
    }

    pub fn is_local(&self) -> bool {
        self.kind == ProfileKind::LocalDev
    }

    /// URL the SDK connects to. For the local server it follows the chosen port.
    pub fn connect_url(&self) -> String {
        match self.kind {
            ProfileKind::LocalDev => format!("ws://127.0.0.1:{}", self.local.port),
            ProfileKind::Remote => self.url.trim().to_string(),
        }
    }

    /// The API secret: fixed for the dev server, from the credential store otherwise.
    pub fn api_secret(&self) -> Result<String> {
        match self.kind {
            ProfileKind::LocalDev => Ok(LOCAL_DEV_SECRET.into()),
            ProfileKind::Remote => {
                let entry = keyring::Entry::new(KEYRING_SERVICE, &self.id)
                    .context("opening the credential store")?;
                entry
                    .get_password()
                    .with_context(|| format!("no API secret saved for profile \"{}\"", self.name))
            }
        }
    }

    pub fn set_api_secret(&self, secret: &str) -> Result<()> {
        if self.kind == ProfileKind::LocalDev {
            return Ok(());
        }
        let entry = keyring::Entry::new(KEYRING_SERVICE, &self.id)
            .context("opening the credential store")?;
        entry.set_password(secret).context("saving the API secret")
    }

    pub fn delete_api_secret(&self) {
        if self.kind == ProfileKind::Remote {
            if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &self.id) {
                let _ = entry.delete_credential();
            }
        }
    }

    /// Problems that stop a connection; empty when the profile is usable.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.name.trim().is_empty() {
            problems.push("Give the profile a name.".to_string());
        }
        if self.kind == ProfileKind::Remote {
            let url = self.url.trim();
            if !(url.starts_with("wss://") || url.starts_with("ws://")) {
                problems.push("The server URL must start with wss:// or ws://.".to_string());
            }
            if self.api_key.trim().is_empty() {
                problems.push("Enter the API key.".to_string());
            }
        }
        if self.default_room.trim().is_empty() {
            problems.push("Enter a default room name.".to_string());
        }
        problems
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileStore {
    pub profiles: Vec<Profile>,
    pub default_id: String,
    #[serde(default)]
    next_id: u32,
}

impl Default for ProfileStore {
    fn default() -> Self {
        Self {
            profiles: vec![Profile::local_dev()],
            default_id: "local-dev".into(),
            next_id: 1,
        }
    }
}

impl ProfileStore {
    pub fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("io", "czoli1976", "livekit-workbench")
            .map(|d| d.config_dir().join("profiles.json"))
    }

    /// Load from disk; a missing or unreadable file gives the default store.
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<ProfileStore>(&s).ok())
            .map(|mut s| {
                s.repair();
                s
            })
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path().context("no config directory on this system")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).context("creating the config directory")?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))
    }

    /// Keep the store usable after hand edits: a local dev profile always exists
    /// and the default always points at a real profile.
    pub fn repair(&mut self) {
        if !self.profiles.iter().any(|p| p.is_local()) {
            self.profiles.insert(0, Profile::local_dev());
        }
        if !self.profiles.iter().any(|p| p.id == self.default_id) {
            self.default_id = self.profiles[0].id.clone();
        }
    }

    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn default_profile(&self) -> &Profile {
        self.get(&self.default_id).unwrap_or(&self.profiles[0])
    }

    pub fn add_remote(&mut self) -> String {
        self.next_id += 1;
        let id = format!("profile-{}", self.next_id);
        self.profiles.push(Profile::new_remote(id.clone()));
        id
    }

    pub fn duplicate(&mut self, id: &str) -> Option<String> {
        let mut copy = self.get(id)?.clone();
        if copy.is_local() {
            return None; // one local server is enough
        }
        self.next_id += 1;
        copy.id = format!("profile-{}", self.next_id);
        copy.name = format!("{} copy", copy.name);
        let new_id = copy.id.clone();
        self.profiles.push(copy);
        Some(new_id)
    }

    /// Remove a profile. The local dev profile cannot be removed.
    pub fn remove(&mut self, id: &str) -> bool {
        let Some(pos) = self.profiles.iter().position(|p| p.id == id) else {
            return false;
        };
        if self.profiles[pos].is_local() {
            return false;
        }
        let removed = self.profiles.remove(pos);
        removed.delete_api_secret();
        self.repair();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_store_has_local_dev_as_default() {
        let s = ProfileStore::default();
        assert_eq!(s.default_profile().kind, ProfileKind::LocalDev);
        assert_eq!(s.default_profile().connect_url(), "ws://127.0.0.1:7880");
    }

    #[test]
    fn local_profile_cannot_be_removed_or_duplicated() {
        let mut s = ProfileStore::default();
        assert!(!s.remove("local-dev"));
        assert!(s.duplicate("local-dev").is_none());
    }

    #[test]
    fn add_duplicate_remove_remote() {
        let mut s = ProfileStore::default();
        let id = s.add_remote();
        let copy = s.duplicate(&id).unwrap();
        assert_ne!(id, copy);
        assert_eq!(s.profiles.len(), 3);
        s.default_id = copy.clone();
        assert!(s.remove(&copy));
        assert_eq!(
            s.default_id, "local-dev",
            "default falls back after removal"
        );
    }

    #[test]
    fn repair_restores_local_profile() {
        let mut s = ProfileStore {
            profiles: vec![],
            default_id: "gone".into(),
            next_id: 0,
        };
        s.repair();
        assert_eq!(s.profiles.len(), 1);
        assert_eq!(s.default_id, "local-dev");
    }

    #[test]
    fn validation() {
        let mut p = Profile::new_remote("x".into());
        p.url = "https://example".into();
        let problems = p.validate();
        assert!(problems.iter().any(|m| m.contains("wss://")));
        assert!(problems.iter().any(|m| m.contains("API key")));
        assert!(Profile::local_dev().validate().is_empty());
    }

    #[test]
    fn round_trips_through_json() {
        let mut s = ProfileStore::default();
        s.add_remote();
        let json = serde_json::to_string(&s).unwrap();
        let back: ProfileStore = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn local_port_drives_url() {
        let mut p = Profile::local_dev();
        p.local.port = 7990;
        assert_eq!(p.connect_url(), "ws://127.0.0.1:7990");
    }
}
