//! Publish settings and view choices, remembered between launches in
//! `settings.json` beside `profiles.json`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use livekit::options::{DegradationPreference, VideoCodec};
use serde::{Deserialize, Serialize};

use super::form::{LayerKind, PublishForm};
use super::pipeline::{ChartMetric, PreviewLayout, SubAsk};
use super::App;
use crate::capabilities::EncoderChoice;
use crate::capture::SourceKind;
use crate::session::AudioInput;

/// Everything as plain strings and numbers, so a newer or older app version
/// can read the file and fall back field by field.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    /// "camera:<name>", "pattern" or "detail".
    pub source: String,
    pub capture: (u32, u32),
    pub fps: u32,
    pub publish_size: Option<(u32, u32)>,
    pub codec: String,
    pub encoder: String,
    pub layers: String,
    pub svc_mode: String,
    pub bitrate_mbps: f32,
    pub max_fps: u32,
    pub degradation: String,
    pub screen_share: bool,
    pub detail_hint: bool,
    pub auto_publish: Option<bool>,
    pub previews: Option<bool>,
    pub layout: String,
    pub chart: String,
    pub settings_open: Option<bool>,
    pub sub_ask: String,
    pub e2ee: Option<bool>,
    /// A test passphrase, stored in plain text.
    pub e2ee_key: Option<String>,
    /// "off", "tone" or "mic:<name>".
    pub audio: String,
    pub audio_kbps: u64,
    pub audio_dtx: Option<bool>,
    pub audio_red: Option<bool>,
    pub audio_aec: Option<bool>,
    pub audio_agc: Option<bool>,
    pub audio_ns: Option<bool>,
    pub video_opts_open: Option<bool>,
    pub audio_opts_open: Option<bool>,
    pub impair: Option<bool>,
    pub impair_up: Option<crate::impair::Impairment>,
    pub impair_down: Option<crate::impair::Impairment>,
}

fn path() -> Option<PathBuf> {
    directories::ProjectDirs::from("io", "czoli1976", "livekit-workbench")
        .map(|d| d.config_dir().join("settings.json"))
}

/// The saved settings: `Ok(None)` when there are none yet. A file that cannot
/// be parsed is moved aside to `settings.bad.json`, so it is kept, and the
/// error is returned; a file that cannot be read is an error and left alone.
pub fn load() -> Result<Option<Saved>, String> {
    let Some(p) = path() else { return Ok(None) };
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("reading {}: {e}", p.display())),
    };
    serde_json::from_str(&text).map(Some).map_err(|e| {
        let bad = p.with_file_name("settings.bad.json");
        let _ = std::fs::rename(&p, &bad);
        format!(
            "{} is not valid ({e}); kept as {}",
            p.display(),
            bad.display()
        )
    })
}

/// Write `text` to `p` through a temporary file, so a failed or interrupted
/// write never leaves a half-written settings file.
fn write_atomic(p: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, p).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn codec_key(c: VideoCodec) -> &'static str {
    match c {
        VideoCodec::H264 => "h264",
        VideoCodec::H265 => "h265",
        VideoCodec::VP8 => "vp8",
        VideoCodec::VP9 => "vp9",
        VideoCodec::AV1 => "av1",
    }
}

fn codec_from(s: &str) -> Option<VideoCodec> {
    Some(match s {
        "h264" => VideoCodec::H264,
        "h265" => VideoCodec::H265,
        "vp8" => VideoCodec::VP8,
        "vp9" => VideoCodec::VP9,
        "av1" => VideoCodec::AV1,
        _ => return None,
    })
}

fn degradation_key(d: DegradationPreference) -> &'static str {
    match d {
        DegradationPreference::MaintainResolution => "resolution",
        DegradationPreference::MaintainFramerate => "framerate",
        DegradationPreference::MaintainFramerateAndResolution => "both",
        _ => "balanced",
    }
}

fn degradation_from(s: &str) -> DegradationPreference {
    match s {
        "resolution" => DegradationPreference::MaintainResolution,
        "framerate" => DegradationPreference::MaintainFramerate,
        "both" => DegradationPreference::MaintainFramerateAndResolution,
        _ => DegradationPreference::Balanced,
    }
}

