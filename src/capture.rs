//! Video sources: cameras (through nokhwa) and two synthetic generators.
//!
//! Every source produces I420 frames at the requested rate and hands them to a
//! callback; the session scales them to the publish size and feeds the SDK.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use livekit::webrtc::native::yuv_helper;
use livekit::webrtc::video_frame::I420Buffer;

#[derive(Clone, Debug, PartialEq)]
pub enum SourceKind {
    /// Index into the camera list.
    Camera(u32),
    /// A texture that scrolls sideways: cheap to encode, needs little bitrate.
    Pattern,
    /// The pattern plus grain that changes every frame: needs a realistic bitrate.
    Detail,
}

impl SourceKind {
    pub fn label(&self) -> String {
        match self {
            SourceKind::Camera(i) => format!("camera {i}"),
            SourceKind::Pattern => "synthetic pattern".into(),
            SourceKind::Detail => "synthetic detailed motion".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CameraDesc {
    pub index: u32,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CameraFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// What the capture side actually got (it can differ from what was asked).
#[derive(Clone, Debug, Default)]
pub struct CaptureInfo {
    pub width: u32,
    pub height: u32,
    pub note: String,
}

/// Ask the OS for camera access (macOS shows its permission prompt here).
pub fn init_camera_access() {
    #[cfg(target_os = "macos")]
    nokhwa::nokhwa_initialize(|granted| {
        if !granted {
            log::warn!("camera access was not granted; synthetic sources still work");
        }
    });
}

pub fn list_cameras() -> Result<Vec<CameraDesc>> {
    use nokhwa::utils::{ApiBackend, CameraIndex};
    let cams = nokhwa::query(ApiBackend::Auto).map_err(|e| anyhow!("{e}"))?;
    Ok(cams
        .iter()
        .enumerate()
        .map(|(i, c)| CameraDesc {
            index: match c.index() {
                CameraIndex::Index(n) => *n,
                _ => i as u32,
            },
            name: c.human_name(),
        })
        .collect())
}

/// Formats the camera advertises, largest first, one entry per size and rate.
pub fn camera_formats(index: u32) -> Result<Vec<CameraFormat>> {
    camera_access()?;
    use nokhwa::pixel_format::RgbAFormat;
    use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
    let mut cam = nokhwa::Camera::new(
        CameraIndex::Index(index),
        RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestResolution),
    )
    .map_err(|e| anyhow!("{e}"))?;
    let mut out: Vec<CameraFormat> = cam
        .compatible_camera_formats()
        .map_err(|e| anyhow!("{e}"))?
        .iter()
        .map(|f| CameraFormat {
            width: f.width(),
            height: f.height(),
            fps: f.frame_rate(),
        })
        .collect();
    out.sort_by_key(|f| std::cmp::Reverse((f.width * f.height, f.fps)));
    out.dedup();
    Ok(out)
}

/// Run a source until `stop` is set. `on_frame` receives each captured frame.
pub fn run(
    source: &SourceKind,
    width: u32,
    height: u32,
    fps: u32,
    stop: &AtomicBool,
    on_info: impl FnOnce(CaptureInfo),
    on_frame: impl FnMut(I420Buffer),
) -> Result<()> {
    match source {
        SourceKind::Camera(index) => {
            run_camera(*index, width, height, fps, stop, on_info, on_frame)
        }
        SourceKind::Pattern | SourceKind::Detail => {
            let detail = *source == SourceKind::Detail;
            on_info(CaptureInfo {
                width,
                height,
                note: source.label(),
            });
            run_synthetic(width, height, fps, detail, stop, on_frame);
            Ok(())
        }
    }
}

fn run_synthetic(
    width: u32,
    height: u32,
    fps: u32,
    detail: bool,
    stop: &AtomicBool,
    mut on_frame: impl FnMut(I420Buffer),
) {
    let mut gen = Synthetic::new(width, height, detail);
    let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let mut next = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        on_frame(gen.next_frame());
        next += interval;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now; // the generator cannot keep up; do not build up a debt
        }
    }
}

/// Camera threads that had not finished when their capture ended, by camera.
static CAMERA_WORKERS: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<u32, std::thread::JoinHandle<()>>>,
> = std::sync::LazyLock::new(Default::default);

enum CamMsg {
    Info(CaptureInfo),
    Frame(I420Buffer),
    Failed(String),
}

/// No first frame within this long after the stream opened: give up.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(8);
/// Frames stopped arriving for this long: give up.
const FRAME_GAP_TIMEOUT: Duration = Duration::from_secs(5);

/// Is the app allowed to use the camera? On macOS the answer belongs to the
/// app bundle, or to the terminal the binary was started from.
pub fn camera_access() -> Result<()> {
    #[cfg(target_os = "macos")]
    if !nokhwa::nokhwa_check() && std::env::var_os("WB_SKIP_CAMERA_CHECK").is_none() {
        let bundled = std::env::current_exe()
            .map(|p| p.to_string_lossy().contains(".app/Contents/MacOS"))
            .unwrap_or(false);
        let who = if bundled {
            "Workbench for LiveKit"
        } else {
            "the terminal or app that started Workbench (or start it from dist/Workbench for LiveKit.app)"
        };
        return Err(anyhow!(
            "camera access is not allowed. Allow it in System Settings › Privacy & Security › Camera for {who}, then restart the app"
        ));
    }
    Ok(())
}

/// Capture from a camera. The camera runs on its own thread: reading a frame
/// can block indefinitely (no permission, a device that never starts), so this
/// side waits with timeouts, checks `stop` every 200 ms, and never waits for
/// that thread to finish.
fn run_camera(
    index: u32,
    width: u32,
    height: u32,
    fps: u32,
    stop: &AtomicBool,
    on_info: impl FnOnce(CaptureInfo),
    mut on_frame: impl FnMut(I420Buffer),
) -> Result<()> {
    use std::sync::mpsc::{sync_channel, RecvTimeoutError};
    use std::sync::Arc;
    camera_access()?;
    // At most one camera thread per camera: one stuck inside the OS stays
    // registered until it exits, and no other is started for that camera.
    {
        let mut workers = CAMERA_WORKERS.lock();
        if let Some(h) = workers.get(&index) {
            if !h.is_finished() {
                return Err(anyhow!(
                    "the camera has not been released by its previous use yet; try again in a moment"
                ));
            }
            workers.remove(&index);
        }
    }
    let inner_stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = sync_channel::<CamMsg>(2);
    let camera_thread = {
        let inner_stop = inner_stop.clone();
        std::thread::Builder::new()
            .name("camera".into())
            .spawn(move || {
                if let Err(e) = camera_loop(index, width, height, fps, &inner_stop, &tx) {
                    let _ = tx.send(CamMsg::Failed(format!("{e:#}")));
                }
            })
            .map_err(|e| anyhow!("starting the camera thread: {e}"))?
    };
    let mut on_info = Some(on_info);
    let mut opened = Instant::now();
    let mut last_frame: Option<Instant> = None;
    let result = loop {
        if stop.load(Ordering::SeqCst) {
            break Ok(());
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(CamMsg::Info(i)) => {
                opened = Instant::now();
                if let Some(f) = on_info.take() {
                    f(i);
                }
            }
            Ok(CamMsg::Frame(buf)) => {
                last_frame = Some(Instant::now());
                on_frame(buf);
            }
            Ok(CamMsg::Failed(e)) => break Err(anyhow!(e)),
            Err(RecvTimeoutError::Timeout) => match last_frame {
                None if opened.elapsed() > FIRST_FRAME_TIMEOUT => {
                    break Err(anyhow!(
                        "the camera sent no frames within {} s. It may be in use by another app, or camera access may be blocked",
                        FIRST_FRAME_TIMEOUT.as_secs()
                    ))
                }
                Some(t) if t.elapsed() > FRAME_GAP_TIMEOUT => {
                    break Err(anyhow!("the camera stopped sending frames"))
                }
                _ => {}
            },
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        }
    };
    // Tell the camera thread to stop and give it up to 2 s to release the
    // camera, so the next user (preview to publisher) can open it. If it is
    // stuck inside the OS, do not wait longer: it ends when the OS returns.
    inner_stop.store(true, Ordering::SeqCst);
    drop(rx);
    let released = Instant::now();
    while !camera_thread.is_finished() && released.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(20));
    }
    if !camera_thread.is_finished() {
        CAMERA_WORKERS.lock().insert(index, camera_thread);
    }
    result
}

