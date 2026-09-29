#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
    echo "usage: sudo $0 api.bokheim.se [meta.bokheim.se]" >&2
    exit 2
fi
if [[ ${EUID} -ne 0 ]]; then
    echo "run this verifier as root so it can inspect the protected runtime socket" >&2
    exit 2
fi

domain=$1
metadata_domain=${2:-meta.bokheim.se}
for candidate in "${domain}" "${metadata_domain}"; do
    if [[ ! ${candidate} =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]] || [[ ${candidate} != *.* ]]; then
        echo "invalid public DNS name: ${candidate}" >&2
        exit 2
    fi
done

for command in caddy curl openssl ss stat systemctl; do
    if ! command -v "${command}" >/dev/null; then
        echo "missing required command: ${command}" >&2
        exit 2
    fi
done

caddy_env=${BOKHEIM_CADDY_ENV:-/etc/bokheim/caddy.env}
caddy_config=${BOKHEIM_CADDY_CONFIG:-/etc/caddy/Caddyfile}
socket=${BOKHEIM_SERVER_SOCKET:-/run/bokheim/sync.sock}

systemctl is-active --quiet bokheim-sync.service
systemctl is-active --quiet caddy.service
caddy validate --envfile "${caddy_env}" --config "${caddy_config}" >/dev/null

if [[ ! -S ${socket} ]]; then
    echo "missing Unix socket: ${socket}" >&2
    exit 1
fi
read -r socket_user socket_group socket_mode < <(stat -c '%U %G %a' "${socket}")
if [[ ${socket_user} != bokheim || ${socket_group} != bokheim || ${socket_mode} != 660 ]]; then
    echo "unsafe Unix socket ownership or mode: ${socket_user}:${socket_group} ${socket_mode}; expected bokheim:bokheim 660" >&2
    exit 1
fi

public_listeners=$(ss -ltnH | awk '{print $4}')
if grep -Eq '^(0\.0\.0\.0|\[::\]|\*):8080$' <<<"${public_listeners}"; then
    echo "the Rust development port is publicly listening on 8080" >&2
    exit 1
fi
if grep -Eq '^(0\.0\.0\.0|\[::\]|\*):5432$' <<<"${public_listeners}"; then
    echo "PostgreSQL is publicly listening on 5432" >&2
    exit 1
fi

http_status=$(curl --silent --show-error --output /dev/null --write-out '%{http_code}' "http://${domain}/api/health")
if [[ ${http_status} != 308 && ${http_status} != 301 ]]; then
    echo "HTTP did not redirect to HTTPS; status was ${http_status}" >&2
    exit 1
fi

headers=$(mktemp)
trap 'rm -f "${headers}"' EXIT
health=$(curl --proto '=https' --tlsv1.2 --fail --silent --show-error --dump-header "${headers}" "https://${domain}/api/health")
if [[ ${health} != ok ]]; then
    echo "unexpected HTTPS health response: ${health}" >&2
    exit 1
fi
live=$(curl --proto '=https' --tlsv1.2 --fail --silent --show-error "https://${domain}/api/live")
ready=$(curl --proto '=https' --tlsv1.2 --fail --silent --show-error "https://${domain}/api/ready")
if [[ ${live} != ok || ${ready} != ok ]]; then
    echo "unexpected liveness/readiness response: live=${live} ready=${ready}" >&2
    exit 1
fi
if ! grep -Eqi '^strict-transport-security:[[:space:]]*max-age=31536000([[:space:]]|\r)*$' "${headers}"; then
    echo "HTTPS response is missing the expected Strict-Transport-Security header" >&2
    exit 1
fi
if grep -Eqi '^server:' "${headers}"; then
    echo "HTTPS response exposes a Server header" >&2
    exit 1
fi

if ! openssl s_client -connect "${domain}:443" -servername "${domain}" -verify_return_error </dev/null 2>/dev/null | openssl x509 -noout -checkend 604800 >/dev/null; then
    echo "TLS certificate verification failed or the certificate expires within seven days" >&2
    exit 1
fi

metadata_health=$(curl --proto '=https' --tlsv1.2 --fail --silent --show-error "https://${metadata_domain}/health")
if [[ ${metadata_health} != ok ]]; then
    echo "unexpected metadata HTTPS health response: ${metadata_health}" >&2
    exit 1
fi
if ! openssl s_client -connect "${metadata_domain}:443" -servername "${metadata_domain}" -verify_return_error </dev/null 2>/dev/null | openssl x509 -noout -checkend 604800 >/dev/null; then
    echo "metadata TLS certificate verification failed or the certificate expires within seven days" >&2
    exit 1
fi

# A malformed JSON object must reach the audiobook handler's typed extractor.
# This performs no upstream lookup and catches proxy omissions and stale binaries.
audiobook_status=$(curl --proto '=https' --tlsv1.2 --silent --show-error --max-time 15 \
    --output /dev/null --write-out '%{http_code}' -H 'Content-Type: application/json' \
    --data '{}' "https://${metadata_domain}/v2/audiobooks/audible")
if [[ ${audiobook_status} != 422 ]]; then
    echo "public audiobook endpoint is unavailable: expected 422, got ${audiobook_status}" >&2
    exit 1
fi

# Browser clients must be able to preflight the JSON POST too.
preflight=$(curl --proto '=https' --tlsv1.2 --silent --show-error --max-time 15 \
    --dump-header - --output /dev/null -X OPTIONS \
    -H 'Origin: https://app.bokheim.se' -H 'Access-Control-Request-Method: POST' \
    -H 'Access-Control-Request-Headers: content-type' \
    "https://${metadata_domain}/v2/audiobooks/audible")
if ! grep -Eqi '^access-control-allow-origin:[[:space:]]*\*' <<<"${preflight}"; then
    echo "public audiobook endpoint is missing browser CORS support" >&2
    exit 1
fi

echo "public deployment verified: https://${domain} and https://${metadata_domain}"
