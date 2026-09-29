#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/../../.." && pwd)
temp_root=${TMPDIR:-/tmp}
e2e_tmp=$(mktemp -d "$temp_root/bokheim-sync-e2e.XXXXXX")
postgres_data="$e2e_tmp/postgres"
postgres_socket=
postgres_started=0
server_pid=

cleanup() {
    local exit_code=$?
    if [[ -n "$server_pid" ]]; then
        kill "$server_pid" >/dev/null 2>&1 || true
        wait "$server_pid" >/dev/null 2>&1 || true
    fi
    if [[ "$postgres_started" == 1 ]]; then
        pg_ctl -D "$postgres_data" -m immediate -w stop >/dev/null 2>&1 || true
    fi
    if [[ "$exit_code" != 0 && -f "$e2e_tmp/server.log" ]]; then
        sed -n '1,240p' "$e2e_tmp/server.log" >&2
    fi
    if [[ "$e2e_tmp" == "$temp_root"/bokheim-sync-e2e.* ]]; then
        rm -rf -- "$e2e_tmp"
    fi
    if [[ -n "$postgres_socket" && "$postgres_socket" == /tmp/bokheim-sync-socket.* ]]; then
        rm -rf -- "$postgres_socket"
    fi
}
trap cleanup EXIT

if [[ -n "${SYNC_E2E_DATABASE_URL:-}" ]]; then
    database_url=$SYNC_E2E_DATABASE_URL
else
    postgres_socket=$(mktemp -d /tmp/bokheim-sync-socket.XXXXXX)
    initdb -D "$postgres_data" -A trust -U postgres --no-locale >/dev/null

    postgres_port=$((55000 + ($$ % 5000)))
    while (echo >/dev/tcp/127.0.0.1/"$postgres_port") >/dev/null 2>&1; do
        postgres_port=$((postgres_port + 1))
    done

    if ! pg_ctl \
        -D "$postgres_data" \
        -l "$e2e_tmp/postgres.log" \
        -o "-F -h 127.0.0.1 -p $postgres_port -k $postgres_socket" \
        -w start >/dev/null; then
        sed -n '1,200p' "$e2e_tmp/postgres.log" >&2
        exit 1
    fi
    postgres_started=1

    database_name="sync_e2e_$$"
    createdb -h 127.0.0.1 -p "$postgres_port" -U postgres "$database_name"
    database_url="postgres://postgres@127.0.0.1:$postgres_port/$database_name"
fi

server_target=${SYNC_E2E_SERVER_TARGET_DIR:-$repo_root/servers/sync/target}
backend_target=${SYNC_E2E_BACKEND_TARGET_DIR:-$repo_root/target}
CARGO_TARGET_DIR="$server_target" cargo build --release --manifest-path "$repo_root/servers/sync/Cargo.toml"

SYNC_E2E_DATABASE_URL="$database_url" \
CARGO_TARGET_DIR="$server_target" \
cargo test --release \
    --manifest-path "$repo_root/servers/sync/Cargo.toml" \
    -p server-account \
    -p server-postgres \
    -- \
    --include-ignored \
    --test-threads=1

SYNC_E2E_DATABASE_URL="$database_url" \
CARGO_TARGET_DIR="$server_target" \
cargo test --release \
    --manifest-path "$repo_root/servers/sync/Cargo.toml" \
    -p sync-server \
    --test public_registration \
    -- \
    --ignored \
    --test-threads=1

assets_root="$e2e_tmp/assets"
mkdir -p "$assets_root/books" "$assets_root/thumbnails"

server_port=$((50000 + ($$ % 5000)))
while (echo >/dev/tcp/127.0.0.1/"$server_port") >/dev/null 2>&1; do
    server_port=$((server_port + 1))
done
DATABASE_URL="$database_url" \
SYNC_SERVER_ADDR="127.0.0.1:$server_port" \
BOKHEIM_ACCOUNT_TOKEN_KEY="WlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlo=" \
BOKHEIM_TEST_USER_EMAIL="admin@bokheim.local" \
BOKHEIM_TEST_USER_PASSWORD="sync-e2e-admin-password" \
BOKHEIM_ASSETS_DIR="$assets_root" \
"$server_target/release/sync-server" >"$e2e_tmp/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do
    if curl --fail --silent "http://127.0.0.1:$server_port/api/ready" >/dev/null; then
        break
    fi
    sleep 0.1
done
curl --fail --silent "http://127.0.0.1:$server_port/api/ready" >/dev/null
curl --fail --silent "http://127.0.0.1:$server_port/api/live" >/dev/null
curl --fail --silent "http://127.0.0.1:$server_port/api/health" >/dev/null

SYNC_E2E_DATABASE_URL="$database_url" \
SYNC_E2E_SERVER_BIN="$server_target/release/sync-server" \
SYNC_E2E_SERVER_URL="http://127.0.0.1:$server_port" \
SYNC_E2E_ADMIN_PASSWORD="sync-e2e-admin-password" \
CARGO_TARGET_DIR="$backend_target" \
cargo test --release \
    --manifest-path "$repo_root/client/library-backend/Cargo.toml" \
    --lib \
    production_http_postgres_ \
    -- \
    --ignored \
    --test-threads=1 | tee "$e2e_tmp/backend-tests.log"

if ! rg -q '^test result: ok\. [1-9][0-9]* passed;' "$e2e_tmp/backend-tests.log"; then
    echo "No PostgreSQL client tests ran; check the test filter." >&2
    exit 1
fi