/// The camera itself, on its own thread.
fn camera_loop(
    index: u32,
    width: u32,
    height: u32,
    fps: u32,
    stop: &AtomicBool,
    tx: &std::sync::mpsc::SyncSender<CamMsg>,
) -> Result<()> {
    use nokhwa::pixel_format::RgbAFormat;
    use nokhwa::utils::{
        CameraFormat as NkFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
        Resolution,
    };

    let mut cam = nokhwa::Camera::new(
        CameraIndex::Index(index),
        RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
    )
    .map_err(|e| anyhow!("opening the camera: {e}"))?;

    // Prefer formats that convert to I420 without a colour-space round trip.
    let mut chosen = false;
    for ff in [
        FrameFormat::NV12,
        FrameFormat::YUYV,
        FrameFormat::MJPEG,
        FrameFormat::RAWRGB,
    ] {
        let wanted = NkFormat::new(Resolution::new(width, height), ff, fps);
        if cam
            .set_camera_requset(RequestedFormat::new::<RgbAFormat>(
                RequestedFormatType::Exact(wanted),
            ))
            .is_ok()
        {
            chosen = true;
            break;
        }
    }
    if !chosen {
        let _ = cam.set_camera_requset(RequestedFormat::new::<RgbAFormat>(
            RequestedFormatType::Closest(NkFormat::new(
                Resolution::new(width, height),
                FrameFormat::NV12,
                fps,
            )),
        ));
    }
    cam.open_stream()
        .map_err(|e| anyhow!("starting the camera: {e}"))?;
    let fmt = cam.camera_format();
    let _ = tx.send(CamMsg::Info(CaptureInfo {
        width: fmt.width(),
        height: fmt.height(),
        note: format!("camera · {} · {} fps", fmt.format(), fmt.frame_rate()),
    }));

    while !stop.load(Ordering::SeqCst) {
        let frame = cam
            .frame()
            .map_err(|e| anyhow!("reading a camera frame: {e}"))?;
        let res = frame.resolution();
        let (w, h) = (res.width(), res.height());
        let raw = frame.buffer();
        let buf = match frame.source_frame_format() {
            FrameFormat::NV12 if raw.len() >= (w * h * 3 / 2) as usize => nv12_to_i420(raw, w, h),
            FrameFormat::YUYV if raw.len() >= (w * h * 2) as usize => yuyv_to_i420(raw, w, h),
            _ => {
                let rgba = frame
                    .decode_image::<RgbAFormat>()
                    .map_err(|e| anyhow!("decoding: {e}"))?;
                let (w, h) = (rgba.width(), rgba.height());
                let mut buf = I420Buffer::new(w, h);
                let (sy, su, sv) = buf.strides();
                let (y, u, v) = buf.data_mut();
                // Bytes are R,G,B,A in memory, which libyuv calls "ABGR".
                yuv_helper::abgr_to_i420(
                    rgba.as_raw(),
                    w * 4,
                    y,
                    sy,
                    u,
                    su,
                    v,
                    sv,
                    w as i32,
                    h as i32,
                );
                buf
            }
        };
        // A full queue means the publisher is behind: drop this frame.
        match tx.try_send(CamMsg::Frame(buf)) {
            Ok(()) | Err(std::sync::mpsc::TrySendError::Full(_)) => {}
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
        }
    }
    let _ = cam.stop_stream();
    Ok(())
}

