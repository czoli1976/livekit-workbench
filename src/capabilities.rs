//! What this machine and SDK build can encode and decode, asked from the SDK at
//! start-up. The main window only offers combinations that appear here.

use livekit::options::{VideoCodec, VideoEncoderBackend};
use livekit::rtc_engine::lk_runtime::LkRuntime;
use livekit::webrtc::prelude::MediaType;
use livekit::webrtc::video_codec::VideoCodec as RtcCodec;

pub const ALL_CODECS: [VideoCodec; 5] = [
    VideoCodec::H265,
    VideoCodec::H264,
    VideoCodec::AV1,
    VideoCodec::VP9,
    VideoCodec::VP8,
];

/// Scalability modes offered for SVC, simplest first.
pub const SVC_MODES: [&str; 12] = [
    "L1T2", "L1T3", "L2T1", "L2T2", "L2T3", "L3T1", "L3T2", "L3T3", "L2T2_KEY", "L2T3_KEY",
    "L3T2_KEY", "L3T3_KEY",
];

#[derive(Clone, Debug)]
pub struct CodecCaps {
    pub codec: VideoCodec,
    /// Hardware backends that can encode this codec here (VideoToolbox, NVENC…).
    pub hardware: Vec<VideoEncoderBackend>,
    pub software: bool,
    /// The receiver side negotiates this codec, so it can be decoded.
    pub decode: bool,
}

impl CodecCaps {
    pub fn can_encode(&self) -> bool {
        self.software || !self.hardware.is_empty()
    }
    pub fn has_hardware(&self) -> bool {
        !self.hardware.is_empty()
    }
    /// SVC needs a scalability-mode capable encoder. In this SDK only the
    /// software VP9 and AV1 encoders have one: the hardware AV1 encoders
    /// (NVENC, Jetson) advertise L1T1 only, and SVC is ignored for the others.
    pub fn svc_with(&self, encoder: EncoderChoice) -> bool {
        codec_supports_svc(self.codec)
            && self.software
            && matches!(encoder, EncoderChoice::Software | EncoderChoice::Auto)
            && !(encoder == EncoderChoice::Auto && self.has_hardware())
    }
}

/// The user's encoder preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EncoderChoice {
    #[default]
    Auto,
    Hardware,
    Software,
}

impl EncoderChoice {
    pub fn label(self) -> &'static str {
        match self {
            EncoderChoice::Auto => "Auto",
            EncoderChoice::Hardware => "Hardware",
            EncoderChoice::Software => "Software",
        }
    }
    pub fn backend(self) -> VideoEncoderBackend {
        match self {
            EncoderChoice::Auto => VideoEncoderBackend::Auto,
            EncoderChoice::Hardware => VideoEncoderBackend::Hardware,
            EncoderChoice::Software => VideoEncoderBackend::Software,
        }
    }
}

pub fn codec_supports_svc(codec: VideoCodec) -> bool {
    matches!(codec, VideoCodec::VP9 | VideoCodec::AV1)
}

/// Simulcast gives separate streams only for VP8, H.264 and H.265. Measured:
/// with three encodings and no scalability mode (which is what the SDK sends),
/// WebRTC turns VP9 into one stream with spatial layers and sends AV1 as a
/// single layer. LiveKit uses SVC for VP9 and AV1 instead.
pub fn codec_supports_simulcast(codec: VideoCodec) -> bool {
    !codec_supports_svc(codec)
}

pub fn codec_name(c: VideoCodec) -> &'static str {
    match c {
        VideoCodec::H265 => "H.265 / HEVC",
        VideoCodec::H264 => "H.264",
        VideoCodec::AV1 => "AV1",
        VideoCodec::VP9 => "VP9",
        VideoCodec::VP8 => "VP8",
    }
}

pub fn backend_name(b: VideoEncoderBackend) -> &'static str {
    match b {
        VideoEncoderBackend::Auto => "Auto",
        VideoEncoderBackend::Software => "Software",
        VideoEncoderBackend::Hardware => "Hardware",
        VideoEncoderBackend::Nvenc => "NVENC",
        VideoEncoderBackend::Vaapi => "VAAPI",
        VideoEncoderBackend::VideoToolbox => "VideoToolbox",
        VideoEncoderBackend::PreEncoded => "Pre-encoded",
        #[allow(unreachable_patterns)]
        _ => "Other",
    }
}

fn same_codec(a: RtcCodec, b: VideoCodec) -> bool {
    matches!(
        (a, b),
        (RtcCodec::VP8, VideoCodec::VP8)
            | (RtcCodec::VP9, VideoCodec::VP9)
            | (RtcCodec::AV1, VideoCodec::AV1)
            | (RtcCodec::H264, VideoCodec::H264)
            | (RtcCodec::H265, VideoCodec::H265)
    )
}

