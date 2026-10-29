#!/bin/sh

# Optional startup profiling uses shell builtins and the monotonic uptime clock.
if [ "${BOKHEIM_KOBO_LAUNCHER_PROFILE:-0}" = 1 ]; then
    read -r launcher_profile_previous _ < /proc/uptime
fi
launcher_profile_mark() {
    [ "${BOKHEIM_KOBO_LAUNCHER_PROFILE:-0}" = 1 ] || return 0
    read -r launcher_profile_now _ < /proc/uptime
    printf 'KOBO_LAUNCHER_PROFILE component=launcher phase=%s start_uptime_s=%s end_uptime_s=%s\n' \
        "$1" "$launcher_profile_previous" "$launcher_profile_now" >>"$LOG"
    launcher_profile_previous=$launcher_profile_now
}

WORKDIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ -f "$WORKDIR/.update/installing" ]; then
    /bin/sh "$WORKDIR/update-recovery.sh" "$WORKDIR" || exit 1
fi
LOG="$WORKDIR/desktop-gpui-kobo.log"
export BOKHEIM_KOBO_EXIT_ACTION_FILE="$WORKDIR/exit-action"
export BOKHEIM_KOBO_CAN_UNINSTALL=1
export BOKHEIM_KOBO_CAN_UPDATE=1
export BOKHEIM_KOBO_INSTALL_ROOT="${WORKDIR%/.adds/bokheim}"
rm -f "$BOKHEIM_KOBO_EXIT_ACTION_FILE"
export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"
export RUST_LOG="${RUST_LOG:-warn,book_startup=debug}"
export BOKHEIM_KOBO_CANVAS_MODE_FILE="${BOKHEIM_KOBO_CANVAS_MODE_FILE:-$WORKDIR/data/canvas-mode}"
NICKEL_WAS_RUNNING=${BOKHEIM_KOBO_RESTORE_NICKEL:-0}
export BOKHEIM_KOBO_WIFI_HELPER="$WORKDIR/wifi-power.sh"
# Read only selected hardware hints while Nickel still exists. Never eval /proc data.
nickel_pid=''
nickel_pids=$(pidof nickel 2>/dev/null)
for nickel_pid in $nickel_pids; do
    NICKEL_WAS_RUNNING=1
    break
done
if [ -n "$nickel_pid" ] && [ -r "/proc/$nickel_pid/environ" ]; then
    wifi_hints=$(tr '\000' '\n' < "/proc/$nickel_pid/environ" | grep -E '^(INTERFACE|WIFI_MODULE|PLATFORM|PRODUCT)=')
    while IFS= read -r hint; do
        case "$hint" in INTERFACE=*|WIFI_MODULE=*|PLATFORM=*|PRODUCT=*) export "$hint" ;; esac
    done <<EOF_WIFI_HINTS
$wifi_hints
EOF_WIFI_HINTS
fi

# Reading known PIDs avoids scanning every process on each shutdown poll.
# Zombies have exited and cannot access the display or supervise Nickel.
firmware_live_pids() {
    firmware_alive=''
    for firmware_pid in $1; do
        if IFS= read -r firmware_stat 2>/dev/null < "/proc/$firmware_pid/stat"; then
            case "${firmware_stat##*) }" in
                Z\ *|X\ *) ;;
                *) firmware_alive="$firmware_alive $firmware_pid" ;;
            esac
        fi
    done
}

# These firmware executable names fit in Linux's comm field. Match all of them
# in one pass, using shell reads rather than a separate pidof lookup per name.
find_firmware_pids() {
    firmware_found=''
    for firmware_comm in /proc/[0-9]*/comm; do
        IFS= read -r firmware_name 2>/dev/null < "$firmware_comm" || continue
        case "$firmware_name" in
            nickel|hindenburg|sickel|fickel|adobehost|foxitpdf|iink)
                firmware_proc=${firmware_comm%/comm}
                firmware_found="$firmware_found ${firmware_proc##*/}"
                ;;
        esac
    done
    firmware_live_pids "$firmware_found"
}

