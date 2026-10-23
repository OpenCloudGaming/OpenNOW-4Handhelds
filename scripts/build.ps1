# Cross-builds the OpenNOW Rust binaries for the R36S (RK3326, dArkOS) on a Windows host.
#
# Requires: rustup, Python (for the ziglang package), and cargo-zigbuild.
#   rustup target add aarch64-unknown-linux-gnu
#   python -m pip install --user ziglang
#   cargo install cargo-zigbuild --locked
#
# Zig's Windows launcher is a batch file, and cmd.exe splits arguments at spaces.
# rustc also expands an 8.3 short path back to the real folder name, so a project
# path that contains a space cannot hold the compile cache. In that case the cache
# is stored at C:\OpenNOW-4Handhelds. Otherwise the cache is .cache\OpenNOW-4Handhelds.
# The built port is build: OpenNOW.sh next to opennow/.

param(
    [string]$BuildRoot = "",
    [string]$CacheRoot = "",
    [string]$Glibc = "2.36"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($BuildRoot)) {
    $BuildRoot = Join-Path $root "build"
}
$target = "aarch64-unknown-linux-gnu"

function Get-ShortPath([string]$Path) {
    $fso = New-Object -ComObject Scripting.FileSystemObject
    if (-not (Test-Path -LiteralPath $Path)) {
        New-Item -ItemType Directory -Force -Path $Path | Out-Null
    }
    $item = Get-Item -LiteralPath $Path
    if ($item.PSIsContainer) { return $fso.GetFolder($item.FullName).ShortPath }
    return $fso.GetFile($item.FullName).ShortPath
}

$sysroot = Get-ShortPath (rustc --print sysroot)
if ($sysroot.Contains(" ")) { throw "No space-free short path for the Rust sysroot '$sysroot'." }

# The linker sees the real path. A short name is expanded again, so it only
# counts as safe when the full project path has no spaces.
if ([string]::IsNullOrWhiteSpace($CacheRoot)) {
    $projectPath = (Get-Item -LiteralPath $root).FullName
    if ($projectPath.Contains(" ")) {
        $CacheRoot = "C:\OpenNOW-4Handhelds"
        Write-Host "The project path contains a space, so the compile cache cannot be stored in the project."
        Write-Host "The cache will be stored at $CacheRoot"
    } else {
        $CacheRoot = Join-Path $root ".cache\OpenNOW-4Handhelds"
        Write-Host "Compile cache: $CacheRoot"
    }
}
if ($CacheRoot.Contains(" ")) { throw "The compile cache path must not contain spaces: $CacheRoot" }
New-Item -ItemType Directory -Force -Path $CacheRoot | Out-Null
Set-Location -LiteralPath $root

$env:CARGO_TARGET_DIR = Join-Path $CacheRoot "target"
$env:CARGO_ZIGBUILD_CACHE_DIR = Join-Path $CacheRoot "zigbuild"
$env:ZIG_GLOBAL_CACHE_DIR = Join-Path $CacheRoot "zigcache"
$env:ZIG_LOCAL_CACHE_DIR = Join-Path $CacheRoot "zigcache"
$env:CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "--sysroot=$sysroot"

function Invoke-ZigBuild([string]$Manifest, [string[]]$Arguments) {
    cargo zigbuild --release --manifest-path $Manifest --target "$target.$Glibc" @Arguments
    if ($LASTEXITCODE -ne 0) { throw "cargo zigbuild failed for $Manifest" }
}

# dArkOS has no desktop keyring; sessions persist in the core's private session file.
Invoke-ZigBuild (Join-Path $root "opennow-core\Cargo.toml") @("--no-default-features", "--bin", "opennow-core")
Invoke-ZigBuild (Join-Path $root "opennow-streamer\Cargo.toml") @("-p", "opennow-streamer-ffi")
Invoke-ZigBuild (Join-Path $root "opennow-handheld\Cargo.toml") @()

# build/ mirrors an EmulationStation ports directory: OpenNOW.sh plus opennow/.
$dist = $BuildRoot
$out = Join-Path $dist "opennow"
New-Item -ItemType Directory -Force -Path $out | Out-Null
$release = Join-Path $env:CARGO_TARGET_DIR "$target\release"
foreach ($file in @("opennow-handheld", "opennow-core")) {
    Copy-Item (Join-Path $release $file) $out -Force
}
$libs = Join-Path $out "libraries"
New-Item -ItemType Directory -Force -Path $libs | Out-Null
Copy-Item (Join-Path $release "libopennow_streamer_ffi.so") $libs -Force
Get-ChildItem -LiteralPath $out -File | Where-Object { $_.Name -like "*.so*" } | Remove-Item -Force
$mpp = Join-Path $root "third_party"
foreach ($file in @("librockchip_mpp.so.0", "librockchip_mpp.so.1", "librockchip_vpu.so.0", "librockchip_vpu.so.1")) {
    Copy-Item (Join-Path $mpp $file) $libs -Force
}
# Windows editors save the launcher with CR LF. The kernel then looks for /bin/bash
# plus a carriage return and reports "required file not found".
$launcher = [IO.File]::ReadAllText((Join-Path $root "opennow-handheld\OpenNOW.sh")) -replace "`r`n", "`n" -replace "`r", "`n"
if (-not $launcher.EndsWith("`n")) { $launcher += "`n" }
[IO.File]::WriteAllText((Join-Path $dist "OpenNOW.sh"), $launcher, (New-Object System.Text.UTF8Encoding $false))
$config = Join-Path $out "stream.json"
if (-not (Test-Path $config)) {
    Copy-Item (Join-Path $root "opennow-handheld\stream.json") $config
}
python (Join-Path $root "scripts\collect_licenses.py") --root $root --out (Join-Path $out "licenses")
if ($LASTEXITCODE -ne 0) { throw "collecting third-party licenses failed" }
Write-Host "R36S ports folder: $dist"
