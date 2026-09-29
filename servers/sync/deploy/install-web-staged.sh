#!/usr/bin/env bash
set -euo pipefail

source_root=${1:-}
if [[ -z ${source_root} || ${source_root} != /* ]]; then
    echo "staged artifact directory must be an absolute path" >&2
    exit 2
fi
for required in web-dist.tar.gz admin-dist.tar.gz; do
    if [[ ! -f ${source_root}/${required} ]]; then
        echo "missing staged deployment file: ${source_root}/${required}" >&2
        exit 1
    fi
done

web_root=/usr/local/share/bokheim-web
if [[ ${EUID} -eq 0 ]]; then
    install -d -o root -g root -m 2775 "${web_root}"
elif [[ ! -d ${web_root} || ! -w ${web_root} ]]; then
    echo "web deployment is not provisioned for $(id -un); run provision-web-deploy.sh once as root" >&2
    exit 2
fi
release=$(mktemp -d "${web_root}/release.XXXXXX")
cleanup() {
    if [[ -n ${release:-} && ! -L ${web_root}/current ]]; then
        rm -rf -- "${release}"
    fi
}
trap cleanup EXIT

if tar -tzf "${source_root}/web-dist.tar.gz" | grep -Eq '(^/|(^|/)\.\.(/|$))'; then
    echo "web archive contains an unsafe path" >&2
    exit 1
fi
if tar -tzf "${source_root}/admin-dist.tar.gz" | grep -Eq '(^/|(^|/)\.\.(/|$))'; then
    echo "administrator archive contains an unsafe path" >&2
    exit 1
fi
tar -xzf "${source_root}/web-dist.tar.gz" -C "${release}"
test -f "${release}/index.html"
install -d -m 0755 "${release}/admin"
tar -xzf "${source_root}/admin-dist.tar.gz" -C "${release}/admin"
test -f "${release}/admin/index.html"
find "${release}" -type d -exec chmod 0755 {} +
find "${release}" -type f -exec chmod 0644 {} +

previous_release=$(readlink -f "${web_root}/current" 2>/dev/null || true)
# Live tabs and coordinators can still request workers from older bundles.
# Reuse immutable files via hard links so switching releases cannot turn those
# requests into missing assets. Existing release directories are retained too.
if [[ -n ${previous_release} && -d ${previous_release}/pkg ]]; then
    cp -aln -- "${previous_release}/pkg/." "${release}/pkg/"
fi

next_link=${web_root}/.current.$$
ln -s "${release}" "${next_link}"
mv -Tf "${next_link}" "${web_root}/current"

# Caddy resolves the release symlink for every request. Switching static files
# therefore needs neither root access nor a Caddy configuration reload.
if ! curl --fail --silent --show-error --max-time 10 https://app.bokheim.se/ >/dev/null || ! curl --fail --silent --show-error --max-time 10 https://app.bokheim.se/admin/ >/dev/null; then
    if [[ -n ${previous_release} ]]; then
        rollback_link=${web_root}/.current.rollback.$$
        ln -s "${previous_release}" "${rollback_link}"
        mv -Tf "${rollback_link}" "${web_root}/current"
    fi
    echo "the deployed web application did not pass its public health check; rolled back" >&2
    exit 1
fi
release=
echo "Bokheim reader and administrator web applications installed."
