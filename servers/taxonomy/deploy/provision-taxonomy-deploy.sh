#!/usr/bin/env bash
set -euo pipefail

deploy_user=${1:-${SUDO_USER:-}}
if [[ ${EUID} -ne 0 || -z ${deploy_user} ]]; then
    echo "run once as root: sudo $0 deployment-user" >&2
    exit 2
fi
if ! id "${deploy_user}" >/dev/null 2>&1; then
    echo "a valid deployment user is required" >&2
    exit 2
fi

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
deploy_group=bokheim-server-deploy
getent group "${deploy_group}" >/dev/null || groupadd --system "${deploy_group}"
usermod -aG "${deploy_group}" "${deploy_user}"
install -d -o root -g "${deploy_group}" -m 3771 /var/lib/bokheim-deploy
install -o root -g root -m 0755 "${script_directory}/install-taxonomy-release.sh" /usr/local/sbin/bokheim-deploy-taxonomy

sudoers_file=/etc/sudoers.d/bokheim-taxonomy-deploy
printf '%s ALL=(root) NOPASSWD: /usr/local/sbin/bokheim-deploy-taxonomy\n' "${deploy_user}" >"${sudoers_file}"
chmod 0440 "${sudoers_file}"
visudo -cf "${sudoers_file}"

echo "Taxonomy deployment access granted to ${deploy_user}. Reconnect SSH if group membership changed."
