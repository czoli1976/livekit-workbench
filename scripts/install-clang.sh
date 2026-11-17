#!/usr/bin/env bash
# Linux only. webrtc-sys compiles against the libc++ shipped inside libwebrtc,
# which requires clang 21 or newer. Ubuntu 24's clang package is 18, so point
# CC and CXX at clang-21.
#
# Prints `export CC=...` and `export CXX=...` on stdout. With $GITHUB_ENV set,
# also appends those variables for later GitHub Actions steps.
#
#   scripts/install-clang.sh            use clang 21 if it is already installed
#   scripts/install-clang.sh --install  install clang-21 from apt.llvm.org if needed
set -euo pipefail

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[[ "$(uname -s)" == Linux ]] || die "install-clang.sh is for Linux."

INSTALL=0
if [[ "${1:-}" == "--install" ]]; then
  INSTALL=1
elif [[ -n "${1:-}" ]]; then
  die "Unknown option: $1"
fi

clang_major() {
  local bin="$1"
  command -v "$bin" >/dev/null 2>&1 || return 1
  "$bin" -dM -E -x c++ /dev/null 2>/dev/null | sed -n 's/^#define __clang_major__ //p'
}

# $1 is a clang++ binary. Print its clang sibling, or nothing.
clang_cc_for() {
  local cxx="$1" dir base
  dir="$(dirname "$cxx")"
  base="$(basename "$cxx")"
  if [[ "$base" == clang++-* ]]; then
    printf '%s/clang-%s\n' "$dir" "${base#clang++-}"
  elif [[ "$base" == "clang++" && -x "$dir/clang" ]]; then
    printf '%s/clang\n' "$dir"
  fi
}

choose() {
  local candidate major cc resolved
  local -a candidates=()
  [[ -n "${CXX:-}" ]] && candidates+=("$CXX")
  candidates+=(clang++-21 clang++)
  for candidate in "${candidates[@]}"; do
    major="$(clang_major "$candidate" || true)"
    [[ -n "$major" && "$major" -ge 21 ]] || continue
    resolved="$(command -v "$candidate")"
    cc="$(clang_cc_for "$resolved")"
    [[ -n "$cc" && -x "$cc" ]] || cc="$resolved"
    printf '%s\n%s\n' "$cc" "$resolved"
    return 0
  done
  return 1
}

install_clang21() {
  command -v apt-get >/dev/null 2>&1 || die "clang 21 or newer is required. This script can install it with apt; on other distributions install clang 21 and set CC and CXX."
  # shellcheck disable=SC1091
  . /etc/os-release
  [[ -n "${VERSION_CODENAME:-}" ]] || die "Could not read VERSION_CODENAME from /etc/os-release."
  local key="/usr/share/keyrings/apt-llvm-org.gpg"
  local list="/etc/apt/sources.list.d/llvm-21.list"
  say() { printf '==> %s\n' "$*" >&2; }
  say "Installing clang-21 from apt.llvm.org"
  sudo mkdir -p /usr/share/keyrings /etc/apt/sources.list.d
  sudo rm -f "$key"
  curl --fail --location --silent --show-error https://apt.llvm.org/llvm-snapshot.gpg.key \
    | sudo gpg --batch --yes --dearmor -o "$key"
  echo "deb [signed-by=$key] https://apt.llvm.org/${VERSION_CODENAME}/ llvm-toolchain-${VERSION_CODENAME}-21 main" \
    | sudo tee "$list" >/dev/null
  sudo apt-get update
  sudo apt-get install -y clang-21
}

emit() {
  local cc="$1" cxx="$2"
  printf 'export CC=%q\n' "$cc"
  printf 'export CXX=%q\n' "$cxx"
  if [[ -n "${GITHUB_ENV:-}" ]]; then
    printf 'CC=%s\nCXX=%s\n' "$cc" "$cxx" >> "$GITHUB_ENV"
  fi
}

if chosen="$(choose)"; then
  emit "${chosen%%$'\n'*}" "${chosen#*$'\n'}"
  exit 0
fi

if [[ $INSTALL -eq 0 ]]; then
  printf 'WebRTC needs clang 21 or newer, and this system does not have it.\n' >&2
  exit 2
fi

install_clang21
if ! chosen="$(choose)"; then
  die "clang-21 installed, but clang++-21 is still not a clang 21 or newer."
fi
emit "${chosen%%$'\n'*}" "${chosen#*$'\n'}"
