#!/usr/bin/env bash
set -euo pipefail

source_root=${1:-/home/johan/bokheim-deploy/incoming}

if [[ ${EUID} -ne 0 ]]; then
    echo "run this installer as root: sudo $0 [staged-artifact-directory]" >&2
    exit 2
fi

if [[ ! ${source_root} = /* ]]; then
    echo "staged artifact directory must be an absolute path" >&2
    exit 2
fi

required_files=(
    sync-server
    bokheim-sync.service
)

for file in "${required_files[@]}"; do
    if [[ ! -f ${source_root}/${file} ]]; then
        echo "missing staged deployment file: ${source_root}/${file}" >&2
        exit 1
    fi
done

if [[ ! -x ${source_root}/sync-server ]]; then
    echo "staged server binaries must be executable" >&2
    exit 1
fi

if [[ ! -s /etc/bokheim/resend-api-key ]]; then
    echo "missing Resend API key: create /etc/bokheim/resend-api-key before deploying" >&2
    exit 2
fi
if [[ ! -s /etc/bokheim/account-token-key ]]; then
    echo "missing account token key: create /etc/bokheim/account-token-key before deploying" >&2
    exit 2
fi
if [[ ! -s /etc/bokheim/admin-password ]]; then
    echo "missing administrator password: create /etc/bokheim/admin-password before deploying" >&2
    exit 2
fi
if [[ ! -s /etc/bokheim/test-user-password ]]; then
    echo "missing test-user password: create /etc/bokheim/test-user-password before deploying" >&2
    exit 2
fi
if [[ ! -s /etc/bokheim/database-url ]]; then
    echo "missing database credential: /etc/bokheim/database-url" >&2
    exit 2
fi
database_url=$(</etc/bokheim/database-url)
if [[ ! ${database_url} =~ ^postgresql://bokheim_app:[[:xdigit:]]{64}@127\.0\.0\.1/bokheim$ ]]; then
    echo "database credential does not identify the expected local bokheim database; refusing destructive deployment" >&2
    exit 2
fi

systemctl stop bokheim-sync.service

install -o root -g root -m 0755 "${source_root}/sync-server" \
    /usr/local/bin/sync-server
rm -f -- /etc/bokheim/disposable-email-domains \
    /usr/local/share/bokheim/disposable-email-domains

install -o root -g root -m 0644 "${source_root}/bokheim-sync.service" \
    /etc/systemd/system/bokheim-sync.service

# This development deployment intentionally starts from an empty metadata
# database. Physical content-addressed assets remain on the large disk.
runuser -u postgres -- dropdb --if-exists --force bokheim
runuser -u postgres -- createdb --owner=bokheim_app bokheim
DATABASE_URL="${database_url}" /usr/local/bin/sync-server migrate
unset database_url

systemctl disable --now bokheim-gc.timer >/dev/null 2>&1 || true
rm -f -- /etc/systemd/system/bokheim-gc.service /etc/systemd/system/bokheim-gc.timer /etc/tmpfiles.d/bokheim.conf
systemctl daemon-reload
systemctl restart bokheim-sync.service

sync_ready=false
for _ in $(seq 1 30); do
    if curl --fail --silent \
        --unix-socket /run/bokheim/sync.sock \
        http://localhost/api/ready | grep -Fqx ok; then
        sync_ready=true
    fi
    if [[ ${sync_ready} == true ]]; then
        break
    fi
    sleep 1
done

if [[ ${sync_ready} != true ]]; then
    echo "deployment did not become healthy" >&2
    systemctl --no-pager --full status bokheim-sync.service >&2 || true
    journalctl -u bokheim-sync.service -n 40 --no-pager -o cat >&2 || true
    exit 1
fi

echo "Bokheim server deployment installed and healthy."
systemctl --no-pager --full status bokheim-sync.service
