#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/../../.." && pwd)
temp_root=${TMPDIR:-/tmp}
stress_tmp=$(mktemp -d "$temp_root/bokheim-server-stress.XXXXXX")
postgres_data="$stress_tmp/postgres"
postgres_socket=$(mktemp -d /tmp/bokheim-stress-socket.XXXXXX)
postgres_started=0
server_pid=
monitor_pid=
monitor_stop=

cleanup() {
    local exit_code=$?
    if [[ -n "$monitor_pid" ]]; then
        touch "$monitor_stop"
        wait "$monitor_pid" >/dev/null 2>&1 || true
    fi
    if [[ -n "$server_pid" ]]; then
        kill "$server_pid" >/dev/null 2>&1 || true
        wait "$server_pid" >/dev/null 2>&1 || true
    fi
    if [[ "$postgres_started" == 1 ]]; then
        pg_ctl -D "$postgres_data" -m immediate -w stop >/dev/null 2>&1 || true
    fi
    if [[ "$exit_code" != 0 && -f "$stress_tmp/server.log" ]]; then
        echo "sync server log after stress failure:" >&2
        sed -n '1,240p' "$stress_tmp/server.log" >&2
    fi
    if [[ "$stress_tmp" == "$temp_root"/bokheim-server-stress.* ]]; then
        rm -rf -- "$stress_tmp"
    fi
    if [[ "$postgres_socket" == /tmp/bokheim-stress-socket.* ]]; then
        rm -rf -- "$postgres_socket"
    fi
}
trap cleanup EXIT

monitor_server_processes() {
    local stop_file=$1
    local output_file=$2
    local postgres_pid=$3
    local clock_ticks
    clock_ticks=$(getconf CLK_TCK)
    local started_ns
    started_ns=$(date +%s%N)
    local server_peak_kib=0
    local postgres_peak_kib=0
    local samples=0
    declare -A first_ticks=()
    declare -A last_ticks=()
    declare -A process_kind=()
    while [[ ! -e "$stop_file" ]]; do
        local server_rss=0
        local postgres_rss=0
        local postgres_children
        postgres_children=$(ps -eo pid=,ppid= | awk -v parent="$postgres_pid" '$2 == parent { print $1 }')
        for process in $server_pid $postgres_pid $postgres_children; do
            [[ -r "/proc/$process/stat" ]] || continue
            local ticks rss kind
            ticks=$(awk '{ print $14 + $15 }' "/proc/$process/stat")
            if [[ -r "/proc/$process/smaps_rollup" ]]; then
                rss=$(awk '/^Pss:/ { print $2 }' "/proc/$process/smaps_rollup")
            else
                rss=$(awk '/^VmRSS:/ { print $2 }' "/proc/$process/status")
            fi
            rss=${rss:-0}
            if [[ -z "${first_ticks[$process]+present}" ]]; then
                first_ticks[$process]=$ticks
            fi
            last_ticks[$process]=$ticks
            if [[ "$process" == "$server_pid" ]]; then
                kind=server
                server_rss=$rss
            else
                kind=postgres
                postgres_rss=$((postgres_rss + rss))
            fi
            process_kind[$process]=$kind
        done
        (( server_rss > server_peak_kib )) && server_peak_kib=$server_rss
        (( postgres_rss > postgres_peak_kib )) && postgres_peak_kib=$postgres_rss
        samples=$((samples + 1))
        sleep 0.05
    done
    local finished_ns
    finished_ns=$(date +%s%N)
    local server_ticks=0
    local postgres_ticks=0
    for process in "${!last_ticks[@]}"; do
        local delta=$((last_ticks[$process] - first_ticks[$process]))
        if [[ "${process_kind[$process]}" == server ]]; then
            server_ticks=$((server_ticks + delta))
        else
            postgres_ticks=$((postgres_ticks + delta))
        fi
    done
    awk -v duration_ns="$((finished_ns - started_ns))" -v ticks="$clock_ticks" -v server_ticks="$server_ticks" -v postgres_ticks="$postgres_ticks" \
        -v server_rss="$server_peak_kib" -v postgres_rss="$postgres_peak_kib" -v samples="$samples" \
        'BEGIN {
            seconds=duration_ns/1000000000;
            server_cpu=server_ticks/ticks;
            postgres_cpu=postgres_ticks/ticks;
            printf "resource sample: wall=%.3fs samples=%d server_cpu=%.3fs server_average_cores=%.2f server_peak_pss=%.1fMiB postgres_cpu=%.3fs postgres_average_cores=%.2f postgres_peak_pss=%.1fMiB\n", seconds, samples, server_cpu, server_cpu/seconds, server_rss/1024, postgres_cpu, postgres_cpu/seconds, postgres_rss/1024;
        }' >"$output_file"
}

