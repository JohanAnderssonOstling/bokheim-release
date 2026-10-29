#!/usr/bin/env bash
set -euo pipefail

APPDIR=${1:?Usage: verify-appdir-compatibility.sh APPDIR [CORE_MAX_GLIBC] [BROWSER_MAX_GLIBC]}
CORE_MAX_GLIBC=${2:-2.17}
BROWSER_MAX_GLIBC=${3:-2.17}
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BROWSER="$APPDIR/usr/bin/bokheim-opds-webview"

test -x "$APPDIR/usr/bin/bokheim"
# Audit the optional system-WebKit executable separately. Its default ceiling
# matches the main app and every bundled library.
bash "$SCRIPT_DIR/verify-glibc-compatibility.sh" "$APPDIR" "$CORE_MAX_GLIBC" --exclude "$BROWSER"
if test -e "$BROWSER"; then
  test -x "$BROWSER"
  bash "$SCRIPT_DIR/verify-glibc-compatibility.sh" "$BROWSER" "$BROWSER_MAX_GLIBC"
else
  printf 'Optional browser helper is not packaged\n'
fi
