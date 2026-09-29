# Public Internet deployment

## Remote code deployment

After the host has been prepared once with `install-server.sh`, deploy a new
release from this checkout with:

```sh
servers/sync/deploy/deploy-remote.sh
```

If remote sudo must be entered from a shell on the server, stage the release
without attempting installation:

```sh
servers/sync/deploy/deploy-remote.sh --stage-only
```

The command prints the exact path of a self-contained privileged entry point.
SSH to the deployment host and run that command, for example:

```sh
sudo /tmp/bokheim-deploy.ABC123/deploy-sudo.sh
```

`deploy-sudo.sh` takes no arguments. It validates the staged artifacts and
credentials and the installed schema before stopping services.

The script builds `sync-server`, transfers its release artifact and systemd
unit over SSH, and invokes the fixed root-owned sync activation helper. It
runs a schema check and never recreates the PostgreSQL database. Provision a
fresh database with `sync-server migrate` before the first release. Catalogue
and metadata services are deployed independently. The
script defaults to `192.168.1.68`;
override the SSH account when needed:

```sh
BOKHEIM_DEPLOY_USER=johan servers/sync/deploy/deploy-remote.sh
```

Provision the non-destructive sync activation helper once on the host:

```sh
sudo servers/sync/deploy/provision-server-deploy.sh johan
```

Reconnect SSH afterward. The provisioner grants passwordless sudo only for
`/usr/local/sbin/bokheim-deploy-sync`; staged shell scripts are never run as
root. Routine deployments then need only the SSH key. The script does not
modify Caddy, replace credentials, delete asset files, or reset the database.

### Debian-compatible release binaries

The production host is Debian 11-era and may provide an older glibc than the
development machine. Do not copy host-native release binaries when the build
machine reports a newer glibc requirement; a binary requiring `GLIBC_2.43`, for
example, will not start on the host.

PDFium dynamically and must use a compatible GNU build environment:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl -p sync-server
python3 shared/pdfium/prepare.py --target x86_64-unknown-linux-gnu \
  --dest target/x86_64-unknown-linux-gnu/release/pdfium --offline