for program in initdb pg_ctl createdb curl cargo psql; do
    command -v "$program" >/dev/null || {
        echo "required program is not installed: $program" >&2
        exit 1
    }
done

initdb -D "$postgres_data" -A trust -U postgres --no-locale >/dev/null
postgres_port=$((50000 + ($$ % 5000)))
while (echo >/dev/tcp/127.0.0.1/"$postgres_port") >/dev/null 2>&1; do
    postgres_port=$((postgres_port + 1))
done
postgres_options="-h 127.0.0.1 -p $postgres_port -k $postgres_socket"
case "${BOKHEIM_STRESS_POSTGRES_DURABLE:-0}" in
    0) postgres_options="-F $postgres_options" ;;
    1) ;;
    *)
        echo "BOKHEIM_STRESS_POSTGRES_DURABLE must be 0 or 1" >&2
        exit 1
        ;;
esac
if ! pg_ctl -D "$postgres_data" -l "$stress_tmp/postgres.log" -o "$postgres_options" -w start >/dev/null; then
    sed -n '1,200p' "$stress_tmp/postgres.log" >&2
    exit 1
fi
postgres_started=1

database_name="server_stress_$$"
createdb -h 127.0.0.1 -p "$postgres_port" -U postgres "$database_name"

server_port=$((55000 + ($$ % 5000)))
while (echo >/dev/tcp/127.0.0.1/"$server_port") >/dev/null 2>&1; do
    server_port=$((server_port + 1))
done

server_target=${BOKHEIM_STRESS_TARGET_DIR:-$repo_root/servers/sync/target}
cargo build --release --manifest-path "$repo_root/servers/sync/Cargo.toml" --bin sync-server --example sync_stress --target-dir "$server_target"

DATABASE_URL="postgres://postgres@127.0.0.1:$postgres_port/$database_name" \
"$server_target/release/sync-server" migrate

stress_email="server-stress@example.com"
stress_password="server-stress-user-password"
mkdir -p "$stress_tmp/assets/books" "$stress_tmp/assets/thumbnails"
DATABASE_URL="postgres://postgres@127.0.0.1:$postgres_port/$database_name" \
SYNC_SERVER_ADDR="127.0.0.1:$server_port" \
BOKHEIM_ASSETS_DIR="$stress_tmp/assets" \
BOKHEIM_ACCOUNT_TOKEN_KEY="WlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlo=" \
BOKHEIM_TEST_USER_EMAIL="$stress_email" \
BOKHEIM_TEST_USER_PASSWORD="$stress_password" \
RUST_LOG=warn \
"$server_target/release/sync-server" >"$stress_tmp/server.log" 2>&1 &
server_pid=$!

ready=0
for _ in $(seq 1 100); do
    if curl --fail --silent "http://127.0.0.1:$server_port/api/ready" >/dev/null; then
        ready=1
        break
    fi
    if ! kill -0 "$server_pid" >/dev/null 2>&1; then
        break
    fi
    sleep 0.1
done
if [[ "$ready" != 1 ]]; then
    sed -n '1,240p' "$stress_tmp/server.log" >&2
    exit 1
fi

verification_response=$(BOKHEIM_STRESS_URL="http://127.0.0.1:$server_port/" \
    BOKHEIM_STRESS_EMAIL="$stress_email" \
    BOKHEIM_STRESS_PASSWORD="$stress_password" \
    BOKHEIM_STRESS_MODE=token \
    "$server_target/release/examples/sync_stress")
shared_token=$(sed -n 's/^BOKHEIM_STRESS_TOKEN=//p' <<<"$verification_response")
if [[ -z "$shared_token" ]]; then
    echo "failed to extract the stress account bearer token" >&2
    exit 1
fi

run_stress_client() {
    BOKHEIM_STRESS_URL="http://127.0.0.1:$server_port/" \
    BOKHEIM_STRESS_TOKEN="$shared_token" \
    BOKHEIM_STRESS_WORKERS="${BOKHEIM_STRESS_WORKERS:-16}" \
    BOKHEIM_STRESS_REQUESTS_PER_WORKER="${BOKHEIM_STRESS_REQUESTS_PER_WORKER:-50}" \
    BOKHEIM_STRESS_EVENTS_PER_REQUEST="${BOKHEIM_STRESS_EVENTS_PER_REQUEST:-4}" \
    BOKHEIM_STRESS_BLOB_BYTES="${BOKHEIM_STRESS_BLOB_BYTES:-1048576}" \
    BOKHEIM_STRESS_SERVER_PID="$server_pid" \
    BOKHEIM_STRESS_POSTGRES_PID="$(sed -n '1p' "$postgres_data/postmaster.pid")" \
    "$server_target/release/examples/sync_stress"
}

