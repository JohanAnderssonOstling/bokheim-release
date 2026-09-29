#!/bin/sh

WORKDIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPORT="$WORKDIR/kobo-audio-probe.txt"

{
    printf 'Kobo audio probe\nDate: %s\n\n' "$(date)"
    printf '%s\n' '=== Audio commands ==='
    for command in aplay bluealsa bluetoothd; do
        printf '%s: ' "$command"
        command -v "$command" 2>&1 || true
    done
    printf '\n%s\n' '=== ALSA cards ==='
    cat /proc/asound/cards 2>&1 || true
    printf '\n%s\n' '=== ALSA devices ==='
    ls -la /dev/snd 2>&1 || true
    printf '\n%s\n' '=== aplay PCMs ==='
    aplay -L 2>&1 || true
    printf '\n%s\n' '=== Bluetooth processes ==='
    ps 2>&1 | grep -E '[b]luetooth|[b]luealsa|[a]play' || true
    printf '\n%s\n' '=== Connected Bluetooth devices ==='
    bluetoothctl devices Connected 2>&1 || true
} >"$REPORT" 2>&1

sync
if [ -x "$WORKDIR/fbink" ]; then
    "$WORKDIR/fbink" -q -c -f -p -m -M -h "Audio probe saved to:\n$REPORT" >/dev/null 2>&1 || true
fi
