#!/usr/bin/env bash
set -euo pipefail

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd -- "${script_directory}/../../.." && pwd)
host=${BOKHEIM_DEPLOY_HOST:-192.168.1.68}
user=${BOKHEIM_DEPLOY_USER:-${USER}}
remote=${user}@${host}
remote_prefix=${BOKHEIM_DEPLOY_REMOTE_PREFIX:-/var/lib/bokheim-deploy/taxonomy-upload}
ssh_options=()
if [[ -n ${BOKHEIM_DEPLOY_SSH_OPTS:-} ]]; then read -r -a ssh_options <<<"${BOKHEIM_DEPLOY_SSH_OPTS}"; fi

if [[ ${EUID} -eq 0 ]]; then
    echo "refusing to deploy as root" >&2
    exit 2
fi

local_stage_root=${BOKHEIM_DEPLOY_LOCAL_STAGE_ROOT:-${repository_root}/.deploy-tmp}
install -d -m 0700 "${local_stage_root}"
stage=$(mktemp -d "${local_stage_root}/taxonomy.XXXXXX")
remote_stage=
cleanup() {
    rm -rf -- "${stage}"
    if [[ -n ${remote_stage} ]]; then ssh "${ssh_options[@]}" "${remote}" "rm -rf -- '${remote_stage}'" >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT

echo "Building taxonomy-server..."
cargo build --release --manifest-path "${repository_root}/Cargo.toml" -p taxonomy-server
binary=${repository_root}/target/release/taxonomy-server
database=${repository_root}/shared/subject-projection/data/unified-taxonomy-v2.sqlite3
[[ -x ${binary} && -f ${database} ]] || { echo "missing taxonomy deployment artifact" >&2; exit 1; }

install -m 0755 "${binary}" "${stage}/taxonomy-server"
install -m 0644 "${database}" "${stage}/unified-taxonomy-v2.sqlite3"
install -m 0644 "${script_directory}/bokheim-taxonomy.service" "${stage}/bokheim-taxonomy.service"
install -m 0644 "${repository_root}/servers/sync/deploy/Caddyfile" "${stage}/Caddyfile"

remote_stage=$(ssh "${ssh_options[@]}" "${remote}" "mktemp -d '${remote_prefix}.XXXXXX'")
scp "${ssh_options[@]}" "${stage}"/* "${remote}:${remote_stage}/"
ssh "${ssh_options[@]}" "${remote}" "if test -d /var/lib/bokheim-deploy/taxonomy-incoming; then mv /var/lib/bokheim-deploy/taxonomy-incoming /var/lib/bokheim-deploy/taxonomy-previous.\$\$; fi; mv '${remote_stage}' /var/lib/bokheim-deploy/taxonomy-incoming"
remote_stage=
ssh "${ssh_options[@]}" "${remote}" "sudo -n /usr/local/sbin/bokheim-deploy-taxonomy"
echo "Taxonomy deployment completed successfully on ${remote}."