/// The codecs a hardware backend really encodes in hardware.
///
/// On Apple platforms the SDK's VideoToolbox backend is WebRTC's ObjC encoder
/// factory, which also carries the libvpx and libaom software encoders, so it
/// lists VP8, VP9 and AV1 too. Only H.264 and H.265 go through
/// VTCompressionSession; Apple has no VP8, VP9 or AV1 hardware encoder.
fn hardware_codecs(backend: VideoEncoderBackend, listed: Vec<RtcCodec>) -> Vec<RtcCodec> {
    match backend {
        VideoEncoderBackend::VideoToolbox => listed
            .into_iter()
            .filter(|c| matches!(c, RtcCodec::H264 | RtcCodec::H265))
            .collect(),
        _ => listed,
    }
}

#[derive(Clone, Debug)]
pub struct HostCaps {
    pub codecs: Vec<CodecCaps>,
    pub backends: Vec<VideoEncoderBackend>,
    /// Decodable codec MIME types, as the receiver advertises them.
    pub decoder_mimes: Vec<String>,
    /// Readable profile names per codec, for sending and for receiving.
    pub send_profiles: Vec<(VideoCodec, Vec<String>)>,
    pub recv_profiles: Vec<(VideoCodec, Vec<String>)>,
    /// One entry per video framework this build can use, hardware first.
    pub frameworks: Vec<Framework>,
    /// Hardware that exists but this build cannot use, and similar notes.
    pub hints: Vec<String>,
}

/// A video framework and what it does here.
#[derive(Clone, Debug)]
pub struct Framework {
    pub name: String,
    pub hardware: bool,
    pub encodes: Vec<VideoCodec>,
    pub decodes: Vec<VideoCodec>,
    /// Where the decode list comes from.
    pub decode_basis: &'static str,
}

impl HostCaps {
    pub fn probe() -> Self {
        let runtime = LkRuntime::instance();
        let factory = runtime.pc_factory();
        let backends: Vec<VideoEncoderBackend> =
            VideoEncoderBackend::list_available().into_iter().collect();
        let software = factory.video_encoder_codecs(VideoEncoderBackend::Software);
        let hw_backends: Vec<VideoEncoderBackend> = backends
            .iter()
            .copied()
            .filter(|b| {
                !matches!(
                    b,
                    VideoEncoderBackend::Auto
                        | VideoEncoderBackend::Software
                        | VideoEncoderBackend::Hardware
                        | VideoEncoderBackend::PreEncoded
                )
            })
            .collect();
        let hw_lists: Vec<(VideoEncoderBackend, Vec<RtcCodec>)> = hw_backends
            .iter()
            .map(|b| (*b, hardware_codecs(*b, factory.video_encoder_codecs(*b))))
            .collect();
        // A generic "Hardware" backend exists on Android: count it when no specific one does.
        let generic_hw = if hw_lists.is_empty() && backends.contains(&VideoEncoderBackend::Hardware)
        {
            factory.video_encoder_codecs(VideoEncoderBackend::Hardware)
        } else {
            Vec::new()
        };

        let receiver = factory.get_rtp_receiver_capabilities(MediaType::Video);
        let mut decoder_mimes: Vec<String> = receiver
            .codecs
            .iter()
            .map(|c| c.mime_type.to_ascii_uppercase())
            .filter(|m| {
                !["RTX", "RED", "ULPFEC", "FLEXFEC"]
                    .iter()
                    .any(|x| m.contains(x))
            })
            .collect();
        decoder_mimes.sort();
        decoder_mimes.dedup();

        let codecs = ALL_CODECS
            .iter()
            .map(|&codec| {
                let mut hardware: Vec<VideoEncoderBackend> = hw_lists
                    .iter()
                    .filter(|(_, list)| list.iter().any(|c| same_codec(*c, codec)))
                    .map(|(b, _)| *b)
                    .collect();
                if generic_hw.iter().any(|c| same_codec(*c, codec)) {
                    hardware.push(VideoEncoderBackend::Hardware);
                }
                let mime = match codec {
                    VideoCodec::H265 => "H265",
                    VideoCodec::H264 => "H264",
                    VideoCodec::AV1 => "AV1",
                    VideoCodec::VP9 => "VP9",
                    VideoCodec::VP8 => "VP8",
                };
                CodecCaps {
                    codec,
                    hardware,
                    software: software.iter().any(|c| same_codec(*c, codec)),
                    decode: decoder_mimes
                        .iter()
                        .any(|m| m.ends_with(&format!("/{mime}"))),
                }
            })
            .collect();
        let send_profiles = profiles(&factory.get_rtp_sender_capabilities(MediaType::Video).codecs);
        let recv_profiles = profiles(&receiver.codecs);
        let mut host = Self {
            codecs,
            backends,
            decoder_mimes,
            send_profiles,
            recv_profiles,
            frameworks: Vec::new(),
            hints: Vec::new(),
        };
        host.frameworks = frameworks(&host, &hw_lists, &software);
        host.hints = platform_hints(&host);
        host
    }