fn nv12_to_i420(raw: &[u8], w: u32, h: u32) -> I420Buffer {
    let mut buf = I420Buffer::new(w, h);
    let (sy, su, sv) = buf.strides();
    let (y, u, v) = buf.data_mut();
    let (src_y, src_uv) = raw.split_at((w * h) as usize);
    yuv_helper::nv12_to_i420(src_y, w, src_uv, w, y, sy, u, su, v, sv, w as i32, h as i32);
    buf
}

/// YUYV (Y0 U Y1 V) to I420, taking chroma from even rows.
fn yuyv_to_i420(raw: &[u8], w: u32, h: u32) -> I420Buffer {
    let mut buf = I420Buffer::new(w, h);
    let (sy, su, sv) = buf.strides();
    let (sy, su, sv) = (sy as usize, su as usize, sv as usize);
    let (wu, hu) = (w as usize, h as usize);
    let (y, u, v) = buf.data_mut();
    for row in 0..hu {
        let src = &raw[row * wu * 2..(row + 1) * wu * 2];
        let dst = &mut y[row * sy..row * sy + wu];
        for x in 0..wu {
            dst[x] = src[x * 2];
        }
        if row % 2 == 0 {
            let cr = row / 2;
            for x in 0..wu / 2 {
                u[cr * su + x] = src[x * 4 + 1];
                v[cr * sv + x] = src[x * 4 + 3];
            }
        }
    }
    buf
}

