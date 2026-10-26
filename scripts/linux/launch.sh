#!/bin/sh
set -eu
appdir=$1
binary=$2
shift 2
export BOKHEIM_SYSTEM_LD_LIBRARY_PATH="${BOKHEIM_SYSTEM_LD_LIBRARY_PATH-${LD_LIBRARY_PATH-}}"
# Core libraries use ELF rpaths. Do not leak their paths to external programs.
libraries=$BOKHEIM_SYSTEM_LD_LIBRARY_PATH
# --list performs the loader's symbol/version checks without executing the app.
# Prefer the system C++ runtime so current graphics drivers retain their ABI.
status=0
resolved=$(LC_ALL=C LD_LIBRARY_PATH="$libraries" /lib64/ld-linux-x86-64.so.2 --list "$appdir/usr/bin/$binary" 2>&1) || status=$?
# Some loaders print missing symbol versions but still exit successfully.
case "$resolved" in
    *'not found'*|*'undefined symbol'*) status=1 ;;
esac
if test "$status" -ne 0; then
    libraries="$appdir/usr/lib/bokheim-compat${libraries:+:$libraries}"
fi
export LD_LIBRARY_PATH="$libraries"
exec "$appdir/usr/bin/$binary" "$@"