    pub fn get(&self, codec: VideoCodec) -> Option<&CodecCaps> {
        self.codecs.iter().find(|c| c.codec == codec)
    }

    /// Is this encoder choice possible for the codec? Returns the reason if not.
    pub fn encoder_allowed(&self, codec: VideoCodec, choice: EncoderChoice) -> Result<(), String> {
        let Some(c) = self.get(codec) else {
            return Err("Unknown codec".into());
        };
        match choice {
            EncoderChoice::Auto if c.can_encode() => Ok(()),
            EncoderChoice::Auto => Err(format!("No {} encoder in this build", codec_name(codec))),
            EncoderChoice::Hardware if c.has_hardware() => Ok(()),
            EncoderChoice::Hardware => Err(format!(
                "No hardware {} encoder on this machine",
                codec_name(codec)
            )),
            EncoderChoice::Software if c.software => Ok(()),
            EncoderChoice::Software => Err(format!(
                "No software {} encoder in this SDK build",
                codec_name(codec)
            )),
        }
    }

    /// The encoder Auto will pick: hardware when there is one, else software.
    pub fn auto_resolves_to(&self, codec: VideoCodec) -> String {
        match self.get(codec) {
            Some(c) if c.has_hardware() => {
                format!("{} (hardware)", backend_name(c.hardware[0]))
            }
            Some(c) if c.software => "software".to_string(),
            _ => "none".to_string(),
        }
    }
}

fn codec_of_mime(mime: &str) -> Option<VideoCodec> {
    let m = mime.to_ascii_uppercase();
    let name = m.rsplit('/').next().unwrap_or("");
    ALL_CODECS.iter().copied().find(|c| match c {
        VideoCodec::H265 => name == "H265" || name == "HEVC",
        VideoCodec::H264 => name == "H264",
        VideoCodec::AV1 => name == "AV1",
        VideoCodec::VP9 => name == "VP9",
        VideoCodec::VP8 => name == "VP8",
    })
}

/// Readable profile names per codec from WebRTC's codec capabilities.
fn profiles(
    caps: &[livekit::webrtc::rtp_parameters::RtpCodecCapability],
) -> Vec<(VideoCodec, Vec<String>)> {
    ALL_CODECS
        .iter()
        .map(|&codec| {
            let mut lines: Vec<String> = caps
                .iter()
                .filter(|c| codec_of_mime(&c.mime_type) == Some(codec))
                .map(|c| profile_label(codec, c.sdp_fmtp_line.as_deref().unwrap_or("")))
                .collect();
            lines.sort();
            lines.dedup();
            (codec, lines)
        })
        .filter(|(_, l)| !l.is_empty())
        .collect()
}

/// A readable profile name from an SDP fmtp line ("Constrained Baseline 3.1").
pub fn profile_label(codec: VideoCodec, fmtp: &str) -> String {
    let param = |k: &str| {
        fmtp.split(';')
            .find_map(|p| p.trim().strip_prefix(&format!("{k}=")).map(str::to_string))
    };
    match codec {
        VideoCodec::H264 => match param("profile-level-id") {
            Some(id) if id.len() == 6 => {
                let head = id[..4].to_ascii_lowercase();
                let profile = match head.as_str() {
                    "42e0" | "4240" | "42c0" => "Constrained Baseline",
                    p if p.starts_with("42") => "Baseline",
                    p if p.starts_with("4d") => "Main",
                    "640c" | "64c0" => "Constrained High",
                    p if p.starts_with("64") => "High",
                    p if p.starts_with("f4") => "High 4:4:4",
                    _ => "other",
                };
                let level = u8::from_str_radix(&id[4..], 16)
                    .map(|l| format!(" {}.{}", l / 10, l % 10))
                    .unwrap_or_default();
                format!("{profile}{level}")
            }
            _ => "default".into(),
        },
        VideoCodec::H265 => match param("profile-id").as_deref() {
            Some("1") => "Main".into(),
            Some("2") => "Main 10".into(),
            Some(p) => format!("profile {p}"),
            None => "default".into(),
        },
        VideoCodec::VP9 => format!(
            "profile {}",
            param("profile-id").unwrap_or_else(|| "0".into())
        ),
        VideoCodec::AV1 => match param("profile").as_deref() {
            Some("1") => "High".into(),
            Some("2") => "Professional".into(),
            _ => "Main".into(),
        },
        VideoCodec::VP8 => "default".into(),
    }
}

