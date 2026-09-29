#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)
RELEASE_DIRECTORY=${RELEASE_DIRECTORY:-"$REPOSITORY_ROOT/release"}
INSTALLER_BINARY=${INSTALLER_BINARY:-"$REPOSITORY_ROOT/apps/kobo-installer/target/release/bokheim-kobo-installer"}
LINUXDEPLOY=${LINUXDEPLOY:-linuxdeploy-x86_64.AppImage}
APPIMAGETOOL=${APPIMAGETOOL:-appimagetool-x86_64.AppImage}

test -x "$INSTALLER_BINARY"
command -v "$LINUXDEPLOY" >/dev/null 2>&1 || test -x "$LINUXDEPLOY"
command -v "$APPIMAGETOOL" >/dev/null 2>&1 || test -x "$APPIMAGETOOL"

PACKAGE_WORK_DIRECTORY=$(mktemp -d)
trap 'rm -rf "$PACKAGE_WORK_DIRECTORY"' EXIT
APPDIR="$PACKAGE_WORK_DIRECTORY/BokheimKoboInstaller.AppDir"
STAGED_ICON="$PACKAGE_WORK_DIRECTORY/se.bokheim.KoboInstaller.png"
ICON="$REPOSITORY_ROOT/apps/ui/design-tokens/assets/icon/bokheim-512.png"

install -Dm644 "$ICON" "$STAGED_ICON"
NO_STRIP=1 "$LINUXDEPLOY" \
  --appdir "$APPDIR" \
  --executable "$INSTALLER_BINARY" \
  --desktop-file "$SCRIPT_DIR/se.bokheim.KoboInstaller.desktop" \
  --icon-file "$STAGED_ICON" \
  --custom-apprun "$SCRIPT_DIR/AppRun"

mkdir -p "$RELEASE_DIRECTORY"
RELEASE_DIRECTORY=$(cd "$RELEASE_DIRECTORY" && pwd)
OUTPUT_FILE="$RELEASE_DIRECTORY/Bokheim-Kobo-Installer-Linux-x86_64.AppImage"
ARCH=x86_64 "$APPIMAGETOOL" --no-appstream "$APPDIR" "$OUTPUT_FILE"
test -s "$OUTPUT_FILE"
chmod +x "$OUTPUT_FILE"
(
  cd "$RELEASE_DIRECTORY"
  sha256sum "$(basename "$OUTPUT_FILE")" > "$(basename "$OUTPUT_FILE").sha256"
)

printf 'Created %s\n' "$OUTPUT_FILE"