impl App {
    fn to_saved(&self) -> Saved {
        let f = &self.form;
        let links = self.session.shared.lock().links.clone();
        let (impair_up, impair_down) = (*links.up.settings.lock(), *links.down.settings.lock());
        let source = match &f.source {
            SourceKind::Camera(i) => match self.cameras.iter().find(|c| c.index == *i) {
                Some(c) => format!("camera:{}", c.name),
                None => "detail".into(),
            },
            SourceKind::Pattern => "pattern".into(),
            SourceKind::Detail => "detail".into(),
            SourceKind::Screen => "screen".into(),
        };
        Saved {
            source,
            capture: f.capture,
            fps: f.fps,
            publish_size: f.publish_size,
            codec: codec_key(f.codec).into(),
            encoder: format!("{:?}", f.encoder).to_lowercase(),
            layers: format!("{:?}", f.layers).to_lowercase(),
            svc_mode: f.svc_mode.clone(),
            bitrate_mbps: f.bitrate_mbps,
            max_fps: f.max_fps,
            degradation: degradation_key(f.degradation).into(),
            screen_share: f.screen_share,
            detail_hint: f.detail_hint,
            auto_publish: Some(self.auto_publish),
            previews: Some(self.previews),
            layout: format!("{:?}", self.layout).to_lowercase(),
            chart: format!("{:?}", self.chart).to_lowercase(),
            settings_open: Some(self.settings_open),
            sub_ask: format!("{:?}", self.sub_ask).to_lowercase(),
            e2ee: Some(self.e2ee_on),
            e2ee_key: Some(self.e2ee_key.clone()),
            audio: match &f.audio.input {
                AudioInput::Off => "off".into(),
                AudioInput::Tone => "tone".into(),
                AudioInput::Microphone(n) => format!("mic:{n}"),
            },
            audio_kbps: f.audio.bitrate_bps / 1000,
            audio_dtx: Some(f.audio.dtx),
            audio_red: Some(f.audio.red),
            audio_aec: Some(f.audio.echo_cancellation),
            audio_agc: Some(f.audio.auto_gain),
            audio_ns: Some(f.audio.noise_suppression),
            video_opts_open: Some(self.video_opts_open),
            audio_opts_open: Some(self.audio_opts_open),
            impair: Some(self.impair_on),
            impair_up: Some(impair_up),
            impair_down: Some(impair_down),
        }
    }

