#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID} -ne 0 || $# -ne 0 ]]; then
    echo "usage: sudo /usr/local/sbin/bokheim-deploy-servers" >&2
    exit 2
fi

exec 9>/run/lock/bokheim-server-deploy.lock
flock -n 9 || { echo "another Bokheim server deployment is running" >&2; exit 1; }

incoming=/var/lib/bokheim-deploy/incoming
for file in sync-server metadata-server bokheim-sync.service bokheim-metadata.service; do
    if [[ ! -f ${incoming}/${file} || -L ${incoming}/${file} ]]; then
        echo "missing or unsafe staged artifact: ${file}" >&2
        exit 1
    fi
done
if [[ ! -x ${incoming}/sync-server || ! -x ${incoming}/metadata-server ]]; then
    echo "staged server binaries must be executable" >&2
    exit 1
fi
if [[ ! -r /home/johan/data/openlibrary/current.sqlite ]]; then
    echo "missing Open Library snapshot: /home/johan/data/openlibrary/current.sqlite" >&2
    exit 2
fi

for credential in database-url account-token-key admin-password resend-api-key test-user-password; do
    if [[ ! -s /etc/bokheim/${credential} ]]; then
        echo "missing credential: /etc/bokheim/${credential}" >&2
        exit 2
    fi
