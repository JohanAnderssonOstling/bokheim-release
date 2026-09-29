#!/usr/bin/env bash
# Run once on the serving host with a reviewed PUBLIC config and release binary.
# Does not install credentials, grant sudo access, change Caddy, or publish a feed.
set -euo pipefail
if [[ ${EUID} -ne 0 || $# -ne 3 ]]; then
    echo "Usage (as root): $0 existing-publishing-user public-config.json release-update-publisher" >&2
    exit 2
fi
publishing_user=$1
config=$2
publisher=$3
[[ "$publishing_user" =~ ^[a-z_][a-z0-9_-]*\$?$ ]] || { echo "Invalid publishing account" >&2; exit 2; }
id -- "$publishing_user" >/dev/null
[[ $(id -u -- "$publishing_user") != 0 ]] || { echo "Use a dedicated non-root publishing account" >&2; exit 2; }
[[ -f "$config" && ! -L "$config" && -x "$publisher" ]] || { echo "Provide a regular public config and an executable release publisher" >&2; exit 2; }
"$publisher" --check-config "$config"

publication_root=/srv/bokheim-updates
installed_config=/etc/bokheim-updates.json
[[ ! -L "$publication_root" && ! -L "$installed_config" ]] || { echo "Publication paths must not be symlinks" >&2; exit 1; }
if [[ -e "$publication_root" ]]; then
    [[ -d "$publication_root" && $(stat -c %u "$publication_root") == $(id -u -- "$publishing_user") ]] || { echo "Publication root belongs to a different account" >&2; exit 1; }
fi
if [[ -e "$installed_config" ]] && ! cmp -s -- "$config" "$installed_config"; then
    echo "Installed trust differs; review and install key rotation explicitly" >&2
    exit 1
fi
publishing_group=$(id -gn -- "$publishing_user")
install -d -o "$publishing_user" -g "$publishing_group" -m 0755 "$publication_root"
install -o root -g root -m 0644 -- "$config" "$installed_config"
echo "Update publication directory and public trust configured for $publishing_user."
