#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)
GPUI_FORK=$(CDPATH= cd -- "$REPO_ROOT/../GPUI-Fork" && pwd)
KOBO_BUILD="$GPUI_FORK/target/gpui-kobo"
TOOLCHAIN="$KOBO_BUILD/toolchain/armv7l-linux-musleabihf-cross"
FBINK_RELEASE="$KOBO_BUILD/fbink-build/FBInk-v1.25.0/Release"
GPUI_PACKAGE="$KOBO_BUILD/package/.adds/gpui-kobo"
OUTPUT="$REPO_ROOT/target/desktop-gpui-kobo"
BUILD_TARGET=${CARGO_TARGET_DIR:-"$REPO_ROOT/target"}
case "$BUILD_TARGET" in /*) ;; *) BUILD_TARGET="$REPO_ROOT/$BUILD_TARGET" ;; esac

# The attached Kobo Libra 2 (N418) uses an NXP i.MX 6SLL: one Cortex-A9
# core with the NEON media engine. Keep the tuning at the Kobo packaging
# boundary so desktop builds and binaries for older Kobo models remain
# unaffected. KOBO_CPU_PROFILE=generic provides an A/B and compatibility
# build without device-specific instructions.
KOBO_CPU_PROFILE=${KOBO_CPU_PROFILE:-libra2}
case "$KOBO_CPU_PROFILE" in
    libra2)
        KOBO_RUSTFLAGS='-C target-cpu=cortex-a9 -C target-feature=+neon'
        KOBO_CFLAGS='-O3 -mcpu=cortex-a9 -mfpu=neon -mfloat-abi=hard'
        ;;
    generic)
        KOBO_RUSTFLAGS=''
        KOBO_CFLAGS='-O3'
        ;;
    *)
        printf 'Unsupported KOBO_CPU_PROFILE: %s\n' "$KOBO_CPU_PROFILE" >&2
        exit 2
        ;;
esac

if [ ! -x "$TOOLCHAIN/bin/armv7l-linux-musleabihf-gcc" ] || [ ! -f "$FBINK_RELEASE/libfbink.a" ]; then
    "$GPUI_FORK/crates/gpui_kobo/scripts/package-kobo.sh" >/dev/null
fi

# This build only runs on developer machines now that the release package is
# published from one, so the job count is no longer pinned to a small CI
# runner. It stays well below the core count: `codegen-units=1` makes each
# rustc a single large process, and the release profile exhausts memory before
# it exhausts cores.
CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-6}

cd "$REPO_ROOT"
FBINK_LIB_DIR="$FBINK_RELEASE" \
RUSTFLAGS="$KOBO_RUSTFLAGS" \
CFLAGS_armv7_unknown_linux_musleabihf="$KOBO_CFLAGS" \
CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER="$TOOLCHAIN/bin/armv7l-linux-musleabihf-gcc" \
CC_armv7_unknown_linux_musleabihf="$TOOLCHAIN/bin/armv7l-linux-musleabihf-gcc" \
AR_armv7_unknown_linux_musleabihf="$TOOLCHAIN/bin/armv7l-linux-musleabihf-ar" \
CARGO_PROFILE_RELEASE_LTO=thin \
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" \
CARGO_PROFILE_RELEASE_DEBUG=0 \
cargo build --release --target-dir "$BUILD_TARGET" --target armv7-unknown-linux-musleabihf \
    -p desktop-gpui --no-default-features --features kobo --bin desktop-gpui-kobo

mkdir -p "$OUTPUT"
STAGE=$(mktemp -d "$OUTPUT/stage.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/.adds/bokheim"
mkdir -p "$APP/third-party-source" "$STAGE/.adds/nm" "$STAGE/Bokheim/Libraries"
cp "$BUILD_TARGET/armv7-unknown-linux-musleabihf/release/desktop-gpui-kobo" "$APP/desktop-gpui-kobo"
cp "$GPUI_PACKAGE/fbink" "$APP/fbink"
cp "$GPUI_PACKAGE/LICENSE-APACHE" "$APP/LICENSE-APACHE"
cp "$GPUI_PACKAGE/LICENSE-FBINK-GPLv3" "$APP/LICENSE-FBINK-GPLv3"
cp "$GPUI_PACKAGE/LICENSE-LILEX-OFL.txt" "$APP/LICENSE-LILEX-OFL.txt"
cp "$REPO_ROOT/vendor/wpa-ctrl/LICENSE" "$APP/LICENSE-WPA-CTRL-BSL-1.0"
cp "$GPUI_PACKAGE/third-party-source/FBInk-v1.25.0.tar.xz" "$APP/third-party-source/"
cp "$REPO_ROOT/apps/desktop-gpui/kobo/run.sh" "$APP/run.sh"
cp "$REPO_ROOT/apps/desktop-gpui/kobo/wifi-power.sh" "$APP/wifi-power.sh"
cp "$REPO_ROOT/apps/desktop-gpui/kobo/run-monitored.sh" "$APP/run-monitored.sh"
cp "$REPO_ROOT/apps/desktop-gpui/kobo/monitor-nickel.sh" "$APP/monitor-nickel.sh"
cp "$REPO_ROOT/apps/desktop-gpui/kobo/probe-kobo-audio.sh" "$APP/probe-kobo-audio.sh"
cp "$REPO_ROOT/apps/desktop-gpui/kobo/nickelmenu" "$STAGE/.adds/nm/desktop-gpui-kobo"
chmod +x "$APP/desktop-gpui-kobo" "$APP/fbink" "$APP/run.sh" "$APP/run-monitored.sh" "$APP/monitor-nickel.sh" "$APP/probe-kobo-audio.sh"
printf 'Bokheim shared GPUI UI for Kobo\nBuilt: %s\nTarget: armv7-unknown-linux-musleabihf\nCPU profile: %s\nRust flags: %s\nC flags: %s\nLTO: thin\nCodegen units: 1\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$KOBO_CPU_PROFILE" "$KOBO_RUSTFLAGS" "$KOBO_CFLAGS" >"$APP/BUILD-INFO.txt"

ARCHIVE="$OUTPUT/desktop-gpui-kobo.tar.gz"
tar -czf "$ARCHIVE" -C "$STAGE" .
printf '%s\n' "$ARCHIVE"