fn frameworks(
    host: &HostCaps,
    hw_lists: &[(VideoEncoderBackend, Vec<RtcCodec>)],
    software: &[RtcCodec],
) -> Vec<Framework> {
    let decodable = |c: VideoCodec| host.get(c).map(|x| x.decode).unwrap_or(false);
    let mut out: Vec<Framework> = Vec::new();
    for (backend, list) in hw_lists {
        let encodes: Vec<VideoCodec> = ALL_CODECS
            .iter()
            .copied()
            .filter(|c| list.iter().any(|r| same_codec(*r, *c)))
            .collect();
        // Hardware decoders the SDK wires up, from its source: VideoToolbox on
        // Apple platforms, NVDEC alongside NVENC. VAAPI and Jetson encode only.
        let (decodes, basis) = match backend {
            VideoEncoderBackend::VideoToolbox | VideoEncoderBackend::Nvenc => (
                [VideoCodec::H264, VideoCodec::H265]
                    .into_iter()
                    .filter(|c| decodable(*c))
                    .collect(),
                "from the SDK source",
            ),
            _ => (Vec::new(), "encode only in this SDK"),
        };
        let name = match backend {
            VideoEncoderBackend::Nvenc => "NVIDIA NVENC / NVDEC".to_string(),
            b => backend_name(*b).to_string(),
        };
        out.push(Framework {
            name,
            hardware: true,
            encodes,
            decodes,
            decode_basis: basis,
        });
    }
    let sw = |c: VideoCodec| software.iter().any(|r| same_codec(*r, c));
    let mut sw_frameworks = Vec::new();
    for (name, codecs, decoder) in [
        ("libvpx", &[VideoCodec::VP8, VideoCodec::VP9][..], true),
        (
            "libaom (encode) · dav1d (decode)",
            &[VideoCodec::AV1][..],
            true,
        ),
        ("OpenH264", &[VideoCodec::H264][..], false),
    ] {
        let encodes: Vec<VideoCodec> = codecs.iter().copied().filter(|c| sw(*c)).collect();
        let decodes: Vec<VideoCodec> = if decoder {
            codecs
                .iter()
                .copied()
                .filter(|c| decodable(*c) && !out.iter().any(|f| f.decodes.contains(c)))
                .collect()
        } else {
            Vec::new()
        };
        if !encodes.is_empty() || !decodes.is_empty() {
            sw_frameworks.push(Framework {
                name: name.to_string(),
                hardware: false,
                encodes,
                decodes,
                decode_basis: if decoder {
                    "from the SDK source"
                } else {
                    "encode only"
                },
            });
        }
    }
    out.extend(sw_frameworks);
    out
}

/// Hardware the machine has but this build cannot use, and platform notes.
fn platform_hints(host: &HostCaps) -> Vec<String> {
    let mut hints = Vec::new();
    let has = |b: VideoEncoderBackend| host.backends.contains(&b);
    if cfg!(target_os = "linux") {
        if std::path::Path::new("/proc/driver/nvidia/version").exists()
            && !has(VideoEncoderBackend::Nvenc)
        {
            hints.push("An NVIDIA GPU and driver are present, but this build has no NVENC/NVDEC: it was built without the CUDA toolkit. Install it (or set CUDA_HOME) and rebuild.".into());
        }
        if cfg!(target_arch = "x86_64")
            && std::path::Path::new("/dev/dri/renderD128").exists()
            && !has(VideoEncoderBackend::Vaapi)
        {
            hints.push("A GPU render node is present, but VAAPI is not in use: the build had no libva-dev, or the VAAPI driver is missing (intel-media-va-driver or mesa-va-drivers).".into());
        }
    }
    if cfg!(target_os = "windows") {
        hints.push("This SDK build has no hardware video on Windows: encoding and decoding are software only.".into());
    }
    for c in &host.codecs {
        if c.can_encode() && !c.decode {
            hints.push(format!(
                "{} can be sent but not received by this app: the SDK has no software decoder for it and no hardware one is in use. Browsers can receive it.",
                codec_name(c.codec)
            ));
        }
    }
    hints
}

