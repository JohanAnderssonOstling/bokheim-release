#!/bin/sh
set -u

WORKDIR=/mnt/onboard/.adds/bokheim
PIDFILE="$WORKDIR/nickel-resource-monitor.pid"
LOG="$WORKDIR/nickel-resource.log"
PREVIOUS_LOG="$WORKDIR/nickel-resource.previous.log"
SAMPLE_INTERVAL=5
MAX_LOG_BYTES=1048576
SCRIPT_PATH="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/$(basename -- "$0")"

start_monitor() {
    if [ -r "$PIDFILE" ]; then
        MONITOR_PID=$(cat "$PIDFILE")
        if kill -0 "$MONITOR_PID" 2>/dev/null; then
            exit 0
        fi
        rm -f "$PIDFILE"
    fi

    "$SCRIPT_PATH" worker </dev/null >/dev/null 2>&1 &
    echo "$!" >"$PIDFILE"
}

stop_monitor() {
    if [ ! -r "$PIDFILE" ]; then
        exit 0
    fi
    MONITOR_PID=$(cat "$PIDFILE")
    kill "$MONITOR_PID" 2>/dev/null || true
    rm -f "$PIDFILE"
}

find_nickel_pid() {
    for comm_path in /proc/[0-9]*/comm; do
        [ -r "$comm_path" ] || continue
        if [ "$(cat "$comm_path")" = "nickel" ]; then
            pid_path=${comm_path%/comm}
            echo "${pid_path##*/}"
            return
        fi
    done
    echo 0
}

run_worker() {
    if [ -f "$LOG" ] && [ "$(wc -c <"$LOG")" -ge "$MAX_LOG_BYTES" ]; then
        mv -f "$LOG" "$PREVIOUS_LOG"
    fi

    TICKS_PER_SECOND=$(getconf CLK_TCK 2>/dev/null || echo 100)
    START_TIME=$(date +%s)
    LAST_TIME=$START_TIME
    LAST_TICKS=0
    LAST_PID=0
    PEAK_RSS_KB=0

    finish_worker() {
        echo "nickel_session_end=$(date) elapsed_s=$(($(date +%s) - START_TIME)) peak_sampled_rss_kb=$PEAK_RSS_KB" >>"$LOG"
        if [ -r "$PIDFILE" ] && [ "$(cat "$PIDFILE")" = "$$" ]; then
            rm -f "$PIDFILE"
        fi
        exit 0
    }
    trap finish_worker INT TERM HUP

    echo "" >>"$LOG"
    echo "=== nickel_session_start=$(date) monitor_pid=$$ sample_interval_s=$SAMPLE_INTERVAL ===" >>"$LOG"

    while :; do
        PID=$(find_nickel_pid)
        NOW=$(date +%s)
        if [ "$PID" != "$LAST_PID" ]; then
            LAST_PID=$PID
            LAST_TIME=$NOW
            LAST_TICKS=0
            echo "nickel_target_pid=$PID" >>"$LOG"
        fi

        if [ "$PID" -gt 0 ] && [ -r "/proc/$PID/stat" ] && [ -r "/proc/$PID/status" ]; then
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
            echo "resource elapsed_s=$((NOW - START_TIME)) cpu_percent=$CPU_PERCENT cpu_ticks=$CPU_TICKS rss_kb=$RSS_KB vm_kb=$VM_KB threads=$THREADS system_mem_available_kb=$MEM_AVAILABLE_KB" >>"$LOG"
            LAST_TIME=$NOW
            LAST_TICKS=$CPU_TICKS
        fi
        sleep "$SAMPLE_INTERVAL"
    done
}

case ${1:-start} in
    start) start_monitor ;;
    stop) stop_monitor ;;
    worker) run_worker ;;
    *) exit 2 ;;
esac
