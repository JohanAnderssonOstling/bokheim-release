#!/bin/sh
set -u

if [ "${BOKHEIM_KOBO_LAUNCHER_PROFILE:-0}" = 1 ]; then
    read -r launcher_profile_previous _ < /proc/uptime
fi
launcher_profile_mark() {
    [ "${BOKHEIM_KOBO_LAUNCHER_PROFILE:-0}" = 1 ] || return 0
    read -r launcher_profile_now _ < /proc/uptime
    printf 'KOBO_LAUNCHER_PROFILE component=monitor phase=%s start_uptime_s=%s end_uptime_s=%s\n' \
        "$1" "$launcher_profile_previous" "$launcher_profile_now" >>"$WORKDIR/desktop-gpui-kobo.log"
    launcher_profile_previous=$launcher_profile_now
}

WORKDIR=/mnt/onboard/.adds/bokheim
RESOURCE_LOG="$WORKDIR/bokheim-resource.log"
PREVIOUS_LOG="$WORKDIR/bokheim-resource.previous.log"
SAMPLE_INTERVAL=5
MAX_LOG_BYTES=1048576
launcher_profile_mark shell_setup

# SSH is an optional, separately provisioned developer tool.
if [ -f "$WORKDIR/ssh/start.sh" ]; then
    if /bin/sh "$WORKDIR/ssh/start.sh" >>"$WORKDIR/ssh/start.log" 2>&1; then
        # A one-time setup request starts SSH while leaving Nickel available.
        if [ -f "$WORKDIR/ssh/start-only" ]; then
            rm -f "$WORKDIR/ssh/start-only"
            exit 0
        fi
    else
        printf 'Kobo SSH startup failed; see ssh/start.log\n' >&2
        [ ! -f "$WORKDIR/ssh/start-only" ] || exit 1
    fi
fi
launcher_profile_mark ssh_start

# Resource sampling is a developer diagnostic, not part of ordinary reading.
if [ "${BOKHEIM_KOBO_RESOURCE_MONITOR:-0}" != 1 ]; then
    exec /bin/sh "$WORKDIR/run.sh"
fi

if [ -f "$RESOURCE_LOG" ] && [ "$(wc -c <"$RESOURCE_LOG")" -ge "$MAX_LOG_BYTES" ]; then
    mv -f "$RESOURCE_LOG" "$PREVIOUS_LOG"
fi
launcher_profile_mark resource_log_prepare

/bin/sh "$WORKDIR/run.sh" &
LAUNCHER_PID=$!
launcher_profile_mark launcher_dispatch
TICKS_PER_SECOND=$(getconf CLK_TCK 2>/dev/null || echo 100)
START_TIME=$(date +%s)
LAST_TIME=$START_TIME
LAST_TICKS=0
LAST_PID=0
PEAK_RSS_KB=0

find_app_pid() {
    for comm_path in /proc/[0-9]*/comm; do
        IFS= read -r process_name 2>/dev/null < "$comm_path" || continue
        # Linux task names are limited to 15 characters.
        case "$process_name" in
            desktop-gpui-ko|desktop-gpui-kobo)
                pid_path=${comm_path%/comm}
                echo "${pid_path##*/}"
                return
                ;;
        esac
    done
    echo "$LAUNCHER_PID"
}

{
    echo ""
    echo "=== session_start=$(date) launcher_pid=$LAUNCHER_PID sample_interval_s=$SAMPLE_INTERVAL ==="
    while kill -0 "$LAUNCHER_PID" 2>/dev/null; do
        PID=$(find_app_pid)
        NOW=$(date +%s)
        if [ "$PID" != "$LAST_PID" ]; then
            LAST_PID=$PID
            LAST_TIME=$NOW
            LAST_TICKS=0
            echo "resource_target_pid=$PID"
        fi

        if [ -r "/proc/$PID/stat" ] && [ -r "/proc/$PID/status" ]; then
            CPU_TICKS=$(awk '{ print $14 + $15 }' "/proc/$PID/stat")
            RSS_KB=$(awk '/^VmRSS:/ { print $2 }' "/proc/$PID/status")
            VM_KB=$(awk '/^VmSize:/ { print $2 }' "/proc/$PID/status")
            THREADS=$(awk '/^Threads:/ { print $2 }' "/proc/$PID/status")
            MEM_AVAILABLE_KB=$(awk '/^MemAvailable:/ { print $2 }' /proc/meminfo)
            [ -n "$RSS_KB" ] || RSS_KB=0
            [ -n "$VM_KB" ] || VM_KB=0
            [ -n "$THREADS" ] || THREADS=0
            if [ -z "$MEM_AVAILABLE_KB" ]; then
                MEM_AVAILABLE_KB=$(awk '/^MemFree:/ { print $2 }' /proc/meminfo)
            fi
            if [ "$RSS_KB" -gt "$PEAK_RSS_KB" ]; then
                PEAK_RSS_KB=$RSS_KB
            fi

            INTERVAL=$((NOW - LAST_TIME))
            CPU_PERCENT=0
            if [ "$LAST_TICKS" -gt 0 ] && [ "$INTERVAL" -gt 0 ]; then
                CPU_PERCENT=$(((CPU_TICKS - LAST_TICKS) * 100 / TICKS_PER_SECOND / INTERVAL))
            fi
            echo "resource elapsed_s=$((NOW - START_TIME)) cpu_percent=$CPU_PERCENT cpu_ticks=$CPU_TICKS rss_kb=$RSS_KB vm_kb=$VM_KB threads=$THREADS system_mem_available_kb=$MEM_AVAILABLE_KB"
            LAST_TIME=$NOW
            LAST_TICKS=$CPU_TICKS
        fi
        sleep "$SAMPLE_INTERVAL"
    done

    wait "$LAUNCHER_PID"
    STATUS=$?
    echo "session_end=$(date) exit_status=$STATUS elapsed_s=$(($(date +%s) - START_TIME)) peak_sampled_rss_kb=$PEAK_RSS_KB"
} >>"$RESOURCE_LOG" 2>&1

exit "$STATUS"
