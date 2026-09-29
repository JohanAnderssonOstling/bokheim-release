#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID} -ne 0 || $# -ne 0 ]]; then
    echo "usage: sudo /usr/local/sbin/bokheim-deploy-taxonomy" >&2
    exit 2
fi

exec 9>/run/lock/bokheim-taxonomy-deploy.lock
flock -n 9 || { echo "another Bokheim taxonomy deployment is running" >&2; exit 1; }

incoming=/var/lib/bokheim-deploy/taxonomy-incoming
for file in taxonomy-server unified-taxonomy-v2.sqlite3 bokheim-taxonomy.service Caddyfile; do
    if [[ ! -f ${incoming}/${file} || -L ${incoming}/${file} ]]; then
        echo "missing or unsafe staged taxonomy artifact: ${file}" >&2
        exit 1
    fi
done
[[ -x ${incoming}/taxonomy-server ]] || { echo "staged taxonomy server must be executable" >&2; exit 1; }

candidate=$(mktemp -d /var/lib/bokheim-deploy/taxonomy-candidate.XXXXXX)
backup=$(mktemp -d /var/lib/bokheim-deploy/taxonomy-backup.XXXXXX)
activation_started=false
deployment_succeeded=false
service_preexisting=false
[[ -f /etc/systemd/system/bokheim-taxonomy.service ]] && service_preexisting=true
cleanup() {
    status=$?
    trap - EXIT
    if [[ ${activation_started} == true && ${deployment_succeeded} != true ]]; then
        echo "restoring the previous taxonomy deployment" >&2
        [[ -f ${backup}/taxonomy-server ]] && install -o root -g root -m 0755 "${backup}/taxonomy-server" /usr/local/bin/taxonomy-server
        [[ -f ${backup}/unified-taxonomy-v2.sqlite3 ]] && install -o root -g root -m 0644 "${backup}/unified-taxonomy-v2.sqlite3" /usr/local/share/bokheim-taxonomy/unified-taxonomy-v2.sqlite3
        [[ -f ${backup}/bokheim-taxonomy.service ]] && install -o root -g root -m 0644 "${backup}/bokheim-taxonomy.service" /etc/systemd/system/bokheim-taxonomy.service
        [[ -f ${backup}/Caddyfile ]] && install -o root -g root -m 0644 "${backup}/Caddyfile" /etc/caddy/Caddyfile
        if [[ ${service_preexisting} == false ]]; then
            systemctl disable --now bokheim-taxonomy.service 2>/dev/null || true
            rm -f -- /etc/systemd/system/bokheim-taxonomy.service
        fi
        systemctl daemon-reload || true
        systemctl restart bokheim-taxonomy.service 2>/dev/null || true
        systemctl reload caddy.service 2>/dev/null || true
    fi
    rm -rf -- "${candidate}" "${backup}"
    exit "${status}"
}
trap cleanup EXIT

install -o root -g root -m 0755 "${incoming}/taxonomy-server" "${candidate}/taxonomy-server"
install -o root -g root -m 0644 "${incoming}/unified-taxonomy-v2.sqlite3" "${candidate}/unified-taxonomy-v2.sqlite3"
install -o root -g root -m 0644 "${incoming}/bokheim-taxonomy.service" "${candidate}/bokheim-taxonomy.service"
install -o root -g root -m 0644 "${incoming}/Caddyfile" "${candidate}/Caddyfile"

# A release is created at publication time, not at build time. Allocation is
# protected by the deployment flock above, so two publishers cannot receive
# the same number. The source snapshot carries release 1 for a fresh install.
previous_release=0
installed_database=/usr/local/share/bokheim-taxonomy/unified-taxonomy-v2.sqlite3
if [[ -f ${installed_database} ]]; then
    previous_release=$(sqlite3 "${installed_database}" "SELECT value FROM taxonomy_meta WHERE key='release_id';")
    [[ ${previous_release} =~ ^[1-9][0-9]*$ ]] || { echo "installed taxonomy has an invalid release identifier" >&2; exit 1; }
fi
next_release=$((previous_release + 1))
sqlite3 "${candidate}/unified-taxonomy-v2.sqlite3" \
    "INSERT INTO taxonomy_meta(key,value) VALUES('release_id','${next_release}') ON CONFLICT(key) DO UPDATE SET value=excluded.value;"

/usr/bin/caddy validate --config "${candidate}/Caddyfile" --envfile /etc/bokheim/caddy.env
sqlite3 "${candidate}/unified-taxonomy-v2.sqlite3" "PRAGMA quick_check; PRAGMA foreign_key_check;" | grep -Fqx ok

for installed in /usr/local/bin/taxonomy-server /usr/local/share/bokheim-taxonomy/unified-taxonomy-v2.sqlite3 /etc/systemd/system/bokheim-taxonomy.service /etc/caddy/Caddyfile; do
    if [[ -f ${installed} ]]; then cp -a -- "${installed}" "${backup}/"; fi
done

activation_started=true
install -d -o root -g root -m 0755 /usr/local/share/bokheim-taxonomy
install -o root -g root -m 0755 "${candidate}/taxonomy-server" /usr/local/bin/taxonomy-server
install -o root -g root -m 0644 "${candidate}/unified-taxonomy-v2.sqlite3" /usr/local/share/bokheim-taxonomy/unified-taxonomy-v2.sqlite3
install -o root -g root -m 0644 "${candidate}/bokheim-taxonomy.service" /etc/systemd/system/bokheim-taxonomy.service
install -o root -g root -m 0644 "${candidate}/Caddyfile" /etc/caddy/Caddyfile
systemctl daemon-reload
systemctl enable --now bokheim-taxonomy.service
systemctl reload caddy.service

ready=false
for _ in $(seq 1 45); do
    if curl --fail --silent http://127.0.0.1:8093/health | grep -Fqx ok; then
        ready=true
        break
    fi
    sleep 1
done
[[ ${ready} == true ]] || { echo "taxonomy deployment did not become healthy" >&2; exit 1; }

deployment_succeeded=true
rm -rf -- "${incoming}"
find /var/lib/bokheim-deploy -mindepth 1 -maxdepth 1 -type d -name 'taxonomy-previous.*' -exec rm -rf -- {} +
echo "Bokheim taxonomy server deployed and healthy."
