# Builds the handheld front end and the OpenNOW core for Windows so they can be
# tried on a PC. The result is a folder with opennow-handheld.exe, the
# opennow-core.exe it starts, and the SDL2.dll it loads. Account data is kept
# in a data folder beside them, separate from a desktop OpenNOW install.

param(
    [string]$BuildRoot = (Join-Path $PSScriptRoot "builds\windows"),
    [string]$SdlVersion = "2.32.10"
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$env:CARGO_TARGET_DIR = Join-Path $BuildRoot "target"

cargo build --release --manifest-path (Join-Path $root "opennow-core\Cargo.toml") --bin opennow-core
if ($LASTEXITCODE -ne 0) { throw "cargo build failed for opennow-core" }
cargo build --release --manifest-path (Join-Path $root "opennow-handheld\Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo build failed for opennow-handheld" }

$out = Join-Path $BuildRoot "dist"
New-Item -ItemType Directory -Force -Path $out | Out-Null
foreach ($file in @("opennow-handheld.exe", "opennow-core.exe")) {
    Copy-Item (Join-Path $env:CARGO_TARGET_DIR "release\$file") $out -Force
}

$dll = Join-Path $out "SDL2.dll"
if (-not (Test-Path $dll)) {
    $zip = Join-Path $BuildRoot "SDL2-$SdlVersion-win32-x64.zip"
    $url = "https://github.com/libsdl-org/SDL/releases/download/release-$SdlVersion/SDL2-$SdlVersion-win32-x64.zip"
    Invoke-WebRequest -Uri $url -OutFile $zip
    Expand-Archive -Path $zip -DestinationPath (Join-Path $BuildRoot "sdl2") -Force
    Copy-Item (Join-Path $BuildRoot "sdl2\SDL2.dll") $dll -Force
    Remove-Item -LiteralPath $zip -Force
    Remove-Item -LiteralPath (Join-Path $BuildRoot "sdl2") -Recurse -Force
}
Write-Host "Windows build: $out\opennow-handheld.exe"