cargo build --release --target x86_64-unknown-linux-musl -p metadata-server
```

The release helper currently builds host-native artifacts itself. When
deploying from a different libc environment, stage the musl artifacts through
the target host's passwordless activation workflow, or update the helper to
build and select the configured target. Never activate a binary until its
linkage has been checked against the target host.

This deployment exposes only Caddy to the Internet. The Rust server accepts
requests through `/run/bokheim/sync.sock`, whose filesystem permissions allow
access only to the `bokheim` user and members of the `bokheim` group. PostgreSQL
remains reachable only on localhost.

## Host preparation

Install PostgreSQL and Caddy from their official packages, then create the
unprivileged application account:

```sh
sudo useradd --system --home-dir /var/lib/bokheim --shell /usr/bin/nologin bokheim
sudo install -d -o bokheim -g bokheim -m 0750 /var/lib/bokheim
sudo install -d -o root -g root -m 0700 /etc/bokheim
```

### Large-disk asset storage

Keep the systemd home-directory sandbox enabled. Store the physical assets on
the large disk and bind-mount them into the service's state directory instead
of giving the service access to `/home`:

```sh
sudo chmod 0755 /home/johan/data
sudo install -d -o bokheim -g bokheim -m 0750 /home/johan/data/bokheim
sudo install -d -o bokheim -g bokheim -m 0750 /var/lib/bokheim/assets
```

Add this persistent mount to `/etc/fstab`:

```fstab
/home/johan/data/bokheim /var/lib/bokheim/assets none bind,nosuid,nodev,noexec 0 0
```

Activate and verify it before starting Bokheim:

```sh
sudo mount /var/lib/bokheim/assets
findmnt --mountpoint /var/lib/bokheim/assets
sudo install -d -o bokheim -g bokheim -m 0750 /var/lib/bokheim/assets/books /var/lib/bokheim/assets/thumbnails
```

The service unit requires this mount and refuses to start if it is absent. Its
absolute `BOKHEIM_ASSETS_DIR` value prevents the process working directory from
deciding where uploads are stored. Application
startup also probes both directories for create, write, sync, and delete access.

Build the release executable and install it without making it writable by the
service account:

```sh
cargo build --release --manifest-path servers/sync/Cargo.toml -p sync-server
sudo install -o root -g root -m 0755 servers/sync/target/release/sync-server /usr/local/bin/sync-server
```

Create `/etc/bokheim/database-url` containing a PostgreSQL URL that resolves to
localhost. Protect the file:

```sh
sudo chmod 0600 /etc/bokheim/database-url
sudo chown root:root /etc/bokheim/database-url
```

The database credential does not create a built-in administrator account or
contain an administrator password.

Create a separate persistent 32-byte key for account PIN, password-reset, and
mail-outbox protection. Do not replace this key while pending registrations,
password resets, or mail rows exist:

```sh
openssl rand -base64 32 | sudo tee /etc/bokheim/account-token-key >/dev/null
sudo chmod 0600 /etc/bokheim/account-token-key
sudo chown root:root /etc/bokheim/account-token-key
```

The service receives it through the read-only `account_token_key` systemd
credential; it is not stored in PostgreSQL.

After the non-destructive sync deployer has been provisioned, configure the
administrator password and activate a staged sync release without putting the
password in shell history:

```sh
servers/sync/deploy/configure-admin-and-deploy-sync.sh
```

The built-in defaults use 32 maximum and 2 warm PostgreSQL connections,
16 staged books/4 GiB globally, 4 staged books/2 GiB per account, and at most
16 concurrent thumbnail uploads globally/4 per account, with additional
declared-byte ceilings of 512 MiB globally/256 MiB per account. Thumbnail
uploads have a two-minute total deadline. Override `BOKHEIM_DATABASE_*`,
`BOKHEIM_MAX_STAGED_*`, `BOKHEIM_MAX_THUMBNAIL_UPLOADS*`, or
`BOKHEIM_MAX_IN_FLIGHT_THUMBNAIL_BYTES*` in a systemd drop-in only after
measuring production saturation; a larger database pool consumes more
PostgreSQL memory.

## Public registration

Public registration is always enabled and requires email verification. The
sync server delivers verification and password-reset messages through Resend,
using its transactional PostgreSQL outbox for retries and idempotency.

Add and verify `mail.bokheim.se` in Resend, then publish the DNS records Resend
provides. The checked-in service sends as
`Bokheim <accounts@mail.bokheim.se>`; change `BOKHEIM_EMAIL_FROM` in the unit if
a different verified domain is chosen. Create a sending-only API key in Resend
and install it on the host without a trailing command or variable name:

```sh
sudo install -d -o root -g root -m 0700 /etc/bokheim
sudo install -o root -g root -m 0600 resend-api-key /etc/bokheim/resend-api-key
```

The systemd unit exposes that file to the service as a read-only credential.
The service is allowed outbound network access because Resend's HTTPS endpoint
does not have stable addresses suitable for `IPAddressAllow`; PostgreSQL and
the sync HTTP listener remain local-only.

## Configured test account

The checked-in service configures `test@bokheim.se` at startup with a 1000 GB
storage quota. Store its password in a second protected credential:

```sh
sudo install -o root -g root -m 0600 test-user-password /etc/bokheim/test-user-password
```

The credential file contains only the password and must not be empty. The
configured test user is exempt from the public-registration password minimum.
Change `BOKHEIM_TEST_USER_EMAIL` in the unit to use a different identifier.
Changing the credential rotates the password at the next
restart and revokes that account's existing sessions; an unchanged password
preserves sessions.

## Administrator dashboard

The checked-in service configures a separate `admin@bokheim.se` account for
the read-only dashboard at `https://app.bokheim.se/admin/`. Its static source
lives in `servers/admin-web` and is not compiled into the mobile/reader app. Install a strong,
unique password in its own credential before deployment:

```sh
sudo install -o root -g root -m 0600 admin-password /etc/bokheim/admin-password
```

Change `BOKHEIM_ADMIN_EMAIL` in the unit if another address is required. The
administrator password must satisfy the public account password policy. On
startup, an unchanged password preserves sessions; changing the credential
rotates the password and revokes existing administrator sessions. Do not reuse
the test-account, database, Resend, or account-token credentials.

Dashboard traffic history is aggregated by minute and retained for 90 days.
A separate incident-response log retains exact trusted client IP addresses and
bounded request metadata for seven days. It contains no raw paths, account
identifiers, query strings, request headers, or bodies. The first read of each dashboard
resource by an administrator in an hour is append-audited, and the audit table
rejects updates and deletion.

Unique-account figures count successful authenticated token use and are shown
for 24-hour, 7-day, 30-day, and all-time windows. Activity updates are batched
and stored as one row per account rather than one row per request.

## Install service configuration

