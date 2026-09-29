#!/bin/sh
set -u

WORKDIR=/mnt/onboard/.adds/bokheim
RESOURCE_LOG="$WORKDIR/bokheim-resource.log"
PREVIOUS_LOG="$WORKDIR/bokheim-resource.previous.log"
SAMPLE_INTERVAL=5
MAX_LOG_BYTES=1048576

if [ -f "$RESOURCE_LOG" ] && [ "$(wc -c <"$RESOURCE_LOG")" -ge "$MAX_LOG_BYTES" ]; then
    mv -f "$RESOURCE_LOG" "$PREVIOUS_LOG"
fi

/bin/sh "$WORKDIR/run.sh" &
LAUNCHER_PID=$!
TICKS_PER_SECOND=$(getconf CLK_TCK 2>/dev/null || echo 100)
START_TIME=$(date +%s)
LAST_TIME=$START_TIME
LAST_TICKS=0
LAST_PID=0
PEAK_RSS_KB=0

find_app_pid() {
    for comm_path in /proc/[0-9]*/comm; do
        [ -r "$comm_path" ] || continue
        if [ "$(cat "$comm_path")" = "desktop-gpui-kobo" ]; then
            pid_path=${comm_path%/comm}
            echo "${pid_path##*/}"
            return
        fi
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
