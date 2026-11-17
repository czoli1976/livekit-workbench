#!/usr/bin/env bash
# Workbench for LiveKit: get what is needed, build, and run (macOS and Linux).
#
#   scripts/build.sh                       newest LiveKit Rust SDK, build, run
#   scripts/build.sh --sdk-version 0.11.0  pin the SDK version
#   scripts/build.sh --no-run              build only
#   scripts/build.sh --yes                 install missing tools without asking
#
# Options:
#   --sdk-version VER|latest   LiveKit Rust SDK (crate `livekit`) to build with.
#                              Default: latest release on crates.io.
#   --no-server                do not install livekit-server (LiveKit Cloud only)
#   --no-run                   build, but do not start the app
#   --yes                      answer yes to every install prompt
#   --debug                    debug build instead of release
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SDK_VERSION="latest"
RUN=1
SERVER=1
YES=0
PROFILE="release"
TESTED_SDK="0.11.0"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --sdk-version) SDK_VERSION="${2:?--sdk-version needs a value}"; shift 2 ;;
    --sdk-version=*) SDK_VERSION="${1#*=}"; shift ;;
    --no-run) RUN=0; shift ;;
    --no-server) SERVER=0; shift ;;
    --yes|-y) YES=1; shift ;;
    --debug) PROFILE="debug"; shift ;;
    -h|--help) sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "Unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done

say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
confirm() {
  [[ $YES -eq 1 ]] && return 0
  read -r -p "$1 [y/N] " reply
  [[ "$reply" =~ ^[Yy] ]]
}

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
  Darwin) PLATFORM=macos; DATA_DIR="$HOME/Library/Application Support/io.czoli1976.livekit-workbench" ;;
  Linux)  PLATFORM=linux; DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/livekit-workbench" ;;
  *) die "Unsupported OS $OS. On Windows use scripts/build.ps1." ;;
esac
BIN_DIR="$DATA_DIR/bin"

# ---------------------------------------------------------------- toolchain
ensure_rust() {
  if command -v cargo >/dev/null 2>&1; then
    say "Rust $(rustc --version | cut -d' ' -f2) found"
    return
  fi
  [[ -x "$HOME/.cargo/bin/cargo" ]] && { export PATH="$HOME/.cargo/bin:$PATH"; return; }
  confirm "Rust is not installed. Install it with rustup (https://rustup.rs)?" || die "Rust is required."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  export PATH="$HOME/.cargo/bin:$PATH"
}

