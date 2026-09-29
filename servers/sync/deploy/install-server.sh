#!/usr/bin/env bash
set -euo pipefail

source_root=${1:-/home/johan/bokheim-deploy/source}
if [[ ${EUID} -ne 0 ]]; then
    echo "run this installer as root: sudo $0 [staged-source-root]" >&2
    exit 2
fi
if [[ ! ${source_root} = /* ]]; then
    echo "staged source root must be an absolute path" >&2
    exit 2
fi

sync_binary=${source_root}/deploy-target/release/sync-server
for required in "${sync_binary}" "${source_root}/servers/sync/deploy/bokheim-sync.service" "${source_root}/servers/sync/deploy/Caddyfile"; do
    if [[ ! -f ${required} ]]; then
        echo "missing staged deployment file: ${required}" >&2
        exit 1
    fi
done
if [[ ! -x ${sync_binary} ]]; then
    echo "staged release binaries must be executable" >&2
    exit 1
fi

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y postgresql postgresql-client ca-certificates curl gpg debian-keyring debian-archive-keyring apt-transport-https util-linux

caddy_repository_tmp=$(mktemp -d)
cleanup() {
    rm -rf -- "${caddy_repository_tmp}"
}
trap cleanup EXIT
curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location https://dl.cloudsmith.io/public/caddy/stable/gpg.key --output "${caddy_repository_tmp}/caddy.gpg.key"
gpg --batch --yes --dearmor --output /usr/share/keyrings/caddy-stable-archive-keyring.gpg "${caddy_repository_tmp}/caddy.gpg.key"
curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt --output "${caddy_repository_tmp}/caddy-stable.list"
install -o root -g root -m 0644 "${caddy_repository_tmp}/caddy-stable.list" /etc/apt/sources.list.d/caddy-stable.list
chmod 0644 /usr/share/keyrings/caddy-stable-archive-keyring.gpg
apt-get update
apt-get install -y caddy

if ! getent group bokheim >/dev/null; then
    groupadd --system bokheim
fi
if ! id -u bokheim >/dev/null 2>&1; then
    useradd --system --gid bokheim --home-dir /var/lib/bokheim --shell /usr/sbin/nologin bokheim
fi

# The large disk also serves existing applications. Sticky world-writable mode
# preserves their access while preventing unprivileged users from renaming or
# deleting Bokheim's protected child directory.
chmod 1777 /home/johan/data
install -d -o bokheim -g bokheim -m 0750 /home/johan/data/bokheim
install -d -o bokheim -g bokheim -m 0750 /var/lib/bokheim /var/lib/bokheim/assets

fstab_line='/home/johan/data/bokheim /var/lib/bokheim/assets none bind,nosuid,nodev,noexec 0 0'
if grep -Eq '^[^#[:space:]]+[[:space:]]+/var/lib/bokheim/assets[[:space:]]' /etc/fstab; then
    if ! grep -Fqx "${fstab_line}" /etc/fstab; then
        echo "refusing to replace an existing, different /var/lib/bokheim/assets mount in /etc/fstab" >&2
        exit 1
    fi
else
    printf '\n%s\n' "${fstab_line}" >>/etc/fstab
fi
if ! findmnt --mountpoint /var/lib/bokheim/assets >/dev/null 2>&1; then
    mount /var/lib/bokheim/assets
fi
findmnt --mountpoint /var/lib/bokheim/assets >/dev/null
install -d -o bokheim -g bokheim -m 0750 /var/lib/bokheim/assets/books /var/lib/bokheim/assets/thumbnails

systemctl enable --now postgresql.service
install -d -o root -g root -m 0700 /etc/bokheim
rm -f -- /etc/bokheim/disposable-email-domains \
    /usr/local/share/bokheim/disposable-email-domains
resend_credential=/etc/bokheim/resend-api-key
if [[ ! -s ${resend_credential} ]]; then
    echo "create ${resend_credential} with a Resend API key before installing" >&2
    exit 2
fi
chmod 0600 "${resend_credential}"
chown root:root "${resend_credential}"
account_token_credential=/etc/bokheim/account-token-key
if [[ ! -s ${account_token_credential} ]]; then
    umask 077
    openssl rand -base64 32 >"${account_token_credential}"
fi
chmod 0600 "${account_token_credential}"
chown root:root "${account_token_credential}"
admin_credential=/etc/bokheim/admin-password
if [[ ! -s ${admin_credential} ]]; then
    echo "create ${admin_credential} with the administrator password before installing" >&2
    exit 2
fi
chmod 0600 "${admin_credential}"
chown root:root "${admin_credential}"
test_user_credential=/etc/bokheim/test-user-password
if [[ ! -s ${test_user_credential} ]]; then
    echo "create ${test_user_credential} with the test account password before installing" >&2
    exit 2
fi
chmod 0600 "${test_user_credential}"
chown root:root "${test_user_credential}"
database_credential=/etc/bokheim/database-url
if [[ -f ${database_credential} ]]; then
    database_url=$(<"${database_credential}")
    db_password=${database_url#postgresql://bokheim_app:}
    db_password=${db_password%%@*}
    if [[ ! ${db_password} =~ ^[[:xdigit:]]{64}$ ]]; then
        echo "existing database credential has an unexpected format" >&2
        exit 1
    fi
else
    db_password=$(openssl rand -hex 32)
    database_url="postgresql://bokheim_app:${db_password}@127.0.0.1/bokheim"
    umask 077
    printf '%s\n' "${database_url}" >"${database_credential}"
fi
chmod 0600 "${database_credential}"
chown root:root "${database_credential}"

runuser -u postgres -- psql --set=ON_ERROR_STOP=1 postgres <<SQL
DO \$\$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'bokheim_app') THEN
        CREATE ROLE bokheim_app LOGIN PASSWORD '${db_password}';
    END IF;
END
\$\$;
ALTER ROLE bokheim_app WITH LOGIN PASSWORD '${db_password}';
SQL
unset db_password

# This is a development deployment target: every install wipes and recreates
# the database rather than preserving data across deploys, so there is no
# upgrade history to maintain -- `sync-server migrate` applies the current
# schema fresh each time. WITH (FORCE) drops the database even if the
# previous deploy's service is still holding connections open.
runuser -u postgres -- psql --set=ON_ERROR_STOP=1 postgres <<SQL
DROP DATABASE IF EXISTS bokheim WITH (FORCE);
CREATE DATABASE bokheim OWNER bokheim_app;
SQL
DATABASE_URL="${database_url}" "${sync_binary}" migrate
unset database_url

install -o root -g root -m 0755 "${sync_binary}" /usr/local/bin/sync-server

install -o root -g root -m 0644 "${source_root}/servers/sync/deploy/bokheim-sync.service" /etc/systemd/system/bokheim-sync.service
install -d -o root -g root -m 0755 /etc/systemd/system/caddy.service.d
install -o root -g root -m 0644 "${source_root}/servers/sync/deploy/caddy-bokheim.conf" /etc/systemd/system/caddy.service.d/bokheim.conf
install -o root -g root -m 0644 "${source_root}/servers/sync/deploy/Caddyfile" /etc/caddy/Caddyfile
install -o root -g root -m 0600 "${source_root}/servers/sync/deploy/caddy.env.example" /etc/bokheim/caddy.env
install -o root -g root -m 0755 "${source_root}/servers/sync/deploy/verify-public.sh" /usr/local/sbin/bokheim-verify-public

# Remove bootstrap artifacts left by deployments that created the former
# built-in administrator account.
rm -f -- /etc/bokheim/admin-password /etc/systemd/system/bokheim-sync.service.d/bootstrap-admin.conf
systemctl disable --now bokheim-gc.timer >/dev/null 2>&1 || true
rm -f -- /etc/systemd/system/bokheim-gc.service /etc/systemd/system/bokheim-gc.timer /etc/tmpfiles.d/bokheim.conf

systemctl daemon-reload
caddy validate --envfile /etc/bokheim/caddy.env --config /etc/caddy/Caddyfile
systemctl enable bokheim-sync.service caddy.service
systemctl restart bokheim-sync.service
systemctl restart caddy.service

for _ in $(seq 1 20); do
    if curl --fail --silent --show-error --unix-socket /run/bokheim/sync.sock http://localhost/api/ready | grep -Fqx ok; then
        sync_ready=true
        break
    fi
    sleep 1
done
if [[ ${sync_ready:-false} != true ]]; then
    echo "sync server did not become healthy" >&2
    systemctl --no-pager --full status bokheim-sync.service >&2 || true
    journalctl -u bokheim-sync.service -n 40 --no-pager -o cat >&2 || true
    exit 1
fi
curl --fail --silent --show-error http://127.0.0.1:8090/health | grep -Fqx ok

echo "Bokheim services installed and healthy locally."
echo "Public verification: sudo bokheim-verify-public api.bokheim.se"
