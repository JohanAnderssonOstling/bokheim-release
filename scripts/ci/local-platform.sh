#!/usr/bin/env bash
# Build packages and verify their compatibility and integrity.
set -euo pipefail
platform=${1:?platform}
output=${2:?fresh output directory}
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
cache_platform=$platform
if [[ "$platform" == linux ]]; then cache_platform=linux-glibc217; fi
CARGO_TARGET_DIR=$(python3 scripts/ci/build_cache.py path "$cache_platform")
export CARGO_TARGET_DIR
mkdir -p "$output"
case "$platform" in
  linux)
    python3 shared/pdfium/prepare.py --target x86_64-unknown-linux-gnu
    sdk_environment=$(mktemp)
    speech_environment=$(mktemp)
    browser_environment=$(mktemp)
    trap 'rm -f -- "$sdk_environment" "$speech_environment" "$browser_environment"' EXIT
    python3 scripts/linux/toolchain.py --env-file "$sdk_environment"
    source "$sdk_environment"
    python3 apps/reader/crates/speech/build_native.py --env-file "$speech_environment"
    source "$speech_environment"
    cargo desktop-app --release --timings
    bash apps/desktop-gpui/linux/appimage/verify-glibc-compatibility.sh "$CARGO_TARGET_DIR/release/bokheim" 2.17
    if [[ "${BOKHEIM_BROWSER:-auto}" != disabled && -z "${BOKHEIM_BROWSER_HELPER:-}" ]]; then
      python3 scripts/linux/browser-toolchain.py --env-file "$browser_environment"
      (
        source "$browser_environment"
        cargo desktop-browser --release --timings
      )
      bash apps/desktop-gpui/linux/appimage/verify-glibc-compatibility.sh "$CARGO_TARGET_DIR/release/bokheim-browser" 2.17
    fi
    rm -f -- "$sdk_environment" "$speech_environment" "$browser_environment"
    trap - EXIT
    python3 apps/reader/crates/speech/prepare.py --voice all --dest "$CARGO_TARGET_DIR/release/tts" --runtime "$(dirname "$SHERPA_ONNX_LIB_DIR")"
    python3 shared/pdfium/prepare.py --target x86_64-unknown-linux-gnu --dest "$CARGO_TARGET_DIR/release/pdfium" --offline
    export APPIMAGE_EXTRACT_AND_RUN=1
    RELEASE_DIRECTORY="$output" DESKTOP_BINARY="${CARGO_TARGET_DIR:-$root/target}/release/bokheim" \
      bash apps/desktop-gpui/linux/appimage/build-appimage.sh
    bash -n "$output/install.sh"
    "$output/Bokheim-x86_64.AppImage" --bokheim-update-info > "$output/update-info-linux-x86_64-appimage.json"
    python3 -c 'import json,sys; p=json.load(open(sys.argv[1])); assert p["schema"] > 0 and p["target"] == "linux-x86_64-appimage"' "$output/update-info-linux-x86_64-appimage.json"
    (cd "$output" && sha256sum --check Bokheim-x86_64.AppImage.sha256)
    scratch=$(mktemp -d)
    trap 'rm -r -- "$scratch"' EXIT
    (cd "$scratch" && "$output/Bokheim-x86_64.AppImage" --appimage-extract >/dev/null)
    test -x "$scratch/squashfs-root/usr/bin/bokheim"
    test -f "$scratch/squashfs-root/se.bokheim.Bokheim.desktop"
    test -e "$scratch/squashfs-root/se.bokheim.Bokheim.svg" -o -e "$scratch/squashfs-root/se.bokheim.Bokheim.png"
    ldd "$scratch/squashfs-root/usr/bin/bokheim" > "$scratch/ldd.txt"
    if grep -F 'not found' "$scratch/ldd.txt"; then exit 1; fi
    bash apps/desktop-gpui/linux/appimage/verify-glibc-compatibility.sh "$output/Bokheim-x86_64.AppImage" 2.17
    bash apps/desktop-gpui/linux/appimage/verify-appdir-compatibility.sh "$scratch/squashfs-root"
    ;;
  android)
    : "${ANDROID_CERT_SHA256:?Production certificate SHA-256 is required}"
    # Cargo and Gradle stage native libraries from the same persistent cache.
    cargo check --release -p client-platform-android --target aarch64-linux-android
    gradle -p apps/desktop-gpui/android --no-daemon assembleRelease
    cp apps/desktop-gpui/android/build/outputs/apk/release/*-release.apk "$output/Bokheim-Android-arm64.apk"
    version=$(python3 -c 'import tomllib; print(tomllib.load(open("apps/desktop-gpui/Cargo.toml", "rb"))["package"]["version"])')
    cargo run --release -p linux-update-host --example release_metadata -- "$version" > "$output/build-compatibility.json"
    python3 apps/desktop-gpui/android/verify-package.py \
      --cert-sha256 "$ANDROID_CERT_SHA256" --version "$version" \
      --apk "$output/Bokheim-Android-arm64.apk" --metadata "$output/build-compatibility.json" \
      --output "$output/update-info-android-aarch64-apk.json"
    ;;
  web)
    command -v brotli >/dev/null
    command -v wasm-opt >/dev/null
    command -v wasm-bindgen >/dev/null
    python3 shared/pdfium/prepare.py --target wasm32-unknown-unknown
    cargo check --release -p client-platform-web --target wasm32-unknown-unknown
    cargo check --release -p web-backend-worker -p web-gpui --target wasm32-unknown-unknown
    BOKHEIM_WEB_OUTPUT_DIR="$output" bash apps/web-gpui/scripts/build.sh
    ;;
  *) echo "Unknown local platform: $platform" >&2; exit 2 ;;
esac
