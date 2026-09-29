#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID} -ne 0 || $# -ne 0 ]]; then
    echo "usage: sudo /usr/local/sbin/bokheim-deploy-sync" >&2
    exit 2
fi

exec 9>/run/lock/bokheim-sync-deploy.lock
flock -n 9 || { echo "another Bokheim sync deployment is running" >&2; exit 1; }

incoming=/var/lib/bokheim-deploy/sync-incoming
for file in sync-server bokheim-sync.service; do
    if [[ ! -f ${incoming}/${file} || -L ${incoming}/${file} ]]; then
        echo "missing or unsafe staged artifact: ${file}" >&2
        exit 1
    fi
done
[[ -x ${incoming}/sync-server ]] || { echo "staged sync server must be executable" >&2; exit 1; }
for credential in database-url admin-password resend-api-key test-user-password; do
    [[ -s /etc/bokheim/${credential} ]] || { echo "missing credential: /etc/bokheim/${credential}" >&2; exit 2; }
done
if [[ ! -s /etc/bokheim/account-token-key ]]; then
    umask 077
    openssl rand -base64 32 >/etc/bokheim/account-token-key
fi
chown root:root /etc/bokheim/account-token-key
chmod 0600 /etc/bokheim/account-token-key

database_url=$(</etc/bokheim/database-url)
if [[ ! ${database_url} =~ ^postgresql://bokheim_app:[[:xdigit:]]{64}@127\.0\.0\.1/bokheim$ ]]; then
    echo "database credential does not identify the expected Bokheim database" >&2
    exit 2
fi

rollback=$(mktemp -d /var/tmp/bokheim-sync-rollback.XXXXXX)
activation_started=false
deployment_succeeded=false
cleanup() {
    status=$?
    trap - EXIT
    if [[ ${activation_started} == true && ${deployment_succeeded} != true ]]; then
        echo "sync activation failed; restoring the previous release" >&2
        install -o root -g root -m 0755 "${rollback}/sync-server" /usr/local/bin/sync-server
        install -o root -g root -m 0644 "${rollback}/bokheim-sync.service" /etc/systemd/system/bokheim-sync.service
        systemctl daemon-reload
        systemctl restart bokheim-sync.service || true
    fi
    rm -rf -- "${rollback}"
    exit "${status}"
}
trap cleanup EXIT

cp -a -- /usr/local/bin/sync-server "${rollback}/sync-server"
cp -a -- /etc/systemd/system/bokheim-sync.service "${rollback}/bokheim-sync.service"

# Refuse activation until the initial schema has been provisioned separately.
runuser -u bokheim -- env DATABASE_URL="${database_url}" "${incoming}/sync-server" verify-schema

activation_started=true
systemctl stop bokheim-sync.service
install -o root -g root -m 0755 "${incoming}/sync-server" /usr/local/bin/sync-server
install -o root -g root -m 0644 "${incoming}/bokheim-sync.service" /etc/systemd/system/bokheim-sync.service
systemctl daemon-reload
systemctl restart bokheim-sync.service

ready=false
for _ in $(seq 1 30); do
    if curl --fail --silent --unix-socket /run/bokheim/sync.sock http://localhost/api/ready | grep -Fqx ok; then
        ready=true
        break
    fi
    sleep 1
done
[[ ${ready} == true ]] || { echo "sync server did not become ready" >&2; exit 1; }

# An unauthenticated request must reach the admin route and be rejected as
# unauthorized. A 404 means the wrong server release is active.
admin_status=$(curl --silent --output /dev/null --write-out '%{http_code}' --max-time 10 -H 'Sec-Fetch-Site: same-origin' https://app.bokheim.se/api/admin/overview)
[[ ${admin_status} == 401 ]] || { echo "administrator API verification returned HTTP ${admin_status}" >&2; exit 1; }

deployment_succeeded=true
unset database_url
rm -rf -- "${incoming}"
echo "Bokheim sync server deployed; administrator API verified."