ensure_platform_deps() {
  if [[ $PLATFORM == macos ]]; then
    xcode-select -p >/dev/null 2>&1 || {
      say "Installing the Xcode command line tools (a system dialog opens)…"
      xcode-select --install || true
      die "Finish the Xcode command line tools install, then run this script again."
    }
    return
  fi
  # Linux: libraries for the window (X11/Wayland, OpenGL), the camera (V4L2)
  # and the WebRTC build.
  local apt_pkgs=(build-essential pkg-config clang libclang-dev curl
    libx11-dev libxext-dev libxrandr-dev libxi-dev libxcursor-dev libxfixes-dev libxdamage-dev libxcomposite-dev libxtst-dev
    libxkbcommon-dev libwayland-dev libgl1-mesa-dev libegl1-mesa-dev libgbm-dev libdrm-dev libva-dev
    libv4l-dev libglib2.0-dev libasound2-dev libpulse-dev libssl-dev libdbus-1-dev)
  local dnf_pkgs=(gcc gcc-c++ make pkgconf-pkg-config clang clang-devel curl
    libX11-devel libXext-devel libXrandr-devel libXi-devel libXcursor-devel libXfixes-devel libXdamage-devel libXcomposite-devel libXtst-devel
    libxkbcommon-devel wayland-devel mesa-libGL-devel mesa-libEGL-devel mesa-libgbm-devel libdrm-devel libva-devel
    libv4l-devel glib2-devel alsa-lib-devel pulseaudio-libs-devel openssl-devel dbus-devel)
  if command -v nvidia-smi >/dev/null 2>&1 && [[ ! -f "${CUDA_HOME:-/usr/local/cuda}/include/cuda.h" ]]; then
    warn "NVIDIA GPU found but no CUDA toolkit: the build will have no NVENC/NVDEC. Install the CUDA toolkit (or set CUDA_HOME) before building to use them."
  fi
  if command -v apt-get >/dev/null 2>&1; then
    local missing=()
    for p in "${apt_pkgs[@]}"; do dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p"); done
    if [[ ${#missing[@]} -gt 0 ]]; then
      confirm "Install ${#missing[@]} system packages with apt (needs sudo): ${missing[*]}?" || die "These packages are required."
      sudo apt-get update && sudo apt-get install -y "${missing[@]}"
    fi
  elif command -v dnf >/dev/null 2>&1; then
    confirm "Install build packages with dnf (needs sudo)?" && sudo dnf install -y "${dnf_pkgs[@]}"
  else
    warn "Unknown package manager. Install the equivalents of: ${apt_pkgs[*]}"
  fi
}

# webrtc-sys rejects the distro clang on Ubuntu 24 (clang 18). clang 21+ is
# required, and CC/CXX have to name it: otherwise the build finds clang++ on PATH.
ensure_clang() {
  [[ $PLATFORM == linux ]] || return 0
  local out status
  if out="$("$ROOT/scripts/install-clang.sh")"; then
    eval "$out"
    say "Using $("$CXX" --version | head -1)"
    return
  else
    status=$?
  fi
  [[ $status -eq 2 ]] || exit "$status"
  confirm "WebRTC needs clang 21 or newer. Install it with apt?" || die "clang 21 or newer is required."
  out="$("$ROOT/scripts/install-clang.sh" --install)"
  eval "$out"
  say "Using $("$CXX" --version | head -1)"
}

# ------------------------------------------------------------ livekit-server
find_server() {
  command -v livekit-server 2>/dev/null && return
  [[ -x "$BIN_DIR/livekit-server" ]] && echo "$BIN_DIR/livekit-server"
  return 0
}

install_server() {
  local found; found="$(find_server)"
  if [[ -n "$found" ]]; then
    say "livekit-server found: $found ($("$found" --version 2>/dev/null | head -1))"
    return
  fi
  confirm "livekit-server (the local dev server) is not installed. Install it?" || {
    warn "Skipped. The Local dev profile needs it; LiveKit Cloud profiles do not."
    return
  }
  if [[ $PLATFORM == macos ]]; then
    command -v brew >/dev/null 2>&1 || die "Homebrew is needed to install livekit-server on macOS (https://brew.sh), or see https://docs.livekit.io/home/self-hosting/local/"
    brew install livekit
    return
  fi
  local arch
  case "$ARCH" in
    x86_64|amd64) arch=amd64 ;;
    aarch64|arm64) arch=arm64 ;;
    armv7l) arch=armv7 ;;
    *) die "No livekit-server build for $ARCH" ;;
  esac
  local tag ver tmp
  tag="$(curl -fsSL https://api.github.com/repos/livekit/livekit/releases/latest | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
  [[ -n "$tag" ]] || die "Could not read the latest livekit-server release from GitHub."
  ver="${tag#v}"
  tmp="$(mktemp -d)"
  local asset="livekit_${ver}_linux_${arch}.tar.gz"
  say "Downloading livekit-server $ver ($asset)"
  curl -fsSL -o "$tmp/$asset" "https://github.com/livekit/livekit/releases/download/$tag/$asset"
  curl -fsSL -o "$tmp/checksums.txt" "https://github.com/livekit/livekit/releases/download/$tag/checksums.txt"
  (cd "$tmp" && grep " $asset\$" checksums.txt | sha256sum -c -) || die "Checksum mismatch for $asset"
  mkdir -p "$BIN_DIR"
  tar -xzf "$tmp/$asset" -C "$tmp"
  install -m 0755 "$tmp/livekit-server" "$BIN_DIR/livekit-server"
  rm -rf "$tmp"
  say "Installed $BIN_DIR/livekit-server (the app finds it there)"
}

# ----------------------------------------------------------------- SDK pin
latest_crate() {
  cargo search "$1" --limit 1 2>/dev/null | sed -n "s/^$1 = \"\([^\"]*\)\".*/\1/p"
}

select_sdk() {
  local want="$SDK_VERSION"
  if [[ "$want" == latest ]]; then
    want="$(latest_crate livekit)"
    [[ -n "$want" ]] || { warn "Could not reach crates.io; keeping the version in Cargo.lock."; return; }
    say "Latest LiveKit Rust SDK on crates.io: $want"
  fi
  local current
  current="$(sed -n '/^name = "livekit"$/{n;s/^version = "\(.*\)"/\1/p;}' Cargo.lock 2>/dev/null | head -1)"
  if [[ "$current" == "$want" ]]; then
    say "Building with LiveKit Rust SDK $want"
    return
  fi
  say "Switching the LiveKit Rust SDK from ${current:-none} to $want"
  cargo add "livekit@=$want" --features rustls-tls-native-roots >/dev/null
  # The server API crate is versioned separately; take its newest release.
  local api; api="$(latest_crate livekit-api)"
  [[ -n "$api" ]] && cargo add "livekit-api@$api" --features rustls-tls-native-roots >/dev/null
  if [[ "$want" != "$TESTED_SDK" ]]; then
    warn "Workbench 1.0 was tested with SDK $TESTED_SDK. If the build fails, run: scripts/build.sh --sdk-version $TESTED_SDK"
  fi
}

# ------------------------------------------------------------------- build
ensure_rust
ensure_platform_deps
ensure_clang
[[ $SERVER -eq 1 ]] && install_server
select_sdk

say "Building ($PROFILE). The first build downloads a prebuilt WebRTC (~300 MB) and takes a few minutes."
if [[ $PROFILE == release ]]; then cargo build --release; else cargo build; fi
EXE="$ROOT/target/$PROFILE/livekit-workbench"
"$EXE" --version | sed 's/^/    /'

if [[ $PLATFORM == macos ]]; then
  "$ROOT/scripts/make-app.sh" "$EXE"
  APP="$ROOT/dist/Workbench for LiveKit.app"
fi

if [[ $RUN -eq 1 ]]; then
  say "Starting Workbench for LiveKit"
  if [[ $PLATFORM == macos ]]; then
    open "$APP"
  else
    exec "$EXE"
  fi
else
  say "Done. Run: ${APP:-$EXE}"
fi
