#!/usr/bin/env bash
set -euo pipefail

# Privileged entry point for an already staged release. This file and
# install-staged.sh are copied into the same directory by deploy-remote.sh.
if [[ ${EUID} -ne 0 ]]; then
    echo "run this deployment as root: sudo $0" >&2
    exit 2
fi
if [[ $# -ne 0 ]]; then
    echo "usage: sudo $0" >&2
    exit 2
fi

script_directory=$(cd -P -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
exec bash "${script_directory}/install-staged.sh" "${script_directory}"
