#!/usr/bin/env bash
# Run from a trusted Linux release machine matching the server architecture.
# The SSH account owns /srv/bokheim-updates; no root execution is required.
set -euo pipefail
if [[ $# != 2 ]]; then
    echo "Usage: $0 user@host /path/to/signed-bundle" >&2
    exit 2
fi
host=$1
bundle=$(realpath -- "$2")
[[ "$host" != -* && "$host" =~ ^[a-zA-Z0-9_.@:-]+$ ]] || { echo "Invalid SSH destination" >&2; exit 2; }
[[ -f "$bundle/manifest.signed.json" ]] || { echo "Missing manifest.signed.json" >&2; exit 2; }
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
# Pin the target directory so a caller's CARGO_TARGET_DIR cannot select a stale binary.
cargo build --release --manifest-path "$repo/Cargo.toml" --target-dir "$repo/target" -p update-publisher
stage=$(ssh "$host" 'mktemp -d /tmp/bokheim-update-publish.XXXXXXXX')
[[ "$stage" =~ ^/tmp/bokheim-update-publish\.[a-zA-Z0-9]+$ ]] || { echo "Unexpected staging path" >&2; exit 1; }
scp "$repo/target/release/update-publisher" "$host:$stage/update-publisher"
scp -r "$bundle" "$host:$stage/bundle"
# Config contains only independently installed public trust keys. The private
# release signing key stays on the signing machine or protected CI signer.
ssh "$host" "umask 022; '$stage/update-publisher' /etc/bokheim-updates.json '$stage/bundle' /srv/bokheim-updates"
echo "Published. Upload staging directory retained for operator cleanup: $stage"
