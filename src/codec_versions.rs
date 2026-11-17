//! Versions of the codec libraries inside the SDK's prebuilt WebRTC.
//!
//! libvpx, libaom, dav1d and OpenH264 each export a version function, and the
//! LiveKit Rust SDK links them into this binary as part of libwebrtc, so the
//! app can ask them directly. VideoToolbox, NVENC and VAAPI are the operating
//! system's or the driver's and have no version of their own here.

use std::ffi::{c_char, CStr};

#[repr(C)]
#[derive(Default)]
struct OpenH264Version {
    major: u32,
    minor: u32,
    revision: u32,
    reserved: u32,
}

extern "C" {
    fn vpx_codec_version_str() -> *const c_char;
    fn aom_codec_version_str() -> *const c_char;
    fn dav1d_version() -> *const c_char;
    fn WelsGetCodecVersionEx(version: *mut OpenH264Version);
}

fn c_str(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: each library returns a pointer to a static, NUL-terminated string.
    unsafe { CStr::from_ptr(p) }
        .to_string_lossy()
        .trim()
        .to_string()
}

#[derive(Clone, Debug, Default)]
pub struct CodecVersions {
    pub libvpx: String,
    pub libaom: String,
    pub dav1d: String,
    pub openh264: String,
}

pub fn read() -> CodecVersions {
    let mut h = OpenH264Version::default();
    // SAFETY: plain C functions with no arguments or one out-parameter we own.
    unsafe {
        WelsGetCodecVersionEx(&mut h);
        CodecVersions {
            libvpx: c_str(vpx_codec_version_str()),
            libaom: c_str(aom_codec_version_str()),
            dav1d: c_str(dav1d_version()),
            openh264: format!("{}.{}.{}", h.major, h.minor, h.revision),
        }
    }
}

impl CodecVersions {
    /// The version for a framework name as Host capabilities lists it.
    pub fn for_framework(&self, name: &str) -> String {
        match name {
            n if n.starts_with("libvpx") => self.libvpx.clone(),
            n if n.starts_with("libaom") => {
                format!("libaom {} · dav1d {}", self.libaom, self.dav1d)
            }
            n if n.starts_with("OpenH264") => self.openh264.clone(),
            _ => "system / driver".into(),
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "libvpx {} · libaom {} · dav1d {} · OpenH264 {}",
            self.libvpx, self.libaom, self.dav1d, self.openh264
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_are_readable() {
        let v = super::read();
        for s in [&v.libvpx, &v.libaom, &v.dav1d, &v.openh264] {
            assert!(
                !s.is_empty() && s.chars().any(|c| c.is_ascii_digit()),
                "{s:?}"
            );
        }
    }
}
