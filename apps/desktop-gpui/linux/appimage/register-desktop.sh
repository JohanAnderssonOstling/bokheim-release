#!/usr/bin/env bash
# Register the running AppImage's location before the backend starts.
set -euo pipefail

appdir=$1
shift
[[ -n ${APPIMAGE:-} && -z ${BOKHEIM_DISABLE_DESKTOP_INTEGRATION:-} && -z ${BOKHEIM_DISABLE_SELF_UPDATE:-} ]] || exit 0
for argument in "$@"; do
  case "$argument" in
    --bokheim-update-info|--bokheim-prepare-update|--bokheim-update-trial) exit 0 ;;
  esac
done

data_dir=${XDG_DATA_HOME:-"$HOME/.local/share"}
bin_dir=${BOKHEIM_BIN_DIR:-"$HOME/.local/bin"}
for directory in "$data_dir" "$bin_dir"; do
  [[ "$directory" == /* && "$directory" != *$'\n'* && "$directory" != *$'\r'* ]] || exit 1
done
source_image=$(readlink -f -- "$APPIMAGE")
[[ -f "$source_image" && "$source_image" != *=* && "$source_image" != *$'\n'* && "$source_image" != *$'\r'* ]] || exit 1
integration_dir="$data_dir/bokheim/desktop-integration"
mkdir -p -- "$integration_dir"
# Serialize registration from different AppImages before application ownership starts.
exec 9> "$integration_dir/desktop-registration.lock"
flock --exclusive --timeout 15 9
stage=$(mktemp -d "$integration_dir/.register-XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT

exec_path=${source_image//\\/\\\\}
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

desktop_dir="$data_dir/applications"
desktop_file="$desktop_dir/se.bokheim.Bokheim.desktop"
launcher="$bin_dir/bokheim"
icon_dir="$data_dir/icons/hicolor/512x512/apps"
icon="$icon_dir/se.bokheim.Bokheim.png"
source_icon="$appdir/usr/share/icons/hicolor/512x512/apps/se.bokheim.Bokheim.png"
for destination in "$desktop_file" "$launcher" "$icon"; do
  [[ ! -d "$destination" ]] || exit 1
done
changed=false
mkdir -p -- "$desktop_dir" "$bin_dir" "$icon_dir"
if ! cmp -s -- "$stage/se.bokheim.Bokheim.desktop" "$desktop_file"; then
  if [[ -f "$desktop_file" && ! -e "$desktop_file.before-install" ]]; then
    cp -a -- "$desktop_file" "$desktop_file.before-install"
  fi
  chmod 644 "$stage/se.bokheim.Bokheim.desktop"
  mv -f -- "$stage/se.bokheim.Bokheim.desktop" "$desktop_file"
  changed=true
fi
if [[ ! -L "$launcher" || $(readlink -- "$launcher") != "$source_image" ]]; then
  if [[ ( -e "$launcher" || -L "$launcher" ) && ! -e "$launcher.before-install" && ! -L "$launcher.before-install" ]]; then
    cp -a -- "$launcher" "$launcher.before-install"
  fi
  ln -s -- "$source_image" "$stage/bokheim"
  mv -f -- "$stage/bokheim" "$launcher"
fi
if ! cmp -s -- "$source_icon" "$icon"; then
  install -m644 "$source_icon" "$stage/icon.png"
  mv -f -- "$stage/icon.png" "$icon"
  changed=true
fi
if [[ "$changed" == true ]]; then
  if command -v update-desktop-database >/dev/null; then
    timeout 5 update-desktop-database "$desktop_dir" >/dev/null 2>&1 || true
  fi
  if command -v gtk-update-icon-cache >/dev/null; then
    timeout 5 gtk-update-icon-cache --force --ignore-theme-index "$data_dir/icons/hicolor" >/dev/null 2>&1 || true
  fi
  if command -v kbuildsycoca6 >/dev/null; then
    timeout 5 kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
  elif command -v kbuildsycoca5 >/dev/null; then
    timeout 5 kbuildsycoca5 --noincremental >/dev/null 2>&1 || true
  fi
fi