uninstall_bokheim() {
    # Remove only shipped program files. Books, app data, optional developer
    # SSH setup, and other applications' NickelMenu configurations stay intact.
    rm -f "$WORKDIR/desktop-gpui-kobo" "$WORKDIR/taxonomy.sqlite3" \
        "$WORKDIR/run.sh" "$WORKDIR/run-monitored.sh" "$WORKDIR/monitor-nickel.sh" \
        "$WORKDIR/wifi-power.sh" "$WORKDIR/command-timeout.sh" \
        "$WORKDIR/probe-kobo-audio.sh" "$WORKDIR/update-recovery.sh" "$WORKDIR/BUILD-INFO.txt" \
        "$WORKDIR/LICENSE-APACHE" "$WORKDIR/LICENSE-FBINK-GPLv3" \
        "$WORKDIR/LICENSE-LILEX-OFL.txt" "$WORKDIR/LICENSE-WPA-CTRL-BSL-1.0" || return 1
    rm -rf "$WORKDIR/runtime" "$WORKDIR/pdfium" "$WORKDIR/third-party-source" \
        "$WORKDIR/command-shims" "$WORKDIR/nickelmenu-install" "$WORKDIR/nickelmenu-icons" "$WORKDIR/.update" || return 1
    rm -f "$WORKDIR/../nm/desktop-gpui-kobo" "$WORKDIR/fbink"
}

restart_nickel() {
    if [ -f "$BOKHEIM_KOBO_EXIT_ACTION_FILE" ]; then
        exit_action=$(head -n 1 "$BOKHEIM_KOBO_EXIT_ACTION_FILE")
        rm -f "$BOKHEIM_KOBO_EXIT_ACTION_FILE"
        case "$exit_action" in
            uninstall)
                if uninstall_bokheim >>"$LOG" 2>&1; then
                    printf 'Bokheim uninstalled; books and settings kept\n' >>"$LOG"
                else
                    printf 'Bokheim uninstall incomplete; see %s\n' "$LOG" >&2
                fi
                sync
                ;;
            poweroff|reboot)
                sync
                if /sbin/"$exit_action" -f; then
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
launcher_profile_mark shell_setup

