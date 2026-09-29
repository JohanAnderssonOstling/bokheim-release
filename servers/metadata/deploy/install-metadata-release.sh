#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID} -ne 0 || $# -ne 0 ]]; then
    echo "usage: sudo /usr/local/sbin/bokheim-deploy-metadata" >&2
    exit 2
fi

exec 9>/run/lock/bokheim-metadata-deploy.lock
flock -n 9 || { echo "another Bokheim metadata deployment is running" >&2; exit 1; }

source_root=/var/lib/bokheim-deploy/metadata-incoming
service_templates=/usr/local/share/bokheim-deploy
for required in pdfium/libpdfium.so pdfium/LICENSE metadata-server; do
    if [[ ! -f ${source_root}/${required} || -L ${source_root}/${required} ]]; then
        echo "missing or unsafe staged artifact: ${required}" >&2
        exit 1
    fi
done
if [[ ! -x ${source_root}/metadata-server ]]; then
    echo "staged server binary must be executable" >&2
    exit 1
fi
if [[ ! -r /home/johan/data/openlibrary/current.sqlite ]]; then
    echo "missing verified metadata snapshot" >&2
    exit 1
fi
for required in bokheim-metadata.service; do
    if [[ ! -f ${service_templates}/${required} || -L ${service_templates}/${required} || $(stat -c %u "${service_templates}/${required}") -ne 0 ]]; then
        echo "missing or unsafe root-owned service template: ${required}" >&2
        exit 1
    fi
done

rollback_root=$(mktemp -d /var/tmp/bokheim-metadata-rollback.XXXXXX)
metadata_binary_existed=false
metadata_unit_existed=false
activation_started=false
deployment_succeeded=false
[[ -f /usr/local/bin/metadata-server ]] && metadata_binary_existed=true
[[ -f /etc/systemd/system/bokheim-metadata.service ]] && metadata_unit_existed=true
for installed in /usr/local/bin/metadata-server /etc/systemd/system/bokheim-metadata.service; do
    if [[ -f ${installed} ]]; then
        cp -a -- "${installed}" "${rollback_root}/"
    fi
done
if [[ -d /usr/local/bin/pdfium ]]; then cp -a /usr/local/bin/pdfium "${rollback_root}/pdfium"; fi
cleanup() {
    status=$?
    trap - EXIT
    if [[ ${activation_started} == true && ${deployment_succeeded} != true ]]; then
        echo "activation failed; restoring the previous metadata release" >&2
        systemctl stop bokheim-metadata.service 2>/dev/null || true
        if [[ -d ${rollback_root}/pdfium ]]; then install -o root -g root -m 0644 "${rollback_root}/pdfium/libpdfium.so" "${rollback_root}/pdfium/LICENSE" /usr/local/bin/pdfium/; fi
        if [[ ${metadata_binary_existed} == true ]]; then
            install -o root -g root -m 0755 "${rollback_root}/metadata-server" /usr/local/bin/metadata-server
        else
            rm -f -- /usr/local/bin/metadata-server
        fi
        if [[ ${metadata_unit_existed} == true ]]; then
            install -o root -g root -m 0644 "${rollback_root}/bokheim-metadata.service" /etc/systemd/system/bokheim-metadata.service
        else
            systemctl disable --now bokheim-metadata.service 2>/dev/null || true
            rm -f -- /etc/systemd/system/bokheim-metadata.service
        fi
        systemctl daemon-reload
        if [[ ${metadata_binary_existed} == true && ${metadata_unit_existed} == true ]]; then
            systemctl restart bokheim-metadata.service || true
        fi
    fi
    rm -rf -- "${rollback_root}"
    exit "${status}"
}
trap cleanup EXIT

activation_started=true
systemctl stop bokheim-metadata.service 2>/dev/null || true
# Do not use a process-name-wide cleanup here. Linux truncates `comm` names,
# which can make a long-running metadata importer look exactly like the server
# and would interrupt an unrelated resumable snapshot build.
install -o root -g root -m 0755 "${source_root}/metadata-server" /usr/local/bin/metadata-server
install -d -o root -g root -m 0755 /usr/local/bin/pdfium
install -o root -g root -m 0644 "${source_root}/pdfium/libpdfium.so" /usr/local/bin/pdfium/libpdfium.so
install -o root -g root -m 0644 "${source_root}/pdfium/LICENSE" /usr/local/bin/pdfium/LICENSE
install -o root -g root -m 0644 "${service_templates}/bokheim-metadata.service" /etc/systemd/system/bokheim-metadata.service
systemctl daemon-reload
systemctl enable bokheim-metadata.service
systemctl restart bokheim-metadata.service

metadata_ready=false
for _ in $(seq 1 30); do
    if curl --noproxy '*' --fail --silent --max-time 3 http://127.0.0.1:8091/health | grep -Fqx ok; then metadata_ready=true; fi
    if [[ ${metadata_ready} == true ]]; then break; fi
    sleep 1
done
if [[ ${metadata_ready} != true ]]; then
    echo "metadata service did not become healthy" >&2
    exit 1
fi

curl --noproxy '*' --fail --silent --show-error --max-time 15 \
    -H 'Content-Type: application/json' \
    --data '{"isbns":["9780821338278"]}' \
    http://127.0.0.1:8091/v2/enrichment >/dev/null

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
rm -rf -- "${source_root}"
find /var/lib/bokheim-deploy -mindepth 1 -maxdepth 1 -type d -name 'metadata-previous.*' -exec rm -rf -- {} +
echo "Bokheim metadata service installed, healthy, and enrichment-verified."