```sh
sudo install -o root -g root -m 0644 servers/sync/deploy/bokheim-sync.service /etc/systemd/system/bokheim-sync.service
sudo install -d -o root -g root -m 0755 /etc/systemd/system/caddy.service.d
sudo install -o root -g root -m 0644 servers/sync/deploy/caddy-bokheim.conf /etc/systemd/system/caddy.service.d/bokheim.conf
sudo install -o root -g root -m 0644 servers/sync/deploy/Caddyfile /etc/caddy/Caddyfile
sudo install -o root -g root -m 0600 servers/sync/deploy/caddy.env.example /etc/bokheim/caddy.env
```

Edit `/etc/bokheim/caddy.env` and replace both example values. Point the domain's
public A and/or AAAA records at this host. Permit inbound TCP ports 80 and 443,
but do not expose PostgreSQL or any Rust application port. Point DNS directly
at this host; adding a CDN or another proxy requires an explicit review of the
forwarded-address trust chain.

Validate and start the deployment:

```sh
sudo systemctl daemon-reload
sudo caddy validate --envfile /etc/bokheim/caddy.env --config /etc/caddy/Caddyfile
sudo systemctl enable --now bokheim-sync.service
sudo systemctl enable caddy.service
sudo systemctl restart caddy.service
curl --fail https://api.bokheim.se/api/health
curl --fail https://meta.bokheim.se/health
```

The expected health response is `ok`. Bokheim clients use the fixed production
origin `https://api.bokheim.se`; there is no end-user server URL setting.

`/api/live` reports only that the process is serving HTTP. `/api/ready` and the
backwards-compatible `/api/health` additionally require a working PostgreSQL
connection and writable, syncable book and thumbnail storage. Deployment and
monitoring must use readiness when deciding whether the server can accept work.
Readiness results are cached for five seconds so frequent or concurrent probes
cannot amplify into repeated durable writes on the asset volume.

Run the complete deployment gate after DNS and certificate issuance settle:

```sh
sudo servers/sync/deploy/verify-public.sh api.bokheim.se meta.bokheim.se
```

## Deploy the GPUI web application

Point `app.bokheim.se` at the same host as `api.bokheim.se`, then provision the
deployment account once on the host:

```sh
sudo servers/sync/deploy/provision-web-deploy.sh johan
```

Reconnect SSH so the new group membership applies. Future static application
deployments build the GPUI WebAssembly target, need no root access, and do not restart either Rust service,
PostgreSQL, or Caddy:

```sh
servers/sync/deploy/deploy-web-remote.sh
```

The installer atomically switches `/usr/local/share/bokheim-web/current` and
rolls back if the public application cannot be fetched. Caddy follows that
symlink and proxies same-origin `/api/*` requests to the sync Unix socket.

It verifies both services, the Caddy configuration, Unix-socket permissions,
HTTP-to-HTTPS redirection, the HTTPS health response, HSTS, certificate validity,
and that neither port 8080 nor PostgreSQL is publicly listening.

## Verification and maintenance

Inspect startup and certificate issuance without printing credentials:

```sh
systemctl status bokheim-sync.service caddy.service
journalctl -u bokheim-sync.service -u caddy.service --since today
```

If the large disk is unavailable, `bokheim-sync.service` deliberately remains
stopped. Check the mount before attempting to restart it:

```sh
findmnt --mountpoint /var/lib/bokheim/assets
systemctl status var-lib-bokheim-assets.mount bokheim-sync.service
```

The HTTP edge and Rust service bound request headers, stalled request bodies,
total request duration, and concurrent in-flight requests. systemd also caps
memory, tasks, and file descriptors. SIGTERM stops new connections and gives
active requests 30 seconds to drain before systemd enforces shutdown.

Upgrade by installing a newly built executable and restarting only the Rust
service. Reload Caddy configuration without downtime:

```sh
sudo systemctl restart bokheim-sync.service
sudo systemctl reload caddy.service
```

To validate, install, reload, and verify an updated Caddyfile in one step, copy
the deployment directory to the server and run:

```bash
sudo ./install-caddy-config.sh
```

The installer retains the previous `/etc/caddy/Caddyfile` as a timestamped
backup and restores it automatically if the reload or metadata health check
fails. It reloads only Caddy and does not restart the Bokheim services.

## No backups: the database is disposable

This is a development deployment target, not a production one. `install-server.sh`
drops and recreates the `bokheim` database on every deploy, so there is no
schema upgrade history and nothing to back up -- redeploying is the recovery
procedure. Do not point this at a deployment expected to hold data that must
survive a redeploy without a design change to persist it first.