done
database_url=$(</etc/bokheim/database-url)
if [[ ! ${database_url} =~ ^postgresql://bokheim_app:[[:xdigit:]]{64}@127\.0\.0\.1/bokheim$ ]]; then
    echo "database credential does not identify the expected local Bokheim database" >&2
    exit 2
fi

candidate=$(mktemp -d /var/lib/bokheim-deploy/candidate.XXXXXX)
backup=$(mktemp -d /var/lib/bokheim-deploy/backup.XXXXXX)
chmod 0755 "${candidate}"
activation_started=false
deployment_succeeded=false
metadata_binary_preexisting=false
metadata_unit_preexisting=false
[[ -f /usr/local/bin/metadata-server ]] && metadata_binary_preexisting=true
[[ -f /etc/systemd/system/bokheim-metadata.service ]] && metadata_unit_preexisting=true
cleanup() {
    status=$?
    trap - EXIT
    if [[ ${activation_started} == true && ${deployment_succeeded} != true ]]; then
        echo "restoring the previous server executables" >&2
        [[ -f ${backup}/sync-server ]] && install -o root -g root -m 0755 "${backup}/sync-server" /usr/local/bin/sync-server
        [[ -f ${backup}/metadata-server ]] && install -o root -g root -m 0755 "${backup}/metadata-server" /usr/local/bin/metadata-server
        [[ -f ${backup}/bokheim-sync.service ]] && install -o root -g root -m 0644 "${backup}/bokheim-sync.service" /etc/systemd/system/bokheim-sync.service
        [[ -f ${backup}/bokheim-metadata.service ]] && install -o root -g root -m 0644 "${backup}/bokheim-metadata.service" /etc/systemd/system/bokheim-metadata.service
        if [[ ${metadata_unit_preexisting} == false ]]; then
            systemctl disable --now bokheim-metadata.service || true
            rm -f -- /etc/systemd/system/bokheim-metadata.service
        fi
        [[ ${metadata_binary_preexisting} == false ]] && rm -f -- /usr/local/bin/metadata-server
        systemctl daemon-reload || true
        if [[ -n ${database_url:-} && -x /usr/local/bin/sync-server ]]; then
            runuser -u bokheim -- env DATABASE_URL="${database_url}" /usr/local/bin/sync-server migrate || true
        fi
        systemctl restart bokheim-metadata.service bokheim-sync.service || true
    fi
    rm -rf -- "${candidate}" "${backup}"
    exit "${status}"
}
trap cleanup EXIT
install -o root -g root -m 0755 "${incoming}/sync-server" "${candidate}/sync-server"
install -o root -g root -m 0755 "${incoming}/metadata-server" "${candidate}/metadata-server"
install -o root -g root -m 0644 "${incoming}/bokheim-sync.service" "${candidate}/bokheim-sync.service"
install -o root -g root -m 0644 "${incoming}/bokheim-metadata.service" "${candidate}/bokheim-metadata.service"

for installed in /usr/local/bin/sync-server /usr/local/bin/metadata-server /etc/systemd/system/bokheim-sync.service /etc/systemd/system/bokheim-metadata.service; do
    if [[ -f ${installed} ]]; then cp -a -- "${installed}" "${backup}/"; fi
done

activation_started=true
systemctl stop bokheim-sync.service bokheim-metadata.service 2>/dev/null || true
install -o root -g root -m 0755 "${candidate}/sync-server" /usr/local/bin/sync-server
install -o root -g root -m 0755 "${candidate}/metadata-server" /usr/local/bin/metadata-server
install -o root -g root -m 0644 "${candidate}/bokheim-sync.service" /etc/systemd/system/bokheim-sync.service
install -o root -g root -m 0644 "${candidate}/bokheim-metadata.service" /etc/systemd/system/bokheim-metadata.service
systemctl daemon-reload
systemctl enable bokheim-metadata.service

# Deployments intentionally reset metadata; content-addressed assets remain.
runuser -u postgres -- dropdb --if-exists --force bokheim
runuser -u postgres -- createdb --owner=bokheim_app bokheim
runuser -u bokheim -- env DATABASE_URL="${database_url}" /usr/local/bin/sync-server migrate

systemctl restart bokheim-metadata.service
systemctl restart bokheim-sync.service

sync_ready=false
metadata_ready=false
for _ in $(seq 1 30); do
    if curl --fail --silent --unix-socket /run/bokheim/sync.sock http://localhost/api/ready | grep -Fqx ok; then sync_ready=true; fi
    if curl --fail --silent http://127.0.0.1:8091/health | grep -Fqx ok; then metadata_ready=true; fi
    if [[ ${sync_ready} == true && ${metadata_ready} == true ]]; then break; fi
    sleep 1
done
if [[ ${sync_ready} != true || ${metadata_ready} != true ]]; then
    echo "deployment did not become healthy" >&2
    exit 1
fi

# Check the public route as well as the internal health endpoint. No provider call.
audiobook_status=$(curl --proto '=https' --tlsv1.2 --silent --show-error --max-time 15 \
    --output /dev/null --write-out '%{http_code}' -H 'Content-Type: application/json' \
    --data '{}' "https://${BOKHEIM_METADATA_DOMAIN:-meta.bokheim.se}/v2/audiobooks/audible")
if [[ ${audiobook_status} != 422 ]]; then
    echo "public audiobook endpoint verification failed: expected 422, got ${audiobook_status}" >&2
    exit 1
fi

# Browser clients must be able to preflight the JSON POST too.
preflight=$(curl --proto '=https' --tlsv1.2 --silent --show-error --max-time 15 \
    --dump-header - --output /dev/null -X OPTIONS \
    -H 'Origin: https://app.bokheim.se' -H 'Access-Control-Request-Method: POST' \
    -H 'Access-Control-Request-Headers: content-type' \
    "https://${BOKHEIM_METADATA_DOMAIN:-meta.bokheim.se}/v2/audiobooks/audible")
if ! grep -Eqi '^access-control-allow-origin:[[:space:]]*\*' <<<"${preflight}"; then
    echo "public audiobook endpoint is missing browser CORS support" >&2
    exit 1
fi

deployment_succeeded=true
unset database_url
rm -rf -- "${incoming}"
find /var/lib/bokheim-deploy -mindepth 1 -maxdepth 1 -type d -name 'previous.*' -exec rm -rf -- {} +
echo "Bokheim sync and metadata servers deployed and healthy."
