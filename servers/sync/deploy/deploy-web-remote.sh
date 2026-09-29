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
dist=
while [[ $# -gt 0 ]]; do
    case "$1" in
        --stage-only) stage_only=true; shift ;;
        --dist) dist=${2:?--dist requires a verified build directory}; shift 2 ;;
        *) echo "usage: $0 [--stage-only] [--dist VERIFIED_DIRECTORY]" >&2; exit 2 ;;
    esac
done
if [[ -n "$dist" ]]; then
    # Deploy the exact bundle checked locally, without rebuilding it.
    dist=$(cd "$dist" && pwd)
    python3 - "$repository_root" "$dist" <<'PY_VERIFY'
import json, sys
from pathlib import Path
sys.path.insert(0, str(Path(sys.argv[1]) / 'scripts/ci'))
import release_artifacts as artifacts
directory = Path(sys.argv[2])
receipt = json.loads((directory / artifacts.receipt_name('web')).read_text())
artifacts.validate(receipt, 'web', receipt['source_commit'], receipt['source_repository'], directory)
if artifacts.command('git', 'rev-parse', 'HEAD') != receipt['source_commit']:
    raise SystemExit('Web bundle belongs to a different deployment source commit')
artifacts.clean_revision(receipt['source_commit'])
artifacts.command('python3', 'scripts/ci/check-server-protocol.py', receipt['source_commit'])
artifacts.command('python3', 'scripts/ci/require-shared-tests.py', receipt['source_repository'], receipt['source_commit'])
PY_VERIFY
else
    "${repository_root}/apps/web-gpui/scripts/build.sh"
    dist="${repository_root}/apps/web-gpui/dist"
fi
tar -czf "${stage}/web-dist.tar.gz" -C "$dist" .
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
