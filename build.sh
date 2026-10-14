#!/usr/bin/env bash
# Builds the R36S port (RK3326, dArkOS) on a Linux machine.
# The result is builds/r36s/dist: OpenNOW.sh plus opennow/.
#
# Requires rustup, cargo-zigbuild, and Zig:
#   rustup target add aarch64-unknown-linux-gnu
#   python3 -m pip install --user ziglang
#   cargo install cargo-zigbuild --locked

set -euo pipefail

case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
        echo "On Windows, run: powershell -File ./build.ps1" >&2
        exit 1
        ;;
esac

root="$(cd "$(dirname "$0")" && pwd)"
target="aarch64-unknown-linux-gnu"
glibc="2.36"
build_root="${OPENNOW_BUILD_ROOT:-$root/builds/r36s}"
dist="$build_root/dist"
out="$dist/opennow"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$build_root/target}"

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo is not installed. Install rustup, then: rustup target add $target" >&2
    exit 1
fi

zig_target="$target"
if command -v cargo-zigbuild >/dev/null 2>&1; then
    zig_target="${target}.${glibc}"
    build_cmd=(cargo zigbuild)
else
    machine="$(uname -m)"
    if [[ "$machine" != "aarch64" && "$machine" != "arm64" ]]; then
        echo "cargo-zigbuild is required to cross-compile for the R36S." >&2
        echo "  cargo install cargo-zigbuild --locked" >&2
        echo "  python3 -m pip install --user ziglang" >&2
        exit 1
    fi
    build_cmd=(cargo build)
fi

build_one() {
    local manifest="$1"
    shift
    "${build_cmd[@]}" --release --manifest-path "$manifest" --target "$zig_target" "$@"
}

# dArkOS has no desktop keyring; sessions persist in the core's private session file.
build_one "$root/opennow-core/Cargo.toml" --no-default-features --bin opennow-core
build_one "$root/opennow-streamer/Cargo.toml" -p opennow-streamer-ffi
build_one "$root/opennow-handheld/Cargo.toml"

release="$CARGO_TARGET_DIR/$target/release"
mkdir -p "$out/libraries"
cp -f "$release/opennow-handheld" "$release/opennow-core" "$out/"
cp -f "$release/libopennow_streamer_ffi.so" "$out/libraries/"
find "$out" -maxdepth 1 -type f -name '*.so*' -delete
for lib in librockchip_mpp.so.0 librockchip_mpp.so.1 librockchip_vpu.so.0 librockchip_vpu.so.1; do
    cp -f "$root/third_party/$lib" "$out/libraries/"
done
sed 's/\r$//' "$root/opennow-handheld/OpenNOW.sh" > "$dist/OpenNOW.sh"
chmod +x "$dist/OpenNOW.sh" "$out/opennow-handheld" "$out/opennow-core"
if [[ ! -f "$out/stream.json" ]]; then
    cp "$root/opennow-handheld/stream.json" "$out/stream.json"
fi
echo "R36S ports folder: $dist"
