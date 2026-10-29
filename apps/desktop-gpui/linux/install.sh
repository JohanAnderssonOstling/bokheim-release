#!/usr/bin/env bash
# Install the release AppImage for the current user, including desktop integration.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: bash install.sh [--version VERSION | --appimage FILE]

Downloads the latest stable Bokheim AppImage, verifies its SHA-256 checksum,
and installs it with a desktop menu entry. No sudo is needed.

  --version VERSION  Install a specific release, for example 0.1.10 or v0.1.10.
  --appimage FILE    Use a local AppImage and its adjacent FILE.sha256 checksum.
  --help            Show this help.

The AppImage is kept in $XDG_DATA_HOME/bokheim/application (normally
~/.local/share/bokheim/application). The command is ~/.local/bin/bokheim;
BOKHEIM_BIN_DIR can override that directory. Libraries and settings are preserved.
EOF
}

fail() { printf 'Bokheim installation failed: %s\n' "$*" >&2; exit 1; }

version=
source_image=
while (($#)); do
  case "$1" in
    --version|--appimage)
      (($# >= 2)) && [[ -n "$2" ]] || fail "$1 needs a value."
      if [[ "$1" == --version ]]; then version=$2; else source_image=$2; fi
      shift 2
      ;;
    --help|-h) usage; exit 0 ;;
    *) fail "Unknown argument: $1. Run with --help for usage." ;;
  esac
done
[[ -z "$version" || -z "$source_image" ]] || fail 'Use either --version or --appimage.'
[[ $(uname -s) == Linux ]] || fail 'This installer supports Linux.'
case $(uname -m) in
  x86_64|amd64) ;;
  *) fail 'The Linux release currently supports x86-64 only.' ;;
esac
for tool in sha256sum mktemp install cp mv ln awk; do
  command -v "$tool" >/dev/null || fail "Required command is missing: $tool."
done

data_dir=${XDG_DATA_HOME:-"$HOME/.local/share"}
bin_dir=${BOKHEIM_BIN_DIR:-"$HOME/.local/bin"}
for directory in "$data_dir" "$bin_dir"; do
  [[ "$directory" == /* && "$directory" != *$'\n'* && "$directory" != *$'\r'* ]] || fail 'Installation directories must be absolute paths without newlines.'
done
image_dir="$data_dir/bokheim/application"
installed_image="$image_dir/Bokheim.AppImage"
[[ "$installed_image" != *=* ]] || fail 'The desktop launcher cannot use an installation path containing =.'
desktop_dir="$data_dir/applications"
desktop_file="$desktop_dir/se.bokheim.Bokheim.desktop"
launcher="$bin_dir/bokheim"
for destination in "$installed_image" "$desktop_file" "$launcher"; do
  [[ ! -d "$destination" ]] || fail "A directory already exists at $destination."
done
mkdir -p -- "$image_dir"
stage=$(mktemp -d "$image_dir/.install-XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT

if [[ -n "$source_image" ]]; then
  [[ -f "$source_image" ]] || fail "AppImage not found: $source_image."
  [[ -f "$source_image.sha256" ]] || fail "Checksum not found: $source_image.sha256."
  cp -- "$source_image" "$stage/Bokheim.AppImage"
  cp -- "$source_image.sha256" "$stage/checksum"
else
  command -v curl >/dev/null || fail 'curl is required to download Bokheim.'
  release_url=https://github.com/JohanAnderssonOstling/bokheim-release/releases
  if [[ -n "$version" ]]; then
    version=${version#v}
    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'Expected a stable version such as 0.1.10.'
    release_url="$release_url/download/v$version"
  else
    # Resolve latest once so both downloads come from the same release.
    release_url=$(curl --fail --location --silent --show-error --proto '=https' --proto-redir '=https' \
      --output /dev/null --write-out '%{url_effective}' "$release_url/latest")
    [[ "$release_url" =~ ^https://github\.com/JohanAnderssonOstling/bokheim-release/releases/tag/v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'Could not resolve the latest stable release.'
    release_url=${release_url/\/tag\//\/download\/}
  fi
  printf 'Downloading Bokheim from %s\n' "$release_url"
  curl --fail --location --show-error --proto '=https' --proto-redir '=https' \
    --output "$stage/Bokheim.AppImage" "$release_url/Bokheim-x86_64.AppImage"
  curl --fail --location --silent --show-error --proto '=https' --proto-redir '=https' \
    --output "$stage/checksum" "$release_url/Bokheim-x86_64.AppImage.sha256"
fi

expected=$(awk 'NR == 1 {print $1}' "$stage/checksum")
[[ "$expected" =~ ^[[:xdigit:]]{64}$ ]] || fail 'Invalid SHA-256 checksum.'
actual=$(sha256sum < "$stage/Bokheim.AppImage")
[[ "${actual%% *}" == "${expected,,}" ]] || fail 'AppImage checksum does not match; existing installation was kept.'
chmod 755 "$stage/Bokheim.AppImage"
# Extract only the menu icon. This runtime operation needs neither FUSE nor a display.
(cd "$stage" && ./Bokheim.AppImage --appimage-extract 'usr/share/icons/hicolor/512x512/apps/se.bokheim.Bokheim.png' >/dev/null)
icon="$stage/squashfs-root/usr/share/icons/hicolor/512x512/apps/se.bokheim.Bokheim.png"
[[ -s "$icon" ]] || fail 'The AppImage does not contain the Bokheim desktop icon.'

# Escape both the Exec quoting layer and the desktop-entry string layer.
exec_path=${installed_image//\\/\\\\}
exec_path=${exec_path//\"/\\\"}
exec_path=${exec_path//\$/\\\$}
exec_path=${exec_path//\`/\\\`}
exec_path=${exec_path//\\/\\\\}
exec_path=${exec_path//%/%%}
cat > "$stage/se.bokheim.Bokheim.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Bokheim
Comment=Read and organize your books
Exec="$exec_path"
Icon=se.bokheim.Bokheim
Terminal=false
Categories=Office;Viewer;
StartupWMClass=se.bokheim.Bokheim
EOF

# Keep a user's previous command/menu entry when replacing another installation.
for previous in "$launcher" "$desktop_file"; do
  if [[ -e "$previous" || -L "$previous" ]]; then
    if [[ ! -e "$previous.before-install" && ! -L "$previous.before-install" ]]; then
      cp -a -- "$previous" "$previous.before-install"
    fi
  fi
done
mkdir -p -- "$bin_dir" "$desktop_dir"
install -Dm644 "$icon" "$data_dir/icons/hicolor/512x512/apps/se.bokheim.Bokheim.png"
mv -f -- "$stage/Bokheim.AppImage" "$installed_image"
ln -s -- "$installed_image" "$stage/bokheim"
mv -f -- "$stage/bokheim" "$launcher"
chmod 644 "$stage/se.bokheim.Bokheim.desktop"
mv -f -- "$stage/se.bokheim.Bokheim.desktop" "$desktop_file"

if command -v update-desktop-database >/dev/null; then
  update-desktop-database "$desktop_dir" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null; then
  gtk-update-icon-cache --force --ignore-theme-index "$data_dir/icons/hicolor" >/dev/null 2>&1 || true
fi
if command -v kbuildsycoca6 >/dev/null; then
  kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
elif command -v kbuildsycoca5 >/dev/null; then
  kbuildsycoca5 --noincremental >/dev/null 2>&1 || true
fi
printf 'Installed %s\nBokheim is now available in your application menu.\n' "$installed_image"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) printf 'To use the bokheim command in a terminal, add %s to PATH.\n' "$bin_dir" ;;
esac
