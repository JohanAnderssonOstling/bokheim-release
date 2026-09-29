#!/usr/bin/env bash
set -euo pipefail

# Run on the PostgreSQL server: sudo bash grant-johan-postgres-read.sh
if [[ ${EUID} -ne 0 ]]; then
    printf 'Run this script with sudo on the PostgreSQL server.\n' >&2
    exit 1
fi

command -v runuser >/dev/null
command -v psql >/dev/null
id johan >/dev/null

runuser -u postgres -- psql -X --dbname=bokheim --set=ON_ERROR_STOP=1 <<'SQL'
BEGIN;
DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'johan') THEN
        CREATE ROLE johan LOGIN;
    END IF;
END
$$;
ALTER ROLE johan LOGIN;
GRANT CONNECT ON DATABASE bokheim TO johan;
GRANT USAGE ON SCHEMA public TO johan;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO johan;
COMMIT;
SQL

printf '\nGranted johan read access to existing tables in bokheim.public.\n'
printf 'As johan, connect with: psql -d bokheim\n'
printf 'New or recreated tables need this script run again.\n'
printf 'Existing privileges and PostgreSQL authentication settings were preserved.\n'