ingest_clients=${BOKHEIM_STRESS_INGEST_CLIENTS:-1}
if [[ "$ingest_clients" =~ ^[0-9]+$ ]] && (( ingest_clients >= 1 && ingest_clients <= 128 )); then
    :
else
    echo "BOKHEIM_STRESS_INGEST_CLIENTS must be between 1 and 128" >&2
    exit 1
fi
collect_metrics=${BOKHEIM_STRESS_COLLECT_METRICS:-0}
case "$collect_metrics" in
    0|1) ;;
    *)
        echo "BOKHEIM_STRESS_COLLECT_METRICS must be 0 or 1" >&2
        exit 1
        ;;
esac
if [[ "$collect_metrics" == 1 ]]; then
    IFS='|' read -r wal_before commits_before reads_before hits_before temp_before inserted_before updated_before deleted_before < <(
        psql -h 127.0.0.1 -p "$postgres_port" -U postgres -d "$database_name" -At -F '|' -c \
            "SELECT (SELECT wal_bytes::BIGINT FROM pg_stat_wal),xact_commit,blks_read,blks_hit,temp_bytes,tup_inserted,tup_updated,tup_deleted FROM pg_stat_database WHERE datname=current_database()"
    )
    monitor_stop="$stress_tmp/monitor.stop"
    monitor_output="$stress_tmp/monitor.out"
    postgres_pid=$(sed -n '1p' "$postgres_data/postmaster.pid")
    monitor_server_processes "$monitor_stop" "$monitor_output" "$postgres_pid" &
    monitor_pid=$!
fi

workload_started_ns=$(date +%s%N)
if (( ingest_clients == 1 )); then
    run_stress_client
else
    client_pids=()
    for client_index in $(seq 1 "$ingest_clients"); do
        run_stress_client >"$stress_tmp/client-$client_index.log" 2>&1 &
        client_pids+=("$!")
    done
    client_failed=0
    for client_pid in "${client_pids[@]}"; do
        if ! wait "$client_pid"; then
            client_failed=1
        fi
    done
    for client_index in $(seq 1 "$ingest_clients"); do
        sed "s/^/client $client_index: /" "$stress_tmp/client-$client_index.log"
    done
    if (( client_failed != 0 )); then
        exit 1
    fi
fi
workload_finished_ns=$(date +%s%N)

if (( ingest_clients > 1 )) && [[ "${BOKHEIM_STRESS_MODE:-full}" == ingest ]]; then
    ingest_books=${BOKHEIM_INGEST_BOOKS:-1095}
    ingest_directories=${BOKHEIM_INGEST_DIRECTORIES:-153}
    aggregate_mutations=$((ingest_clients * (ingest_directories + ingest_books * 2)))
    awk -v clients="$ingest_clients" -v mutations="$aggregate_mutations" -v duration_ns="$((workload_finished_ns - workload_started_ns))" \
        'BEGIN { seconds=duration_ns/1000000000; printf "concurrent library ingest: clients=%d mutations=%d throughput=%.0f/s wall=%.3fs\n", clients, mutations, mutations/seconds, seconds }'
fi

if [[ "$collect_metrics" == 1 ]]; then
    touch "$monitor_stop"
    wait "$monitor_pid"
    monitor_pid=
    cat "$monitor_output"
    # PostgreSQL publishes cumulative backend and WAL statistics asynchronously.
    # Wait for the collector flush so short stress runs do not report zero work.
    sleep 1.1
    IFS='|' read -r wal_after commits_after reads_after hits_after temp_after inserted_after updated_after deleted_after < <(
        psql -h 127.0.0.1 -p "$postgres_port" -U postgres -d "$database_name" -At -F '|' -c \
            "SELECT (SELECT wal_bytes::BIGINT FROM pg_stat_wal),xact_commit,blks_read,blks_hit,temp_bytes,tup_inserted,tup_updated,tup_deleted FROM pg_stat_database WHERE datname=current_database()"
    )
    echo "postgres deltas: wal_bytes=$((wal_after - wal_before)) commits=$((commits_after - commits_before)) blocks_read=$((reads_after - reads_before)) blocks_hit=$((hits_after - hits_before)) temp_bytes=$((temp_after - temp_before)) tuples_inserted=$((inserted_after - inserted_before)) tuples_updated=$((updated_after - updated_before)) tuples_deleted=$((deleted_after - deleted_before))"
fi
