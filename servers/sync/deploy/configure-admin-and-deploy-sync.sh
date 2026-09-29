#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID} -eq 0 ]]; then
    echo "run this as the deployment user, not as root" >&2
    exit 2
fi
if [[ ! -r /dev/tty || ! -w /dev/tty ]]; then
    echo "an interactive terminal is required" >&2
    exit 2
fi
if [[ ! -x /usr/local/sbin/bokheim-deploy-sync ]]; then
    echo "the safe sync deployer is not provisioned" >&2
    exit 2
fi

# Authenticate before reading the new administrator password, so sudo never
# consumes the password intended for Bokheim from standard input.
sudo -v

admin_password=
admin_password_confirm=
cleanup() {
    unset admin_password admin_password_confirm
}
trap cleanup EXIT HUP INT TERM

if ! sudo test -s /etc/bokheim/admin-password; then
    while true; do
        read -r -s -p 'Bokheim admin password (minimum 12 characters): ' admin_password </dev/tty
        printf '\n' >/dev/tty
        read -r -s -p 'Confirm Bokheim admin password: ' admin_password_confirm </dev/tty
        printf '\n' >/dev/tty

        if [[ ${admin_password} != "${admin_password_confirm}" ]]; then
            echo "passwords do not match; try again" >&2
            continue
        fi
        character_count=${#admin_password}
        byte_count=$(LC_ALL=C; printf '%s' "${admin_password}" | wc -c)
        if (( character_count < 12 )); then
            echo "password must contain at least 12 characters" >&2
            continue
        fi
        if (( byte_count > 1024 )); then
            echo "password must not exceed 1024 bytes" >&2
            continue
        fi
        break
    done

    printf '%s\n' "${admin_password}" | sudo install -o root -g root -m 0600 /dev/stdin /etc/bokheim/admin-password
    unset admin_password admin_password_confirm
fi

sudo -n /usr/local/sbin/bokheim-deploy-sync
echo "Sign in at https://app.bokheim.se/admin/ as admin@bokheim.se."