    /// Apply saved settings over the defaults. Anything missing or no longer
    /// valid keeps its default; `sanitize_form` then fixes what this machine
    /// cannot do.
    pub(super) fn apply_saved(&mut self, s: &Saved) {
        let d = PublishForm::default();
        let f = &mut self.form;
        if let Some(name) = s.source.strip_prefix("camera:") {
            if let Some(c) = self.cameras.iter().find(|c| c.name == name) {
                f.source = SourceKind::Camera(c.index);
            }
        } else if s.source == "pattern" {
            f.source = SourceKind::Pattern;
        } else if s.source == "detail" {
            f.source = SourceKind::Detail;
        }
        if s.capture.0 > 0 && s.capture.1 > 0 {
            f.capture = s.capture;
        }
        if s.fps > 0 {
            f.fps = s.fps;
        }
        f.publish_size = s.publish_size;
        f.codec = codec_from(&s.codec).unwrap_or(d.codec);
        f.encoder = match s.encoder.as_str() {
            "hardware" => EncoderChoice::Hardware,
            "software" => EncoderChoice::Software,
            _ => EncoderChoice::Auto,
        };
        f.layers = match s.layers.as_str() {
            "simulcast" => LayerKind::Simulcast,
            "svc" => LayerKind::Svc,
            _ => LayerKind::Single,
        };
        if !s.svc_mode.is_empty() {
            f.svc_mode = s.svc_mode.clone();
        }
        if s.bitrate_mbps > 0.0 {
            f.bitrate_mbps = s.bitrate_mbps;
        }
        if s.max_fps > 0 {
            f.max_fps = s.max_fps;
        }
        f.degradation = degradation_from(&s.degradation);
        f.screen_share = s.screen_share;
        f.detail_hint = s.detail_hint;
        if let Some(v) = s.auto_publish {
            self.auto_publish = v;
        }
        if let Some(v) = s.previews {
            self.previews = v;
        }
        self.layout = match s.layout.as_str() {
            "localonly" => PreviewLayout::LocalOnly,
            "faronly" => PreviewLayout::FarOnly,
            _ => PreviewLayout::SideBySide,
        };
        self.chart = match s.chart.as_str() {
            "resolution" => ChartMetric::Resolution,
            "framerate" => ChartMetric::FrameRate,
            _ => ChartMetric::Bitrate,
        };
        if let Some(v) = s.settings_open {
            self.settings_open = v;
        }
        self.sub_ask = match s.sub_ask.as_str() {
            "fittile" => SubAsk::FitTile,
            "half" => SubAsk::Half,
            "quarter" => SubAsk::Quarter,
            _ => SubAsk::Top,
        };
        if let Some(v) = s.e2ee {
            self.e2ee_on = v;
        }
        if let Some(k) = s.e2ee_key.as_ref().filter(|k| !k.is_empty()) {
            self.e2ee_key = k.clone();
        }
        let a = &mut self.form.audio;
        a.input = match s.audio.as_str() {
            "tone" => AudioInput::Tone,
            m if m.starts_with("mic:") => AudioInput::Microphone(m[4..].to_string()),
            _ => AudioInput::Off,
        };
        if s.audio_kbps > 0 {
            a.bitrate_bps = s.audio_kbps * 1000;
        }
        for (slot, v) in [
            (&mut a.dtx, s.audio_dtx),
            (&mut a.red, s.audio_red),
            (&mut a.echo_cancellation, s.audio_aec),
            (&mut a.auto_gain, s.audio_agc),
            (&mut a.noise_suppression, s.audio_ns),
        ] {
            if let Some(v) = v {
                *slot = v;
            }
        }
        if let Some(v) = s.video_opts_open {
            self.video_opts_open = v;
        }
        if let Some(v) = s.audio_opts_open {
            self.audio_opts_open = v;
        }
        if let Some(v) = s.impair {
            self.impair_on = v;
        }
        let links = self.session.shared.lock().links.clone();
        if let Some(v) = s.impair_up {
            *links.up.settings.lock() = v;
        }
        if let Some(v) = s.impair_down {
            *links.down.settings.lock() = v;
        }
        self.sanitize_form();
    }

    /// Write the settings about a second after they change, and on quit.
    pub(super) fn save_settings(&mut self, now: bool) {
        if !self.persist_settings {
            return;
        }
        let s = self.to_saved();
        if Some(&s) == self.saved.as_ref() {
            return;
        }
        if !now && self.saved_at.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.saved_at = Instant::now();
        let Some(p) = path() else { return };
        // Recorded only once written, so a failed write is retried.
        match serde_json::to_string_pretty(&s) {
            Ok(text) => match write_atomic(&p, &text) {
                Ok(()) => self.saved = Some(s),
                Err(e) => log::warn!("saving settings: {e}"),
            },
            Err(e) => log::warn!("saving settings: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        for c in crate::capabilities::ALL_CODECS {
            assert_eq!(codec_from(codec_key(c)), Some(c));
        }
        for d in [
            DegradationPreference::MaintainResolution,
            DegradationPreference::MaintainFramerate,
            DegradationPreference::MaintainFramerateAndResolution,
            DegradationPreference::Balanced,
        ] {
            assert_eq!(degradation_from(degradation_key(d)), d);
        }
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("wb-saved-{}", std::process::id()));
        let p = dir.join("settings.json");
        write_atomic(&p, "{\"codec\":\"vp8\"}").unwrap();
        write_atomic(&p, "{\"codec\":\"av1\"}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"codec\":\"av1\"}");
        assert!(!p.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_or_partial_files_load() {
        let s: Saved = serde_json::from_str(r#"{"codec":"vp9","unknown":1}"#).unwrap();
        assert_eq!(s.codec, "vp9");
        assert_eq!(s.fps, 0);
    }
}
