#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
    printf 'Usage: %s /path/to/KOBOeReader\n' "$0" >&2
    exit 2
fi

DEVICE_ROOT=${1%/}
if [ ! -d "$DEVICE_ROOT/.kobo" ]; then
    printf '%s does not look like a mounted Kobo filesystem (.kobo is missing)\n' "$DEVICE_ROOT" >&2
    exit 1
fi

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ARCHIVE=$("$SCRIPT_DIR/package-kobo.sh")
tar -xzf "$ARCHIVE" -C "$DEVICE_ROOT"
sync

printf 'Installed Bokheim under %s/.adds/bokheim\n' "$DEVICE_ROOT"
printf 'Safely eject the Kobo, then select "Bokheim Kobo" in NickelMenu.\n'
printf 'Place Bokheim library directories in %s/Bokheim/Libraries.\n' "$DEVICE_ROOT"
