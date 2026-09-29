#!/usr/bin/env bash
# Called by local-release.py after the shared test gate.
set -euo pipefail
platform=${1:?platform}
output=${2:?fresh output directory}
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
mkdir -p "$output"
case "$platform" in
  linux)
    python3 shared/pdfium/prepare.py --target x86_64-unknown-linux-gnu
    cargo test --release -p client-platform-native -p client-platform-desktop --features client-platform-native/websocket
    cargo test --release -p linux-update-host
    python3 apps/desktop-gpui/scripts/build.py
    export APPIMAGE_EXTRACT_AND_RUN=1
    RELEASE_DIRECTORY="$output" DESKTOP_BINARY="${CARGO_TARGET_DIR:-$root/target}/release/desktop-gpui" \
      bash apps/desktop-gpui/linux/appimage/build-appimage.sh
    "$output/Bokheim-x86_64.AppImage" --bokheim-update-info > "$output/update-info-linux-x86_64-appimage.json"
    python3 -c 'import json,sys; p=json.load(open(sys.argv[1])); assert p["schema"] > 0 and p["target"] == "linux-x86_64-appimage"' "$output/update-info-linux-x86_64-appimage.json"
    (cd "$output" && sha256sum --check Bokheim-x86_64.AppImage.sha256)
    scratch=$(mktemp -d)
    trap 'rm -r -- "$scratch"' EXIT
    (cd "$scratch" && "$output/Bokheim-x86_64.AppImage" --appimage-extract >/dev/null)
    test -x "$scratch/squashfs-root/usr/bin/desktop-gpui"
    test -f "$scratch/squashfs-root/se.bokheim.Bokheim.desktop"
    test -e "$scratch/squashfs-root/se.bokheim.Bokheim.svg" -o -e "$scratch/squashfs-root/se.bokheim.Bokheim.png"
    ldd "$scratch/squashfs-root/usr/bin/desktop-gpui" > "$scratch/ldd.txt"
    if grep -F 'not found' "$scratch/ldd.txt"; then exit 1; fi
    bash apps/desktop-gpui/linux/appimage/verify-glibc-compatibility.sh "$output/Bokheim-x86_64.AppImage" 2.35
    bash apps/desktop-gpui/linux/appimage/verify-glibc-compatibility.sh "$scratch/squashfs-root" 2.35
    ;;
  android)
    : "${ANDROID_CERT_SHA256:?Production certificate SHA-256 is required}"
    # Gradle stages Rust from the workspace target directory.
    unset CARGO_TARGET_DIR
    cargo check --release -p client-platform-android --target aarch64-linux-android
    gradle -p apps/desktop-gpui/android --no-daemon assembleRelease testReleaseUnitTest
    cp apps/desktop-gpui/android/build/outputs/apk/release/*-release.apk "$output/Bokheim-Android-arm64.apk"
    version=$(python3 -c 'import tomllib; print(tomllib.load(open("apps/desktop-gpui/Cargo.toml", "rb"))["package"]["version"])')
    cargo run --release -p linux-update-host --example release_metadata -- "$version" > "$output/build-compatibility.json"
    python3 apps/desktop-gpui/android/verify-package.py \
      --cert-sha256 "$ANDROID_CERT_SHA256" --version "$version" \
      --apk "$output/Bokheim-Android-arm64.apk" --metadata "$output/build-compatibility.json" \
      --output "$output/update-info-android-aarch64-apk.json"
    if [[ -n "${ANDROID_SERIAL:-}" ]]; then
      gradle -p apps/desktop-gpui/android --no-daemon assembleReleaseAndroidTest
      python3 apps/desktop-gpui/android/verify-release.py \
        --serial "$ANDROID_SERIAL" --cert-sha256 "$ANDROID_CERT_SHA256" \
        --apk "$output/Bokheim-Android-arm64.apk" \
        --test-apk apps/desktop-gpui/android/build/outputs/apk/androidTest/release/*.apk \
        --output "$output/android-device-verification.json"
    fi
    ;;
  web)
    : "${PLAYWRIGHT_MODULE:?Set PLAYWRIGHT_MODULE to the installed playwright/index.mjs}"
    command -v wasm-opt >/dev/null
    command -v wasm-bindgen >/dev/null
    python3 shared/pdfium/prepare.py --target wasm32-unknown-unknown
    cargo check --release -p client-platform-web --target wasm32-unknown-unknown --features web-runtime-tests
    cargo check --release -p web-backend-worker --features web-runtime-tests --example cpu_worker --example sync_coordinator --target wasm32-unknown-unknown
    cargo check --release -p web-backend-worker -p web-gpui --target wasm32-unknown-unknown
    node --test client/platforms/web/tests/signal.mjs
    node apps/web-gpui/scripts/startup.browser-test.mjs
    BOKHEIM_WEB_OUTPUT_DIR="$output" bash apps/web-gpui/scripts/build.sh
    export CHROMIUM_PATH=${CHROMIUM_PATH:-$(node --input-type=module -e 'const {chromium}=await import(process.env.PLAYWRIGHT_MODULE); process.stdout.write(chromium.executablePath());')}
    export WASM_BINDGEN=${WASM_BINDGEN:-$(command -v wasm-bindgen)}
    BOKHEIM_TEST_DIST="$output" node apps/web-gpui/scripts/deployment.browser-test.mjs
    ;;
  *) echo "Unknown local platform: $platform" >&2; exit 2 ;;
esac
