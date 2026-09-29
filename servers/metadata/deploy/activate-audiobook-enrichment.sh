#!/usr/bin/env bash
set -euo pipefail

# Activate a reviewed metadata release and the existing Caddyfile with only
# the audiobook route added. Preserve service settings and metadata snapshots.
stage=${1:-}
if [[ ${EUID} -ne 0 || ${stage} != /* || $# -ne 1 ]]; then
    echo "usage: sudo $0 /absolute/staged-release-directory" >&2
    exit 2
fi
for file in metadata-server Caddyfile Caddyfile.before.sha256 verify-public.sh; do
    [[ -f ${stage}/${file} && ! -L ${stage}/${file} ]] || { echo "missing staged ${file}" >&2; exit 1; }
done
[[ -x ${stage}/metadata-server ]] || exit 1
exec 9>/run/lock/bokheim-metadata-deploy.lock
flock -n 9 || { echo "another metadata deployment is running" >&2; exit 1; }
expected=$(cat "${stage}/Caddyfile.before.sha256")
actual=$(sha256sum /etc/caddy/Caddyfile | cut -d ' ' -f 1)
[[ ${actual} == "${expected}" ]] || { echo "Caddyfile changed since staging; review and stage again" >&2; exit 1; }
caddy validate --envfile /etc/bokheim/caddy.env --config "${stage}/Caddyfile" --adapter caddyfile
backup=$(mktemp -d /var/tmp/bokheim-audiobook-rollback.XXXXXX)
cp -a /usr/local/bin/metadata-server /etc/caddy/Caddyfile "${backup}/"
if [[ -f /usr/local/sbin/bokheim-verify-public ]]; then cp -a /usr/local/sbin/bokheim-verify-public "${backup}/"; fi
complete=false
rollback() {
    status=$?
    trap - EXIT
    if [[ ${complete} != true ]]; then
        systemctl stop bokheim-metadata.service || true
        install -o root -g root -m 0755 "${backup}/metadata-server" /usr/local/bin/metadata-server
        install -o root -g root -m 0644 "${backup}/Caddyfile" /etc/caddy/Caddyfile
        if [[ -f ${backup}/bokheim-verify-public ]]; then install -o root -g root -m 0755 "${backup}/bokheim-verify-public" /usr/local/sbin/bokheim-verify-public; fi
        systemctl restart bokheim-metadata.service || true
        systemctl reload caddy.service || true
        echo "Activation failed; previous release restored. Backup: ${backup}" >&2
    fi
    exit "${status}"
}
trap rollback EXIT
systemctl stop bokheim-metadata.service
install -o root -g root -m 0755 "${stage}/metadata-server" /usr/local/bin/metadata-server
install -o root -g root -m 0644 "${stage}/Caddyfile" /etc/caddy/Caddyfile
systemctl restart bokheim-metadata.service
systemctl reload caddy.service
ready=false
for _ in $(seq 1 30); do
    if [[ $(curl -fsS --max-time 3 http://127.0.0.1:8091/health) == ok ]]; then ready=true; break; fi
    sleep 1
done
[[ ${ready} == true ]] || { echo "metadata service is not healthy" >&2; exit 1; }
code=$(curl -sS --max-time 15 -o /dev/null -w '%{http_code}' -H 'Content-Type: application/json' \
    --data '{}' "https://${BOKHEIM_METADATA_DOMAIN:-meta.bokheim.se}/v2/audiobooks/audible")
[[ ${code} == 422 ]] || { echo "public audiobook endpoint returned ${code}; expected 422" >&2; exit 1; }
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

install -o root -g root -m 0755 "${stage}/verify-public.sh" /usr/local/sbin/bokheim-verify-public
complete=true
echo "Metadata release and persistent audiobook route activated. Backup: ${backup}"
