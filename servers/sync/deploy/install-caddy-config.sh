#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID} -ne 0 ]]; then
    echo "usage: sudo $0 [Caddyfile]" >&2
    exit 2
fi
if [[ $# -gt 1 ]]; then
    echo "usage: sudo $0 [Caddyfile]" >&2
    exit 2
fi

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
candidate=${1:-${script_directory}/Caddyfile}
installed=/etc/caddy/Caddyfile
caddy_env=/etc/bokheim/caddy.env
metadata_url=https://meta.bokheim.se/health

for command in caddy curl install systemctl; do
    if ! command -v "${command}" >/dev/null; then
        echo "missing required command: ${command}" >&2
        exit 2
    fi
done
if [[ ! -f ${candidate} || -L ${candidate} ]]; then
    echo "candidate must be a regular, non-symlink file: ${candidate}" >&2
    exit 2
fi
if [[ ! -f ${installed} || -L ${installed} ]]; then
    echo "installed Caddyfile is missing or unsafe: ${installed}" >&2
    exit 2
fi
if [[ ! -f ${caddy_env} || -L ${caddy_env} ]]; then
    echo "Caddy environment file is missing or unsafe: ${caddy_env}" >&2
    exit 2
fi

echo "Validating ${candidate} with ${caddy_env}..."
caddy validate --envfile "${caddy_env}" --config "${candidate}" --adapter caddyfile

backup="${installed}.backup.$(date -u +%Y%m%dT%H%M%SZ)"
cp -a -- "${installed}" "${backup}"
activation_started=false
deployment_succeeded=false
cleanup() {
    status=$?
    trap - EXIT
    if [[ ${activation_started} == true && ${deployment_succeeded} != true ]]; then
        echo "Caddy activation failed; restoring ${backup}." >&2
        install -o root -g root -m 0644 "${backup}" "${installed}"
        systemctl reload caddy.service || true
    fi
    exit "${status}"
}
trap cleanup EXIT

activation_started=true
install -o root -g root -m 0644 "${candidate}" "${installed}"
systemctl reload caddy.service
systemctl is-active --quiet caddy.service

metadata_ready=false
for _ in $(seq 1 30); do
    if [[ $(curl --proto '=https' --tlsv1.2 --fail --silent --show-error --max-time 5 "${metadata_url}" 2>/dev/null) == ok ]]; then
        metadata_ready=true
        break
    fi
    sleep 1
done
if [[ ${metadata_ready} != true ]]; then
    echo "metadata health verification failed: ${metadata_url}" >&2
    exit 1
fi

deployment_succeeded=true
echo "Caddy configuration installed permanently and reloaded."
echo "Previous configuration: ${backup}"
echo "Metadata endpoint verified: ${metadata_url}"
