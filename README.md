# Workbench for LiveKit

A desktop app for seeing what really happens to video in [LiveKit](https://livekit.io): pick a codec, an encoder (hardware or software), simulcast or SVC, a bitrate cap, and watch every stage of the pipeline, from capture through encode, send and the SFU to the decoder at the far end, with the numbers WebRTC itself reports.

Built in Rust with the [LiveKit Rust SDK](https://github.com/livekit/rust-sdks) and [egui](https://github.com/emilk/egui). Runs on macOS, Windows and Linux.

## What it does

- **Publishes and receives in one app.** It joins a room twice, as a publisher and as a subscriber, so you see your own video come back through the SFU. Connect, publish, unpublish and disconnect are separate steps; *Auto-publish on connect* is on by default.
- **Only offers what your machine can do.** Codecs and encoders come from the SDK at start-up. Options that cannot work are greyed out with the reason, for example SVC with a hardware encoder.
- **Shows what applies live and what needs a republish.** The stock SDK keeps a published track's sender private, so changing the codec, bitrate cap or encoder republishes the track (about 1 s without video). Switching the input and choosing the subscriber's layer apply at once.
- **Pipeline cards with health outlines.** Capture, encode, send, SFU and receive each get a green, amber or red outline. Hover a card for the reason (CPU-limited encoder, still ramping, packet loss, freezes…).
- **Stats & Telemetry window.** Per-layer target vs actual resolution, frame rate and bitrate; encoder implementation; why WebRTC degraded (bandwidth, CPU); one row per subscriber, including browser viewers; transport; an event log; CSV recording; and a reference table of what the Rust SDK sends to the server.
- **Ask optimal resolution.** The app, and optionally browser viewers, tell the SFU the size of the tile the video is shown in. The SFU then forwards the smallest simulcast or SVC layer that fills it, at most every 5 s and only when the size changes.
- **Browser viewer.** One click opens a page that joins the room, plays the video and reports its decoder stats back to the app.
- **Local dev server, managed for you.** With the *Local dev* profile the app starts `livekit-server` itself and shows its CPU, memory, rooms, traffic and log. LiveKit Cloud and self-hosted servers work through profiles; API secrets live in the system credential store (macOS Keychain, Windows Credential Manager, Secret Service on Linux).

## Quick start

The build scripts install what is missing (Rust, platform build tools, `livekit-server`), build the app and start it. Each install asks first; pass `--yes` to skip the questions.

**macOS and Linux**

```bash
git clone https://github.com/czoli1976/livekit-workbench
cd livekit-workbench
scripts/build.sh
```

**Windows** (PowerShell)

```powershell
git clone https://github.com/czoli1976/livekit-workbench
cd livekit-workbench
powershell -ExecutionPolicy Bypass -File scripts\build.ps1
```

The first build downloads a prebuilt WebRTC (about 300 MB) and takes a few minutes.

### Choosing the LiveKit SDK version

By default the scripts build with the **newest** LiveKit Rust SDK on crates.io. To pin a version:

```bash
scripts/build.sh --sdk-version 0.11.0
```

```powershell
scripts\build.ps1 -SdkVersion 0.11.0
```

Version 1.0 was tested with SDK 0.11.0. *About* shows the exact SDK version the app was built with, read from `Cargo.lock` at build time.

### What the scripts install

| | macOS | Linux | Windows |
|---|---|---|---|
| Rust | rustup | rustup | rustup |
| Build tools | Xcode command line tools | apt or dnf packages (X11/Wayland, OpenGL, V4L2, clang) | Visual Studio C++ build tools (winget) |
| livekit-server | Homebrew (`brew install livekit`) | GitHub release, checksum-verified, into the app's data folder | GitHub release, checksum-verified, into the app's data folder |

## Using it

1. Start the app. With the *Local dev* profile the server starts with it; the status line at the bottom shows it running.
2. Pick an input (a camera, or a synthetic source that needs no camera), a codec and an encoder.
3. Press **Connect**. With Auto-publish on, it connects and publishes at once.
4. Watch the pipeline cards and the chart. Open **Stats & Telemetry** for the details.
5. Change settings while publishing. Live settings apply at once; the others collect as pending changes until you press **Apply**.

The synthetic *detailed motion* source changes every pixel every frame, so it needs a realistic bitrate, like a camera. *Moving pattern* is cheap to encode.

### LiveKit Cloud

Add a profile under **Edit profiles** with your project URL, API key and secret, then press **Test connection**. Cloud usage is billed: a 50 Mbps test with two subscribers moves about 45 GB an hour.

### Command line

```bash
livekit-workbench --version     # app and SDK versions
livekit-workbench --caps        # what the SDK reports per encoder backend
livekit-workbench --headless --codec vp9 --encoder sw --layers L3T3_KEY --seconds 20
livekit-workbench --headless --help
```

Headless mode publishes and receives without a window, prints one line of stats per second, and exits 0 when video made the round trip. `--lifecycle` also exercises unpublish, publish, republish and a live source switch; `--ask 320x180` checks that asking for a smaller tile gets a smaller layer.

## Good to know

- **Simulcast** is offered for VP8, H.264 and H.265. With VP9 and AV1, WebRTC does not send separate streams for the simulcast request this SDK makes: VP9 becomes one stream with spatial layers and AV1 a single layer. Use SVC for those codecs, as LiveKit does.
- **SVC** is offered for VP9 and AV1 with a software encoder only. The SDK applies a scalability mode only for those codecs, and its hardware AV1 encoders advertise L1T1 only.
- **On macOS the SDK uses VideoToolbox for H.264 and H.265 only**, to encode and to decode. VP8 and VP9 use libvpx, and AV1 uses libaom to encode and dav1d to decode, even on Macs whose hardware can decode AV1 (M3 and later) or VP9. Asking for a hardware encoder with those codecs still gets the software one, so the app shows them as software only. Browser viewers such as Safari may use the hardware decoders; their rows in Stats show what the browser reports. There is no software H.265 encoder in the SDK, so H.265 always uses VideoToolbox.
- **Video FEC** has no client switch in the SDK. The server enables FlexFEC for publisher to SFU (`rtc.flexfec.upstream_enabled`); the Local dev profile has an option for it.
- **Bitrate ramps up.** WebRTC starts a camera track at no more than 1 Mbps and raises its estimate over about a minute. Publishing as *Screen share* starts near the cap.
- **Disconnect returns at once.** On machines with network security software, closing WebRTC's sockets can take several seconds; the app finishes that in the background.

## Building by hand

```bash
cargo build --release
cargo test
```

On macOS, `scripts/make-app.sh` wraps the binary in `dist/Workbench for LiveKit.app`, which macOS needs to ask for camera access.

## Licence

Apache 2.0, see [LICENSE](LICENSE) and [NOTICE](NOTICE).

## Author

[czoli1976](https://github.com/czoli1976) · [LinkedIn](https://www.linkedin.com/in/ckristianzoli)

Workbench for LiveKit is an independent project and is not affiliated with LiveKit, Inc.
