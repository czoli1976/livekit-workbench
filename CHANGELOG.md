# Changelog

## 2.0.0

Workbench 2.0 adds audio, network impairment, end-to-end encryption and a
reworked settings panel. Tested with the LiveKit Rust SDK 0.11.0 and
livekit-server 1.13.11.

### New

- **Audio.** Publish a synthetic tone or a microphone next to the video.
  - Opus bitrate from 6 to 510 kbps, DTX and RED.
  - WebRTC's echo cancellation, noise suppression and gain control, on by default.
  - Bitrate, loss, jitter, jitter buffer and concealment for both directions, under the pipeline and in Stats.
- **Network impairment without admin rights.** An in-app TURN relay carries
  both of the app's connections. It adds loss, delay, jitter and a rate cap,
  separately for the publisher's uplink and the subscriber's downlink.
  - Settings change live while connected.
  - Works on macOS, Linux and Windows, with any server.
- **End-to-end encryption** with a shared passphrase, including in the browser
  viewers the app opens.
- **Sources panel.** Camera and microphone sit together at the top of the
  settings, each with its own options that fold out.
- **What the SFU measures.** The Server tab shows the local server's own loss,
  jitter, RTT and reordering in each direction, and its connection quality
  score.
- **Settings are remembered** between launches.
- **Preview before connecting.** The chosen input shows in the local tile
  straight away, and the app starts on the system camera.
- **End-to-end test suite.** `scripts/e2e.sh` runs 16 headless round trips
  through a running server. It covers codecs and layers, the publish
  lifecycle, layer requests, audio, E2EE, impairment, the relay retry and
  live input switching.

### Fixed

- The app could hang when the camera was not allowed or did not respond.
- A failed input ended the whole publish, and later input changes did
  nothing. The track now stays published: pick another input.
- Undocked previews were blurry. Previews now follow the size they are
  drawn at.
- The server's traffic and packet rates read 0 most of the time.
- The Linux build failed to link.

### Changed

- Screen sharing is hidden from the input list for now. The engine still
  supports it, and headless runs can test it with `--source screen`.

### Known issues

- Now and then a connection through the impairment relay does not connect
  on the first try. Before the retry it was about one in twenty. No packet
  passes the relay for that connection; the cause lies between WebRTC's TURN
  client and the relay and is not yet found. The app now gives up on the
  first try after 5 s and tries once more with a new relay. In 60 runs after
  the change the first try never failed; the retry itself is tested by
  forcing the first try to time out.
- Tested on macOS (Apple silicon). Linux and Windows are built and
  unit-tested in CI but not run by hand.
- This release is the source tag; build with `scripts/build.sh` or
  `scripts/build.ps1`. There is no prebuilt app.