{
    printf '\n[%s] starting Bokheim Kobo\n' "$(date)"
    cd "$WORKDIR" || exit 1
    chmod +x desktop-gpui-kobo fbink
    launcher_profile_mark log_and_permissions

    LOADING_TEXT='Bokheim Kobo
Loading...'
    if ! "$WORKDIR/fbink" -q -c -f -p -m -M -W GC16 -S 3 "$LOADING_TEXT"; then
        printf '[%s] Could not present Bokheim loading screen; continuing startup\n' "$(date)"
    fi
    launcher_profile_mark loading_screen

    if [ "$NICKEL_WAS_RUNNING" -eq 1 ]; then
        sync
        launcher_profile_mark sync
        read -r nickel_stop_started _ < /proc/uptime
        # /proc/uptime has two fractional digits. Prefix the fraction with 1
        # so shell arithmetic treats values such as 08 as decimal, not octal.
        nickel_stop_seconds=${nickel_stop_started%.*}
        nickel_stop_fraction=${nickel_stop_started#*.}
        nickel_stop_deadline=$((nickel_stop_seconds * 100 + 1$nickel_stop_fraction - 100 + 500))
        find_firmware_pids
        nickel_remaining=$firmware_alive
        launcher_profile_mark firmware_discover
        for firmware_pid in $nickel_remaining; do
            kill -TERM "$firmware_pid" 2>/dev/null || true
        done
        launcher_profile_mark firmware_terminate
        # Proceed once every firmware process sharing the display or supervising
        # Nickel has exited. A slow shutdown must not leave two UIs running.
        nickel_stop_polls=0
        while :; do
            firmware_live_pids "$nickel_remaining"
            nickel_remaining=$firmware_alive
            launcher_profile_mark firmware_check
            if [ -z "$nickel_remaining" ]; then
                # One final scan catches processes respawned during shutdown.
                # Keep the original deadline if another generation appeared.
                find_firmware_pids
                nickel_remaining=$firmware_alive
                launcher_profile_mark firmware_verify
                [ -n "$nickel_remaining" ] || break
                for firmware_pid in $nickel_remaining; do
                    kill -TERM "$firmware_pid" 2>/dev/null || true
                done
            fi
            read -r nickel_stop_now _ < /proc/uptime
            nickel_stop_seconds=${nickel_stop_now%.*}
            nickel_stop_fraction=${nickel_stop_now#*.}
            nickel_stop_now_cs=$((nickel_stop_seconds * 100 + 1$nickel_stop_fraction - 100))
            if [ "$nickel_stop_now_cs" -ge "$nickel_stop_deadline" ]; then
                printf '[%s] Firmware processes did not exit within five seconds (PIDs: %s); aborting startup\n' "$(date)" "$nickel_remaining"
                exit 1
            fi
            sleep 0.1
            launcher_profile_mark firmware_wait
            nickel_stop_polls=$((nickel_stop_polls + 1))
        done
        read -r nickel_stop_finished _ < /proc/uptime
        printf 'KOBO_LAUNCHER_STARTUP phase=nickel_stopped start_uptime_s=%s end_uptime_s=%s wait_polls=%s\n' \
            "$nickel_stop_started" "$nickel_stop_finished" "$nickel_stop_polls"
    fi
    launcher_profile_mark firmware_shutdown_complete

    # The executable's RPATH selects its private runtime, including PDFium's
    # dependencies. Firmware commands launched by the app must use system libc.
    bluetoothctl_real=$(command -v bluetoothctl 2>/dev/null || true)
    chmod +x "$WORKDIR/command-shims/bluetoothctl" 2>/dev/null || true
    launcher_profile_mark executable_prepare
    PATH="$WORKDIR/command-shims:$PATH" BOKHEIM_KOBO_BLUETOOTHCTL_REAL="$bluetoothctl_real" \
        LD_LIBRARY_PATH= "$WORKDIR/desktop-gpui-kobo"
    app_status=$?
    printf '[%s] Bokheim Kobo exited: status=%s\n' "$(date)" "$app_status"
    if [ "$app_status" -gt 128 ]; then
        printf '[%s] Possible terminating signal: %s\n' "$(date)" "$((app_status - 128))"
    fi
    sync
    if [ -f "$BOKHEIM_KOBO_EXIT_ACTION_FILE" ] && [ "$(head -n 1 "$BOKHEIM_KOBO_EXIT_ACTION_FILE")" = update ]; then
        rm -f "$BOKHEIM_KOBO_EXIT_ACTION_FILE"
        "$WORKDIR/fbink" -q -c -f -p -m -M -W GC16 -S 3 'Updating Bokheim...' || true
        if LD_LIBRARY_PATH= "$WORKDIR/desktop-gpui-kobo" --apply-update; then
            # The new launcher keeps the old session's obligation to restore
            # Nickel when the user later quits Bokheim.
            trap - 0 1 2 15
            export BOKHEIM_KOBO_RESTORE_NICKEL="$NICKEL_WAS_RUNNING"
            exec /bin/sh "$WORKDIR/run.sh"
        fi
        printf 'Kobo update installation failed; restoring the previous program\n'
        /bin/sh "$WORKDIR/update-recovery.sh" "$WORKDIR" || exit 1
        rm -f "$WORKDIR/.update/ready.json"
        printf 'The update could not be installed. Download it again and retry.\n' >"$WORKDIR/.update/error"
        "$WORKDIR/fbink" -q -c -f -p -m -M -W GC16 -S 3 'Update failed. Open Bokheim to try again.' || true
        exit 1
    fi
    # Deployment uses SIGTERM before replacing files. A controlled stop should
    # restore Nickel without a failure screen.
    if [ "$app_status" -ne 0 ] && [ "$app_status" -ne 143 ]; then
        "$WORKDIR/fbink" -q -c -f -p -m -M -h \
            "Bokheim Kobo failed; see desktop-gpui-kobo.log"
        sleep 10
        exit "$app_status"
    fi

    printf '[%s] Bokheim Kobo complete\n' "$(date)"
} >>"$LOG" 2>&1

exit 0
