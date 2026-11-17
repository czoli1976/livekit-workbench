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
}

fn path() -> Option<PathBuf> {
    directories::ProjectDirs::from("io", "czoli1976", "livekit-workbench")
        .map(|d| d.config_dir().join("settings.json"))
}

pub fn load() -> Option<Saved> {
    let text = std::fs::read_to_string(path()?).ok()?;
    serde_json::from_str(&text).ok()
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
        } else if s.source == "screen" {
            f.source = SourceKind::Screen;
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
        self.sanitize_form();
    }

    /// Write the settings about a second after they change, and on quit.
    pub(super) fn save_settings(&mut self, now: bool) {
        let s = self.to_saved();
        if Some(&s) == self.saved.as_ref() {
            return;
        }
        if !now && self.saved_at.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.saved_at = Instant::now();
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(&s) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&p, text) {
                    log::warn!("saving settings: {e}");
                }
            }
            Err(e) => log::warn!("saving settings: {e}"),
        }
        self.saved = Some(s);
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
    fn old_or_partial_files_load() {
        let s: Saved = serde_json::from_str(r#"{"codec":"vp9","unknown":1}"#).unwrap();
        assert_eq!(s.codec, "vp9");
        assert_eq!(s.fps, 0);
    }
}
