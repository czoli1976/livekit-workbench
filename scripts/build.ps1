<#
.SYNOPSIS
  Workbench for LiveKit: get what is needed, build, and run (Windows).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\build.ps1
  powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -SdkVersion 0.11.0
  powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -NoRun

.PARAMETER SdkVersion
  LiveKit Rust SDK (crate "livekit") to build with, or "latest" (default).
.PARAMETER NoServer
  Do not install livekit-server (LiveKit Cloud only).
.PARAMETER NoRun
  Build, but do not start the app.
.PARAMETER Yes
  Install missing tools without asking.
.PARAMETER DebugBuild
  Debug build instead of release.
#>
param(
  [string]$SdkVersion = "latest",
  [switch]$NoServer,
  [switch]$NoRun,
  [switch]$Yes,
  [switch]$DebugBuild
)
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root
$TestedSdk = "0.11.0"
$DataDir = Join-Path $env:APPDATA "czoli1976\livekit-workbench\data"
$BinDir = Join-Path $DataDir "bin"

function Say($m) { Write-Host "==> $m" -ForegroundColor Cyan }
function Warn($m) { Write-Host "warning: $m" -ForegroundColor Yellow }
function Confirm($q) {
  if ($Yes) { return $true }
  $r = Read-Host "$q [y/N]"
  return $r -match '^[Yy]'
}

# ---------------------------------------------------------------- toolchain
function Ensure-Rust {
  if (Get-Command cargo -ErrorAction SilentlyContinue) {
    Say ("Rust " + ((rustc --version) -split ' ')[1] + " found"); return
  }
  $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
  if (Test-Path (Join-Path $cargoBin "cargo.exe")) { $env:PATH = "$cargoBin;$env:PATH"; return }
  if (-not (Confirm "Rust is not installed. Install it with rustup (https://rustup.rs)?")) { throw "Rust is required." }
  $installer = Join-Path $env:TEMP "rustup-init.exe"
  Invoke-WebRequest -UseBasicParsing -Uri "https://win.rustup.rs/x86_64" -OutFile $installer
  & $installer -y --profile minimal
  $env:PATH = "$cargoBin;$env:PATH"
}

function Ensure-BuildTools {
  # Rust on Windows needs the MSVC linker and the Windows SDK.
  $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
  $hasVc = $false
  if (Test-Path $vswhere) {
    $hasVc = [bool](& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
  }
  if ($hasVc) { Say "Visual Studio C++ build tools found"; return }
  if (-not (Confirm "The Visual Studio C++ build tools are missing. Install them with winget?")) {
    throw "Install 'Desktop development with C++' from https://visualstudio.microsoft.com/visual-cpp-build-tools/ and run this script again."
  }
  winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-package-agreements --accept-source-agreements `
    --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
}

# ------------------------------------------------------------ livekit-server
function Install-Server {
  $onPath = Get-Command livekit-server -ErrorAction SilentlyContinue
  $local = Join-Path $BinDir "livekit-server.exe"
  if ($onPath) { Say "livekit-server found: $($onPath.Source)"; return }
  if (Test-Path $local) { Say "livekit-server found: $local"; return }
  if (-not (Confirm "livekit-server (the local dev server) is not installed. Install it?")) {
    Warn "Skipped. The Local dev profile needs it; LiveKit Cloud profiles do not."; return
  }
  $rel = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/livekit/livekit/releases/latest"
  $tag = $rel.tag_name; $ver = $tag.TrimStart('v')
  $arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "amd64" }
  $asset = "livekit_${ver}_windows_${arch}.zip"
  $tmp = Join-Path $env:TEMP ("lk-" + [guid]::NewGuid())
  New-Item -ItemType Directory -Path $tmp | Out-Null
  Say "Downloading livekit-server $ver ($asset)"
  $base = "https://github.com/livekit/livekit/releases/download/$tag"
  Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile (Join-Path $tmp $asset)
  Invoke-WebRequest -UseBasicParsing -Uri "$base/checksums.txt" -OutFile (Join-Path $tmp "checksums.txt")
  $expected = (Get-Content (Join-Path $tmp "checksums.txt") | Where-Object { $_ -match " $([regex]::Escape($asset))$" }) -split ' ' | Select-Object -First 1
  $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $asset)).Hash.ToLower()
  if ($expected -ne $actual) { throw "Checksum mismatch for $asset" }
  Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force
  New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
  Copy-Item (Join-Path $tmp "livekit-server.exe") $local -Force
  Remove-Item -Recurse -Force $tmp
  Say "Installed $local (the app finds it there)"
}

# ----------------------------------------------------------------- SDK pin
function Latest-Crate($name) {
  $line = (cargo search $name --limit 1 2>$null | Select-Object -First 1)
  if ($line -match "^$([regex]::Escape($name)) = `"([^`"]+)`"") { return $Matches[1] }
  return $null
}

function Select-Sdk {
  $want = $SdkVersion
  if ($want -eq "latest") {
    $want = Latest-Crate "livekit"
    if (-not $want) { Warn "Could not reach crates.io; keeping the version in Cargo.lock."; return }
    Say "Latest LiveKit Rust SDK on crates.io: $want"
  }
  $lock = Get-Content Cargo.lock -Raw -ErrorAction SilentlyContinue
  $current = $null
  if ($lock -match '(?m)^name = "livekit"\r?\nversion = "([^"]+)"') { $current = $Matches[1] }
  if ($current -eq $want) { Say "Building with LiveKit Rust SDK $want"; return }
  Say "Switching the LiveKit Rust SDK from $current to $want"
  cargo add "livekit@=$want" --features rustls-tls-native-roots | Out-Null
  $api = Latest-Crate "livekit-api"
  if ($api) { cargo add "livekit-api@$api" --features rustls-tls-native-roots | Out-Null }
  if ($want -ne $TestedSdk) {
    Warn "Workbench 2.0 was tested with SDK $TestedSdk. If the build fails, run: scripts\build.ps1 -SdkVersion $TestedSdk"
  }
}

# ------------------------------------------------------------------- build
Ensure-Rust
Ensure-BuildTools
if (-not $NoServer) { Install-Server }
Select-Sdk

$profile = if ($DebugBuild) { "debug" } else { "release" }
Say "Building ($profile). The first build downloads a prebuilt WebRTC (~300 MB) and takes a few minutes."
if ($DebugBuild) { cargo build } else { cargo build --release }
if ($LASTEXITCODE -ne 0) { throw "Build failed." }
$exe = Join-Path $Root "target\$profile\livekit-workbench.exe"

if (-not $NoRun) {
  Say "Starting Workbench for LiveKit"
  Start-Process $exe
} else {
  Say "Done. Run: $exe"
}
