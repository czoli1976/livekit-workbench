#!/usr/bin/env bash
# End-to-end suite (16 cases): each case publishes, goes through a LiveKit server and is
# received back by the app (headless), and passes when media made the trip.
#
#   scripts/e2e.sh [path/to/livekit-workbench]
#
# Needs a running LiveKit server: LIVEKIT_URL (default ws://127.0.0.1:7880),
# LIVEKIT_API_KEY / LIVEKIT_API_SECRET (default the dev key), for example
# `livekit-server --dev`. The codec cases assume macOS hardware encoders
# (VideoToolbox); on other machines they report what the host supports.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
B="${1:-$ROOT/target/release/livekit-workbench}"
[[ -x "$B" ]] || { echo "No binary at $B; build first (cargo build --release)." >&2; exit 2; }
export RUST_LOG=off
pass=0; fail=0
run() {
  local name="$1"; shift
  local out status
  out=$("$B" --headless "$@" 2>&1)
  status=$?
  if [[ $status -eq 0 ]] && grep -q "RESULT: ok" <<<"$out"; then
    printf "PASS %s\n" "$name"; pass=$((pass + 1))
  else
    printf "FAIL %s\n" "$name"; fail=$((fail + 1))
    grep -iE "FAILED|error|audio:" <<<"$out" | head -4 | sed 's/^/     /'
  fi
}
run "H.264 hardware, single layer"   --codec h264 --encoder hw --seconds 8
run "H.264 simulcast"                --codec h264 --layers simulcast --seconds 8
run "H.265 hardware"                 --codec h265 --encoder hw --seconds 8
run "VP8 simulcast"                  --codec vp8 --layers simulcast --seconds 8
run "VP9 SVC L3T3_KEY"               --codec vp9 --encoder sw --layers L3T3_KEY --seconds 10
run "AV1 SVC L3T3_KEY"               --codec av1 --encoder sw --layers L3T3_KEY --seconds 10
run "Unpublish, publish, republish"  --lifecycle --seconds 6
run "Ask for a 320x180 layer"        --layers simulcast --ask 320x180 --seconds 6
run "Audio, WebRTC processing on"    --audio tone --seconds 8
run "Audio 256 kbps, processing off" --audio tone --audio-kbps 256 --no-aec --no-ns --no-agc --seconds 8
run "End-to-end encryption"          --e2ee e2e-test-pass --audio tone --seconds 8
run "Uplink 3% loss, 40 ms"          --up 3,40,10 --layers simulcast --audio tone --seconds 12
run "Uplink 40 ms delay"             --up 0,40 --seconds 8
run "Downlink 1.5 Mbps cap"          --down 0,0,0,1500 --layers simulcast --seconds 15
WB_RELAY_FIRST_TRY_MS=50 \
run "Relay connect retry (forced)"  --up 0 --seconds 6
run "Live input switch"              --source pattern --switch detail,pattern --seconds 4
echo "TOTAL: $pass passed, $fail failed"
[[ $fail -eq 0 ]]
