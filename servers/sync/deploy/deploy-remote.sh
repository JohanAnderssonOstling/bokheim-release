#!/usr/bin/env bash
set -euo pipefail

# Build and deploy only the sync server over SSH. Catalogue and metadata
# services have independent release lifecycles and must not gate a sync fix.

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd -- "${script_directory}/../../.." && pwd)
host=${BOKHEIM_DEPLOY_HOST:-192.168.1.68}
user=${BOKHEIM_DEPLOY_USER:-${USER}}
remote_prefix=${BOKHEIM_DEPLOY_REMOTE_PREFIX:-/var/lib/bokheim-deploy/sync-upload}
cargo_target=${BOKHEIM_DEPLOY_CARGO_TARGET:-x86_64-unknown-linux-musl}
stage_only=false
ssh_options=()
if [[ -n ${BOKHEIM_DEPLOY_SSH_OPTS:-} ]]; then
    read -r -a ssh_options <<<"${BOKHEIM_DEPLOY_SSH_OPTS}"
fi

if [[ ${1:-} == --help || ${1:-} == -h ]]; then
    cat <<'EOF'
Usage: servers/sync/deploy/deploy-remote.sh [--stage-only]

  --stage-only  Build and transfer the sync release without activating it.

Environment:
  BOKHEIM_DEPLOY_HOST          SSH host (default: 192.168.1.68)
  BOKHEIM_DEPLOY_USER          SSH user (default: current user)
  BOKHEIM_DEPLOY_SSH_OPTS      Extra SSH options, space separated
  BOKHEIM_DEPLOY_REMOTE_PREFIX Temporary directory prefix (default: /var/lib/bokheim-deploy/sync-upload)
EOF
    exit 0
fi
if [[ ${1:-} == --stage-only ]]; then
    stage_only=true
elif [[ $# -ne 0 ]]; then
    echo "unknown argument: $1" >&2
    exit 2
fi

if [[ ${EUID} -eq 0 ]]; then
    echo "refusing to build and deploy as root; run this from the development checkout" >&2
    exit 2
fi

remote=${user}@${host}
local_stage_root=${BOKHEIM_DEPLOY_LOCAL_STAGE_ROOT:-${repository_root}/.deploy-tmp}
install -d -m 0700 "${local_stage_root}"
stage=$(mktemp -d "${local_stage_root}/server.XXXXXX")
remote_stage=
keep_remote_stage=false
cleanup() {
    rm -rf -- "${stage}"
    if [[ -n ${remote_stage} && ${keep_remote_stage} == false ]]; then
        ssh "${ssh_options[@]}" "${remote}" "rm -rf -- '${remote_stage}'" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

echo "Building sync-server for ${cargo_target}..."
cargo build --release --target "${cargo_target}" --manifest-path "${repository_root}/servers/sync/Cargo.toml" -p sync-server

sync_binary=${repository_root}/servers/sync/target/${cargo_target}/release/sync-server
for required in "${sync_binary}"; do
    if [[ ! -x ${required} ]]; then
        echo "missing executable release artifact: ${required}" >&2
        exit 1
    fi
done
for required in "${repository_root}/servers/sync/deploy/bokheim-sync.service"; do
    if [[ ! -f ${required} ]]; then
        echo "missing deployment file: ${required}" >&2
        exit 1
    fi
done

install -m 0755 "${sync_binary}" "${stage}/sync-server"
install -m 0644 "${repository_root}/servers/sync/deploy/bokheim-sync.service" "${stage}/bokheim-sync.service"
echo "Checking SSH access to ${remote}..."
remote_stage=$(ssh "${ssh_options[@]}" "${remote}" "mktemp -d '${remote_prefix}.XXXXXX'")
scp "${ssh_options[@]}" "${stage}"/* "${remote}:${remote_stage}/"
ssh "${ssh_options[@]}" "${remote}" \
    "chmod 0755 '${remote_stage}' && if test -d /var/lib/bokheim-deploy/sync-incoming; then mv /var/lib/bokheim-deploy/sync-incoming /var/lib/bokheim-deploy/sync-previous.\$\$; fi; mv '${remote_stage}' /var/lib/bokheim-deploy/sync-incoming"
remote_stage=/var/lib/bokheim-deploy/sync-incoming

if [[ ${stage_only} == true ]]; then
    keep_remote_stage=true
    echo "Release staged on ${remote}."
    echo "Activate with: sudo /usr/local/sbin/bokheim-deploy-sync"
    exit 0
fi

echo "Installing staged release on ${remote}..."
ssh "${ssh_options[@]}" "${remote}" "sudo -n /usr/local/sbin/bokheim-deploy-sync"
keep_remote_stage=true

echo "Deployment completed successfully on ${remote}."
