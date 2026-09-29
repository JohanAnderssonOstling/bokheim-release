#!/usr/bin/env bash
set -euo pipefail

deploy_user=${1:-${SUDO_USER:-}}
if [[ ${EUID} -ne 0 ]]; then
    echo "run once as root: sudo $0 deployment-user" >&2
    exit 2
fi
if [[ -z ${deploy_user} ]] || ! id "${deploy_user}" >/dev/null 2>&1; then
    echo "a valid deployment user is required" >&2
    exit 2
fi

deploy_group=bokheim-web-deploy
web_root=/usr/local/share/bokheim-web
getent group "${deploy_group}" >/dev/null || groupadd --system "${deploy_group}"
usermod -aG "${deploy_group}" "${deploy_user}"
install -d -o root -g "${deploy_group}" -m 2775 "${web_root}"
chgrp "${deploy_group}" "${web_root}"
chmod 2775 "${web_root}"

echo "Web deployment access granted to ${deploy_user}. Reconnect SSH before deploying."
