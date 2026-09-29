#!/bin/sh

WORKDIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
LOG="$WORKDIR/desktop-gpui-kobo.log"
export BOKHEIM_KOBO_EXIT_ACTION_FILE="$WORKDIR/exit-action"
rm -f "$BOKHEIM_KOBO_EXIT_ACTION_FILE"
export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"
export RUST_LOG="${RUST_LOG:-warn,book_startup=debug}"
export BOKHEIM_KOBO_CANVAS_MODE_FILE="${BOKHEIM_KOBO_CANVAS_MODE_FILE:-$WORKDIR/data/canvas-mode}"
export BOKHEIM_WESTERN_SHAPING_BENCHMARK=1
NICKEL_WAS_RUNNING=0
export BOKHEIM_KOBO_WIFI_HELPER="$WORKDIR/wifi-power.sh"
# Read only selected hardware hints while Nickel still exists. Never eval /proc data.
nickel_pid=$(pidof nickel 2>/dev/null | awk '{print $1}')
if [ -n "$nickel_pid" ] && [ -r "/proc/$nickel_pid/environ" ]; then
    wifi_hints=$(tr '\000' '\n' < "/proc/$nickel_pid/environ" | grep -E '^(INTERFACE|WIFI_MODULE|PLATFORM|PRODUCT)=')
    while IFS= read -r hint; do
        case "$hint" in INTERFACE=*|WIFI_MODULE=*|PLATFORM=*|PRODUCT=*) export "$hint" ;; esac
    done <<EOF_WIFI_HINTS
$wifi_hints
EOF_WIFI_HINTS
fi

if pidof nickel >/dev/null 2>&1; then
    NICKEL_WAS_RUNNING=1
fi

restart_nickel() {
    if [ -f "$BOKHEIM_KOBO_EXIT_ACTION_FILE" ]; then
        exit_action=$(head -n 1 "$BOKHEIM_KOBO_EXIT_ACTION_FILE")
        rm -f "$BOKHEIM_KOBO_EXIT_ACTION_FILE"
        case "$exit_action" in
            poweroff|reboot)
                sync
                if /sbin/"$exit_action"; then
                    return
                fi
                printf 'Kobo %s failed; restoring Nickel\n' "$exit_action" >&2
                ;;
        esac
    fi
    if [ "$NICKEL_WAS_RUNNING" -ne 1 ] || pidof nickel >/dev/null 2>&1; then
        return
    fi
    (
        cd /
        export LD_LIBRARY_PATH=/usr/local/Kobo
        /usr/local/Kobo/hindenburg >/dev/null 2>&1 &
        LIBC_FATAL_STDERR_=1 /usr/local/Kobo/nickel \
            -platform kobo >/dev/null 2>&1 &
        if command -v udevadm >/dev/null 2>&1; then
            udevadm trigger >/dev/null 2>&1 &
        fi
    )
}

trap restart_nickel 0 1 2 15

{
    printf '\n[%s] starting Bokheim Kobo\n' "$(date)"
    cd "$WORKDIR" || exit 1
    chmod +x desktop-gpui-kobo fbink

    LOADING_TEXT='Bokheim Kobo
Loading...'
    if ! "$WORKDIR/fbink" -q -c -f -p -m -M -W GC16 -S 3 "$LOADING_TEXT"; then
        printf '[%s] Could not present Bokheim loading screen; continuing startup\n' "$(date)"
    fi

    if [ "$NICKEL_WAS_RUNNING" -eq 1 ]; then
        sync
        killall -TERM nickel hindenburg sickel fickel adobehost foxitpdf iink \
            >/dev/null 2>&1 || true
        sleep 2
    fi

    "$WORKDIR/desktop-gpui-kobo"
    app_status=$?
    printf '[%s] Bokheim Kobo exited: status=%s\n' "$(date)" "$app_status"
    if [ "$app_status" -gt 128 ]; then
        printf '[%s] Possible terminating signal: %s\n' "$(date)" "$((app_status - 128))"
    fi
    sync
    if [ "$app_status" -ne 0 ]; then
        "$WORKDIR/fbink" -q -c -f -p -m -M -h \
            "Bokheim Kobo failed; see desktop-gpui-kobo.log"
        sleep 10
        exit "$app_status"
    fi

    printf '[%s] Bokheim Kobo complete\n' "$(date)"
} >>"$LOG" 2>&1

exit 0