/// Synthetic I420 source. The picture scrolls sideways, so every frame differs.
struct Synthetic {
    w: u32,
    h: u32,
    /// Luma planes to cycle through: one for the pattern, several for detail.
    frames: Vec<Vec<u8>>,
    n: u64,
}

/// Peak-to-peak strength of the per-frame grain, in luma steps.
const DETAIL_GRAIN: u32 = 12;
/// Distinct grain fields; consecutive frames never share one.
const DETAIL_FRAMES: usize = 4;

impl Synthetic {
    fn new(w: u32, h: u32, detail: bool) -> Self {
        let (wu, hu) = (w as usize, h as usize);
        let mut rng = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut base = vec![0u8; wu * hu];
        for yy in 0..hu {
            let row = &mut base[yy * wu..(yy + 1) * wu];
            for (xx, px) in row.iter_mut().enumerate() {
                let grad = ((xx + yy) / 4) as u32 & 0xFF;
                let block = if ((xx / 64) + (yy / 64)) % 2 == 0 {
                    40
                } else {
                    0
                };
                let grain = (next() & 0x0F) as u32;
                *px = ((grad / 2 + block + grain + 16).min(220)) as u8;
            }
        }
        let frames = if detail {
            (0..DETAIL_FRAMES)
                .map(|_| {
                    let mut f = base.clone();
                    for chunk in f.chunks_mut(8) {
                        let mut r = next();
                        for px in chunk {
                            *px = px.saturating_add(((r & 0xFF) as u32 * DETAIL_GRAIN / 256) as u8);
                            r >>= 8;
                        }
                    }
                    f
                })
                .collect()
        } else {
            vec![base]
        };
        Self { w, h, frames, n: 0 }
    }

