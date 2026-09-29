#!/usr/bin/env bash
set -euo pipefail

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd -- "${script_directory}/../../.." && pwd)
host=${BOKHEIM_DEPLOY_HOST:-192.168.1.68}
user=${BOKHEIM_DEPLOY_USER:-${USER}}
remote=${user}@${host}
ssh_options=()
if [[ -n ${BOKHEIM_DEPLOY_SSH_OPTS:-} ]]; then
    read -r -a ssh_options <<<"${BOKHEIM_DEPLOY_SSH_OPTS}"
fi
local_stage_root=${BOKHEIM_DEPLOY_LOCAL_STAGE_ROOT:-${repository_root}/.deploy-tmp}
install -d -m 0700 "${local_stage_root}"
stage=$(mktemp -d "${local_stage_root}/web.XXXXXX")
remote_stage=
keep_remote_stage=false
cleanup() {
    rm -rf -- "${stage}"
    if [[ -n ${remote_stage} && ${keep_remote_stage} == false ]]; then
        ssh "${ssh_options[@]}" "${remote}" "rm -rf -- '${remote_stage}'" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

stage_only=false
if [[ ${1:-} == --stage-only && $# -eq 1 ]]; then
    stage_only=true
elif [[ $# -ne 0 ]]; then
    echo "usage: $0 [--stage-only]" >&2
    exit 2
fi

"${repository_root}/apps/web-gpui/scripts/build.sh"
tar -czf "${stage}/web-dist.tar.gz" -C "${repository_root}/apps/web-gpui/dist" .
tar -czf "${stage}/admin-dist.tar.gz" -C "${repository_root}/servers/admin-web" .
install -m 0755 "${script_directory}/install-web-staged.sh" "${stage}/install-web-staged.sh"

remote_stage=$(ssh "${ssh_options[@]}" "${remote}" "mktemp -d '/tmp/bokheim-web-deploy.XXXXXX'")
scp "${ssh_options[@]}" "${stage}"/* "${remote}:${remote_stage}/"
keep_remote_stage=true
if [[ ${stage_only} == true ]]; then
    echo "Web release staged on ${remote}."
    echo "On the deployment host, run: '${remote_stage}/install-web-staged.sh' '${remote_stage}'"
    exit 0
fi

ssh -tt "${ssh_options[@]}" "${remote}" "'${remote_stage}/install-web-staged.sh' '${remote_stage}'"
keep_remote_stage=false
echo "Web deployment completed on ${remote}."
