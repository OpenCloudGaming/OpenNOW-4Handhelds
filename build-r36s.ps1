# Cross-builds the OpenNOW Rust binaries for the R36S (RK3326, dArkOS) on a Windows host.
#
# Requires: rustup, Python (for the ziglang package), and cargo-zigbuild.
#   rustup target add aarch64-unknown-linux-gnu
#   python -m pip install --user ziglang
#   cargo install cargo-zigbuild --locked
#
# zig splits linker response-file arguments at spaces, and rustc canonicalizes
# dependency paths to their long form, so this folder and the build directory must
# not contain spaces. The Rust sysroot is passed through its 8.3 short path.

param(
    [string]$BuildRoot = (Join-Path $PSScriptRoot "builds\r36s"),
    [string]$Glibc = "2.36"
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$target = "aarch64-unknown-linux-gnu"
foreach ($path in @($root, $BuildRoot)) {
    if ($path.Contains(" ")) { throw "Build paths must not contain spaces: '$path'" }
}

$sysroot = (New-Object -ComObject Scripting.FileSystemObject).GetFolder((rustc --print sysroot)).ShortPath
if ($sysroot.Contains(" ")) { throw "No space-free short path for the Rust sysroot '$sysroot'." }
Set-Location $root

$env:CARGO_TARGET_DIR = Join-Path $BuildRoot "target"
$env:CARGO_ZIGBUILD_CACHE_DIR = Join-Path $BuildRoot "zigbuild"
$env:ZIG_GLOBAL_CACHE_DIR = Join-Path $BuildRoot "zigcache"
$env:ZIG_LOCAL_CACHE_DIR = Join-Path $BuildRoot "zigcache"
$env:CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "--sysroot $sysroot"

function Invoke-ZigBuild([string]$Manifest, [string[]]$Arguments) {
    cargo zigbuild --release --manifest-path $Manifest --target "$target.$Glibc" @Arguments
    if ($LASTEXITCODE -ne 0) { throw "cargo zigbuild failed for $Manifest" }
}

# dArkOS has no desktop keyring; sessions persist in the core's private session file.
Invoke-ZigBuild (Join-Path $root "opennow-core\Cargo.toml") @("--no-default-features", "--bin", "opennow-core")
Invoke-ZigBuild (Join-Path $root "opennow-streamer\Cargo.toml") @("-p", "opennow-streamer-ffi")
Invoke-ZigBuild (Join-Path $root "opennow-handheld\Cargo.toml") @()

# dist/ mirrors an EmulationStation ports directory: OpenNOW.sh plus opennow/.
$dist = Join-Path $BuildRoot "dist"
$out = Join-Path $dist "opennow"
New-Item -ItemType Directory -Force -Path $out | Out-Null
$release = Join-Path $env:CARGO_TARGET_DIR "$target\release"
foreach ($file in @("opennow-handheld", "opennow-core", "libopennow_streamer_ffi.so")) {
    Copy-Item (Join-Path $release $file) $out -Force
}
$mpp = Join-Path $root "third_party"
foreach ($file in @("librockchip_mpp.so.0", "librockchip_mpp.so.1", "librockchip_vpu.so.0", "librockchip_vpu.so.1")) {
    Copy-Item (Join-Path $mpp $file) $out -Force
}
Copy-Item (Join-Path $root "opennow-handheld\OpenNOW.sh") $dist -Force
$config = Join-Path $out "stream.json"
if (-not (Test-Path $config)) {
    Copy-Item (Join-Path $root "opennow-handheld\stream.json") $config
}
Write-Host "R36S ports folder: $dist"
