#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd "$SCRIPT_DIR/../../../.." && pwd)
PACKAGE_VERSION=${1:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$REPOSITORY_ROOT/apps/desktop-gpui/Cargo.toml" | head -n 1)}
RELEASE_DIRECTORY=${RELEASE_DIRECTORY:-"$REPOSITORY_ROOT/release"}
DESKTOP_BINARY=${DESKTOP_BINARY:-"$REPOSITORY_ROOT/target/release/desktop-gpui"}
LINUXDEPLOY=${LINUXDEPLOY:-linuxdeploy-x86_64.AppImage}
APPIMAGETOOL=${APPIMAGETOOL:-appimagetool-x86_64.AppImage}

test -n "$PACKAGE_VERSION"
test -x "$DESKTOP_BINARY"
command -v "$LINUXDEPLOY" >/dev/null 2>&1 || test -x "$LINUXDEPLOY"
command -v "$APPIMAGETOOL" >/dev/null 2>&1 || test -x "$APPIMAGETOOL"

PACKAGE_WORK_DIRECTORY=$(mktemp -d)
trap 'rm -rf "$PACKAGE_WORK_DIRECTORY"' EXIT

APPDIR="$PACKAGE_WORK_DIRECTORY/Bokheim.AppDir"
STAGED_ICON="$PACKAGE_WORK_DIRECTORY/se.bokheim.Bokheim.png"
DESKTOP_FILE="$REPOSITORY_ROOT/apps/desktop-gpui/linux/se.bokheim.Bokheim.desktop"
ICON_DIRECTORY="$REPOSITORY_ROOT/apps/ui/design-tokens/assets/icon"

install -Dm644 "$ICON_DIRECTORY/bokheim-512.png" "$STAGED_ICON"
python3 "$REPOSITORY_ROOT/shared/pdfium/prepare.py" --target x86_64-unknown-linux-gnu --dest "$APPDIR/usr/bin/pdfium"

"$LINUXDEPLOY" \
  --appdir "$APPDIR" \
  --executable "$DESKTOP_BINARY" \
  --library "$APPDIR/usr/bin/pdfium/libpdfium.so" \
  --desktop-file "$DESKTOP_FILE" \
  --icon-file "$STAGED_ICON" \
  --custom-apprun "$SCRIPT_DIR/AppRun"

for size in 16 24 32 48 64 128 256 512; do
  install -Dm644 \
    "$ICON_DIRECTORY/bokheim-${size}.png" \
    "$APPDIR/usr/share/icons/hicolor/${size}x${size}/apps/se.bokheim.Bokheim.png"
done
install -Dm644 \
  "$ICON_DIRECTORY/bokheim.svg" \
  "$APPDIR/usr/share/icons/hicolor/scalable/apps/se.bokheim.Bokheim.svg"

mkdir -p "$RELEASE_DIRECTORY"
RELEASE_DIRECTORY=$(cd "$RELEASE_DIRECTORY" && pwd)
OUTPUT_FILE="$RELEASE_DIRECTORY/Bokheim-x86_64.AppImage"

(
  cd "$RELEASE_DIRECTORY"
  ARCH=x86_64 VERSION="$PACKAGE_VERSION" \
    "$APPIMAGETOOL" --no-appstream "$APPDIR" "$(basename "$OUTPUT_FILE")"
)

test -s "$OUTPUT_FILE" || { printf 'AppImage was not created: %s\n' "$OUTPUT_FILE" >&2; exit 1; }
chmod +x "$OUTPUT_FILE"
(cd "$RELEASE_DIRECTORY" && sha256sum "$(basename "$OUTPUT_FILE")" > "$(basename "$OUTPUT_FILE").sha256")

printf 'Created %s\n' "$OUTPUT_FILE"
printf 'Created %s\n' "$OUTPUT_FILE.sha256"
