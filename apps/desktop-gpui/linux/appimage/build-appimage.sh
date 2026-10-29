#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd "$SCRIPT_DIR/../../../.." && pwd)
PACKAGE_VERSION=${1:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$REPOSITORY_ROOT/apps/desktop-gpui/Cargo.toml" | head -n 1)}
RELEASE_DIRECTORY=${RELEASE_DIRECTORY:-"$REPOSITORY_ROOT/release"}
DESKTOP_BINARY=${DESKTOP_BINARY:-"$REPOSITORY_ROOT/target/release/bokheim"}
BROWSER_MODE=${BOKHEIM_BROWSER:-auto}
if test "$BROWSER_MODE" = auto && test -n "${OPDS_BROWSER:-}"; then
  BROWSER_MODE=required
fi
OPDS_BROWSER=${OPDS_BROWSER:-"$(dirname "$DESKTOP_BINARY")/bokheim-opds-webview"}
LINUXDEPLOY=${LINUXDEPLOY:-linuxdeploy-x86_64.AppImage}
APPIMAGETOOL=${APPIMAGETOOL:-appimagetool-x86_64.AppImage}

test -n "$PACKAGE_VERSION"
test -x "$DESKTOP_BINARY"
case "$BROWSER_MODE" in
  auto) ;;
  required) test -x "$OPDS_BROWSER" || { printf 'Browser helper is missing or not executable: %s\n' "$OPDS_BROWSER" >&2; exit 1; } ;;
  disabled) ;;
  *) printf 'Invalid BOKHEIM_BROWSER value: %s (expected auto, required or disabled)\n' "$BROWSER_MODE" >&2; exit 1 ;;
esac
command -v "$LINUXDEPLOY" >/dev/null 2>&1 || test -x "$LINUXDEPLOY"
command -v "$APPIMAGETOOL" >/dev/null 2>&1 || test -x "$APPIMAGETOOL"

PACKAGE_WORK_DIRECTORY=$(mktemp -d)
trap 'rm -rf "$PACKAGE_WORK_DIRECTORY"' EXIT

APPDIR="$PACKAGE_WORK_DIRECTORY/Bokheim.AppDir"
STAGED_ICON="$PACKAGE_WORK_DIRECTORY/se.bokheim.Bokheim.png"
DESKTOP_FILE="$REPOSITORY_ROOT/apps/desktop-gpui/linux/se.bokheim.Bokheim.desktop"
ICON_DIRECTORY="$REPOSITORY_ROOT/apps/ui/design-tokens/assets/icon"

install -Dm644 "$(dirname "$DESKTOP_BINARY")/taxonomy.sqlite3" "$APPDIR/usr/bin/taxonomy.sqlite3"
install -Dm644 "$REPOSITORY_ROOT/LICENSE" "$APPDIR/usr/share/licenses/bokheim/LICENSE"
install -Dm644 "$REPOSITORY_ROOT/COPYRIGHT" "$APPDIR/usr/share/licenses/bokheim/COPYRIGHT"
install -Dm644 "$ICON_DIRECTORY/bokheim-512.png" "$STAGED_ICON"
python3 "$REPOSITORY_ROOT/shared/pdfium/prepare.py" --target x86_64-unknown-linux-gnu --dest "$APPDIR/usr/bin/pdfium"
python3 "$REPOSITORY_ROOT/shared/reader-speech/prepare.py" --voice all --dest "$APPDIR/usr/bin/tts"

source "$REPOSITORY_ROOT/scripts/linux/package-libraries.sh"
"$LINUXDEPLOY" \
  --appdir "$APPDIR" \
  --executable "$DESKTOP_BINARY" \
  --library "$APPDIR/usr/bin/pdfium/libpdfium.so" \
  --desktop-file "$DESKTOP_FILE" \
  --icon-file "$STAGED_ICON" \
  "${LINUX_LIBRARY_OPTIONS[@]}" \
  --custom-apprun "$SCRIPT_DIR/AppRun"
python3 "$REPOSITORY_ROOT/scripts/linux/stage-runtime.py" "$APPDIR"
install -Dm755 "$SCRIPT_DIR/register-desktop.sh" "$APPDIR/usr/libexec/bokheim-register-desktop"
# Give the staged libraries their own relative rpaths, without exporting the
# AppDir library directory to browsers and other external programs.
"$LINUXDEPLOY" --appdir "$APPDIR" --deploy-deps-only "$APPDIR/usr/lib" "${LINUX_LIBRARY_OPTIONS[@]}"

# The webview helper links against the system's WebKitGTK 4.1, so it bypasses
# linuxdeploy: no bundled libraries and no rpath into usr/lib.
if test "$BROWSER_MODE" != disabled && test -x "$OPDS_BROWSER"; then
  install -Dm755 "$OPDS_BROWSER" "$APPDIR/usr/bin/bokheim-opds-webview"
else
  printf 'Packaging without the optional browser helper\n'
fi

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

bash "$SCRIPT_DIR/verify-appdir-compatibility.sh" "$APPDIR"

(
  cd "$RELEASE_DIRECTORY"
  ARCH=x86_64 VERSION="$PACKAGE_VERSION" \
    "$APPIMAGETOOL" --no-appstream "$APPDIR" "$(basename "$OUTPUT_FILE")"
)

test -s "$OUTPUT_FILE" || { printf 'AppImage was not created: %s\n' "$OUTPUT_FILE" >&2; exit 1; }
chmod +x "$OUTPUT_FILE"
bash "$SCRIPT_DIR/verify-glibc-compatibility.sh" "$OUTPUT_FILE" 2.17
(cd "$RELEASE_DIRECTORY" && sha256sum "$(basename "$OUTPUT_FILE")" > "$(basename "$OUTPUT_FILE").sha256")
install -m755 "$SCRIPT_DIR/../install.sh" "$RELEASE_DIRECTORY/install.sh"

printf 'Created %s\n' "$OUTPUT_FILE"
printf 'Created %s\n' "$OUTPUT_FILE.sha256"
printf 'Created %s/install.sh\n' "$RELEASE_DIRECTORY"