    fn next_frame(&mut self) -> I420Buffer {
        let (wu, hu) = (self.w as usize, self.h as usize);
        let mut buf = I420Buffer::new(self.w, self.h);
        let (sy, su, sv) = buf.strides();
        let (sy, su, sv) = (sy as usize, su as usize, sv as usize);
        let (cw, chh) = (wu.div_ceil(2), hu.div_ceil(2));
        let (y, u, v) = buf.data_mut();
        let src_frame = &self.frames[(self.n as usize) % self.frames.len()];
        let shift = (self.n as usize * 8) % wu.max(1);
        for r in 0..hu {
            let src = &src_frame[r * wu..(r + 1) * wu];
            let dst = &mut y[r * sy..r * sy + wu];
            dst[..wu - shift].copy_from_slice(&src[shift..]);
            dst[wu - shift..].copy_from_slice(&src[..shift]);
        }
        let phase = (self.n % 256) as u8;
        for r in 0..chh {
            u[r * su..r * su + cw].fill(96u8.wrapping_add(phase / 4));
            v[r * sv..r * sv + cw].fill(160u8.wrapping_sub(phase / 4));
        }
        self.n += 1;
        buf
    }
}

/// `--camera-test`: open the first camera and time each step, with a watchdog
/// so a step that never returns is reported instead of hanging.
pub fn camera_test() -> i32 {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let say = |s: String| {
            let _ = tx.send(format!("{:>6} ms  {s}", t0.elapsed().as_millis()));
        };
        #[cfg(target_os = "macos")]
        say(format!(
            "access already granted: {}",
            nokhwa::nokhwa_check()
        ));
        let cams = match list_cameras() {
            Ok(c) => c,
            Err(e) => return say(format!("list failed: {e:#}")),
        };
        say(format!(
            "cameras: {:?}",
            cams.iter().map(|c| (c.index, &c.name)).collect::<Vec<_>>()
        ));
        let Some(cam) = cams.first() else {
            return say("no camera".into());
        };
        match camera_formats(cam.index) {
            Ok(f) => say(format!("{} formats, first {:?}", f.len(), f.first())),
            Err(e) => return say(format!("formats failed: {e:#}")),
        }
        let stop = AtomicBool::new(false);
        let mut n = 0u32;
        let started = Instant::now();
        let r = run_camera(
            cam.index,
            1280,
            720,
            30,
            &stop,
            |i| say(format!("stream open: {}x{} {}", i.width, i.height, i.note)),
            |_| {
                n += 1;
                if n == 1 || n == 30 {
                    say(format!("frame {n}"));
                }
                if started.elapsed() > Duration::from_secs(3) {
                    stop.store(true, Ordering::SeqCst);
                }
            },
        );
        say(format!("capture ended: {r:?} after {n} frames"));
        say("done".into());
    });
    let mut failed = false;
    loop {
        // 20 s from the last progress line, not from the start.
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(line) => {
                println!("{line}");
                if line.contains("failed") || line.contains("Err(") || line.ends_with("no camera") {
                    failed = true;
                }
                if line.ends_with("done")
                    || line.contains(" failed:")
                    || line.ends_with("no camera")
                {
                    return i32::from(failed);
                }
            }
            Err(_) => {
                println!("HUNG: no progress for 20 s (the last step above never returned)");
                return 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use livekit::webrtc::video_frame::VideoBuffer;

    #[test]
    fn synthetic_frames_have_the_requested_size_and_change() {
        let mut g = Synthetic::new(64, 36, true);
        let a = g.next_frame();
        let b = g.next_frame();
        assert_eq!((a.width(), a.height()), (64, 36));
        assert_ne!(a.data().0, b.data().0, "consecutive frames differ");
    }

    #[test]
    fn yuyv_conversion_keeps_luma() {
        // 2x2 YUYV: Y0 U Y1 V per pair of pixels.
        let raw = [10u8, 100, 20, 200, 30, 110, 40, 210];
        let buf = yuyv_to_i420(&raw, 2, 2);
        let (y, u, v) = buf.data();
        let (sy, _, _) = buf.strides();
        assert_eq!(&y[0..2], &[10, 20]);
        assert_eq!(&y[sy as usize..sy as usize + 2], &[30, 40]);
        assert_eq!(u[0], 100);
        assert_eq!(v[0], 200);
    }
}