/// Print what the SDK reports per backend, for `--caps` and bug reports.
pub fn print_probe() {
    let runtime = LkRuntime::instance();
    let factory = runtime.pc_factory();
    for b in VideoEncoderBackend::list_available() {
        let list: Vec<String> = factory
            .video_encoder_codecs(b)
            .iter()
            .map(|c| format!("{c:?}"))
            .collect();
        println!("{:<12} {}", backend_name(b), list.join(", "));
    }
    let caps = HostCaps::probe();
    for c in &caps.codecs {
        println!(
            "{:<14} hardware: {:<24} software: {:<5} decode: {}",
            codec_name(c.codec),
            c.hardware
                .iter()
                .map(|b| backend_name(*b))
                .collect::<Vec<_>>()
                .join(","),
            c.software,
            c.decode
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(codec: VideoCodec, hw: bool, sw: bool) -> CodecCaps {
        CodecCaps {
            codec,
            hardware: if hw {
                vec![VideoEncoderBackend::VideoToolbox]
            } else {
                vec![]
            },
            software: sw,
            decode: true,
        }
    }

    #[test]
    fn svc_only_for_software_vp9_and_av1() {
        let av1 = caps(VideoCodec::AV1, false, true);
        assert!(av1.svc_with(EncoderChoice::Software));
        assert!(av1.svc_with(EncoderChoice::Auto));
        assert!(!av1.svc_with(EncoderChoice::Hardware));
        let av1_with_hw = caps(VideoCodec::AV1, true, true);
        assert!(
            !av1_with_hw.svc_with(EncoderChoice::Auto),
            "Auto would pick hardware"
        );
        assert!(av1_with_hw.svc_with(EncoderChoice::Software));
        assert!(!caps(VideoCodec::H264, true, true).svc_with(EncoderChoice::Software));
        assert!(!caps(VideoCodec::VP8, false, true).svc_with(EncoderChoice::Software));
    }

    #[test]
    fn videotoolbox_is_h264_and_h265_only() {
        let all = vec![
            RtcCodec::H264,
            RtcCodec::VP8,
            RtcCodec::VP9,
            RtcCodec::AV1,
            RtcCodec::H265,
        ];
        let hw = hardware_codecs(VideoEncoderBackend::VideoToolbox, all.clone());
        assert_eq!(hw.len(), 2);
        assert!(hw
            .iter()
            .all(|c| matches!(c, RtcCodec::H264 | RtcCodec::H265)));
        assert_eq!(
            hardware_codecs(VideoEncoderBackend::Nvenc, all.clone()).len(),
            5
        );
    }

    #[test]
    fn simulcast_only_for_non_svc_codecs() {
        assert!(codec_supports_simulcast(VideoCodec::VP8));
        assert!(codec_supports_simulcast(VideoCodec::H264));
        assert!(codec_supports_simulcast(VideoCodec::H265));
        assert!(!codec_supports_simulcast(VideoCodec::VP9));
        assert!(!codec_supports_simulcast(VideoCodec::AV1));
    }

    #[test]
    fn profile_names() {
        let h = |f: &str| profile_label(VideoCodec::H264, f);
        assert_eq!(
            h("level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"),
            "Constrained Baseline 3.1"
        );
        assert_eq!(h("profile-level-id=640c34"), "Constrained High 5.2");
        assert_eq!(h("profile-level-id=4d001f"), "Main 3.1");
        assert_eq!(
            profile_label(VideoCodec::H265, "level-id=180;profile-id=2;tier-flag=0"),
            "Main 10"
        );
        assert_eq!(profile_label(VideoCodec::VP9, "profile-id=2"), "profile 2");
        assert_eq!(
            profile_label(VideoCodec::AV1, "level-idx=5;profile=0;tier=0"),
            "Main"
        );
    }

    #[test]
    fn encoder_rules() {
        let host = HostCaps {
            codecs: vec![
                caps(VideoCodec::H265, true, false),
                caps(VideoCodec::AV1, false, true),
            ],
            backends: vec![],
            decoder_mimes: vec![],
            send_profiles: vec![],
            recv_profiles: vec![],
            frameworks: vec![],
            hints: vec![],
        };
        assert!(host
            .encoder_allowed(VideoCodec::H265, EncoderChoice::Hardware)
            .is_ok());
        assert!(host
            .encoder_allowed(VideoCodec::H265, EncoderChoice::Software)
            .is_err());
        assert!(host
            .encoder_allowed(VideoCodec::AV1, EncoderChoice::Hardware)
            .is_err());
        assert!(host
            .encoder_allowed(VideoCodec::AV1, EncoderChoice::Auto)
            .is_ok());
        assert_eq!(host.auto_resolves_to(VideoCodec::AV1), "software");
    }
}
