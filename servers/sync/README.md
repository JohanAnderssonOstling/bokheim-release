# Server architecture

The server is a composition binary over separately enforced account,
synchronization, storage, transport, and database boundaries:

```text
sync-server
├── server-account ───────────────────────> server-postgres
├── server-http
│   ├── server-account
│   ├── server-postgres ─────────────────> server-asset-store
│   ├── server-asset-store
│   └── server-wire-http
├── server-postgres
└── server-asset-store
```

- `server-account` owns the account boundary in internal `core`, `http`, and
  `postgres` modules. Synchronization receives only its `IdentityResolver`
  capability; account persistence owns password hashing, sessions, signup,
  rate limits, and the account-email outbox.
- `server-account` exposes a provider-neutral email outbox and sender trait. The
  composition binary runs a Resend adapter when its API key and sender address
  are configured; account persistence contains no provider credentials or HTTP
  implementation.
- `server-postgres` owns the shared PostgreSQL pool, readiness probe, ordered
  schema application, synchronization and quota persistence, atomic
  library-ownership authorization, asset upload orchestration, and PostgreSQL
  advisory-lock coordination. Its synchronization errors and database upload
  reservation types stay local to this crate. Account SQL remains inside
  `server-account`.
- `server-asset-store` implements content-addressed book and thumbnail storage
  and owns the asset kinds, stream types, and storage errors used at that
  boundary.
- `server-http` translates synchronization and blob requests to the concrete
  server services. It receives identity resolution but no
  credential-management service.
- `sync-server` reads configuration and wires the concrete services into the
  HTTP router. It is the only crate allowed to depend on all adapters.

Account and synchronization PostgreSQL code remain separate modules over the
shared `server-postgres::PostgresDatabase` capability and do not share
credential or synchronization persistence.

## Boundary enforcement

- Every server package is `publish = false` and inherits workspace lints that
  forbid unsafe code, unreachable public items, and mismatched public interfaces.
- Account and synchronization HTTP modules expose only router constructors. Their
  Axum states, handlers, extractors, and wire DTOs remain module-internal.

Run `cargo test --manifest-path servers/sync/Cargo.toml --workspace` to execute the
server tests.

## Public registration

Public signup is always exposed. Email addresses are validated syntactically and
must be verified before an account becomes active.

Set `BOKHEIM_RESEND_API_KEY` and `BOKHEIM_EMAIL_FROM` together to enable the
built-in Resend worker. The API key may instead be supplied as the
`resend_api_key` systemd credential. The worker claims `account_email_outbox`
rows, uses each row ID as Resend's idempotency key, removes successful sends,
and retries failures without logging recipients or one-time tokens. If neither
setting is present, the server starts but leaves account emails queued.

`BOKHEIM_ACCOUNT_TOKEN_KEY` or the `account_token_key` systemd credential is
always required and must contain a base64-encoded 32-byte key. The key is kept
separate from PostgreSQL: verification PINs and password-reset tokens use
domain-separated HMAC-SHA256 verifiers, while recoverable outbox values use
authenticated ChaCha20-Poly1305 encryption bound to the row ID, recipient, and
message kind. Successful delivery deletes the outbox row.

Registration reserves the canonical email and username for 24 hours and stores
only an Argon2 password hash plus a keyed hash of a six-digit verification PIN. PIN redemption
is scoped to the normalized email address and persistently limited to five attempts per
address per hour, in addition to the network limit. Verification
is single-use and creates a one-hour access token plus a 90-day rotating refresh
session. Password-reset tokens live for one hour, are single-use, and revoke all
sessions when consumed. Registration,
resend, verification, login, and reset routes use PostgreSQL-backed IP and
subject buckets, so limits survive restarts and work across server processes.

Requests for existing accounts and unknown password-reset emails deliberately
return the same `202 Accepted` response. Disposable-domain policy rejection and
malformed email/password inputs return `400` because they reveal no account
existence.

## Administrator dashboard

The dedicated administrator web application is deployed separately from the
GPUI WebAssembly frontend and exposed at `/admin`. Configure it
with `BOKHEIM_ADMIN_EMAIL` and either `BOKHEIM_ADMIN_PASSWORD` or the
`admin_password` systemd credential. Administrator status is stored explicitly
on the account and checked from PostgreSQL on every dashboard request; the
ordinary configured test account is not an administrator.

The dashboard stores 90 days of minute aggregates containing only bounded
route categories, HTTP method and status class, counts, declared transfer
bytes, and latency totals. For incident response, a separate request log keeps
the exact trusted client IP, bounded route category, method, status class,
declared bytes, and latency for seven days. Neither store contains raw paths,
account or email identifiers, headers, tokens, query strings, or bodies. Dashboard reads are
persistently rate-limited. The first read of each dashboard resource by an
administrator in an hour is appended to a database-enforced immutable audit
log. Browser reads require the secure session cookie and
`Sec-Fetch-Site: same-origin`; bearer authentication remains available for
trusted operational clients. Admin responses are the only JSON API in this
service; synchronization and account contracts remain versioned binary wire
messages.

`GET /api/admin/services` collects bounded JSON operational snapshots from the
local authority and metadata services for the same authenticated dashboard.
The default targets are `http://127.0.0.1:8090/` and
`http://127.0.0.1:8091/`. `BOKHEIM_ADMIN_AUTHORITY_URL` and
`BOKHEIM_ADMIN_METADATA_URL` may override them with literal loopback HTTP base
URLs, or an empty value may disable a section. Redirects, non-loopback targets,
credentials, queries, responses over 1 MiB, and requests exceeding five seconds
are rejected. The browser never connects to either internal service directly.

Successful authenticated token use is batched into one lifetime activity row
per account and one aggregate row per active account per UTC day. The dashboard
reports daily, weekly, and monthly active accounts; new verified accounts;
first-time and returning accounts; and whether active accounts used sync or
only authenticated. It also exposes a 30-day aggregate history without account
identities. Daily account rows expire after 90 days, and administrator-dashboard
reads do not count as engagement. Registrations that have not been verified and
failed authentication attempts do not count as account use.

For development deployments, `BOKHEIM_TEST_USER_EMAIL` together with either
`BOKHEIM_TEST_USER_PASSWORD` or the `test_user_password` systemd credential
upserts a verified account at startup. The configured password is validated by
the normal account policy. Deployment assigns that account a fixed 1000 GB
storage quota; password changes revoke its existing sessions.

## Authentication lookup cache

Successful access-token resolution is cached inside the account component so a
hot synchronization exchange needs only its synchronization SQL statement. The
cache stores the SHA-256 token digest, authenticated identity, owning session,
and absolute token expiry; it never stores the raw bearer token. It is bounded
to 65,536 entries, has a maximum 30-second residency, and still rejects an entry
immediately at the token's absolute expiry.

Refresh rotation, refresh-token reuse, logout, password reset, individual
session revocation, and bulk revocation invalidate affected entries after the
database transaction commits. A cache revision closes the query/revocation race:
a lookup begun before invalidation cannot repopulate its result afterward.

The current deployment has one active synchronization-server process, so these
service-level invalidations are immediate. Before horizontally running multiple
server processes, add shared invalidation delivery (for example PostgreSQL
`LISTEN`/`NOTIFY`) or accept the bounded 30-second cross-process cache window.

## Asset quota and transfer fairness

Book blobs and thumbnails share a PostgreSQL account quota ledger. Physical book
files remain globally deduplicated by content hash, while each user is charged
once for every unique book hash referenced anywhere in that user's libraries.
Thumbnails are physically and financially scoped to the account and are charged
once per unique thumbnail hash. Metadata does not consume storage quota. New
accounts receive a 5 GiB quota lazily.

Global deduplication is not an authorization capability. A stored book can be
claimed or downloaded only when the authenticated account already owns it or
has a synchronized lifecycle reference to its hash. An unreferenced manifest
entry is reported as requiring upload whether or not another account has stored
the same hash; the upload path verifies the complete BLAKE3-matching body.

Uploads require an exact `Content-Length`. The server reserves that many bytes
before storage, validates the completed asset, then atomically converts the
reservation into used bytes. Failed and abandoned reservations are released;
removing the user's final synchronized reference refunds the charge.
Synchronized values are canonical PostgreSQL state, not a JSONB event archive.
Each domain table owns its complete value and synchronization metadata: the
total `VersionKey`, actor identity, and a globally ordered `server_seq`. The
server stores and relays that state; it does not interpret it. Directories
and physical placements keep a handful of typed columns because the server
itself needs them (referential existence checks, and content-hash deltas
computed transactionally for blob quota accounting); every other domain's
payload -- book lifecycle, metadata, and annotations -- is an opaque
wire-encoded `value` column the server never reads
inside of. Adding or changing a synchronized field on those domains never
touches this schema; only the client interprets what is inside. Pulls merge
the indexed directory, book-lifecycle, physical-placement, reading-position,
annotation and metadata tables. Winning LWW updates
redraw the global sequence so offline clients receive the new value, while
retries and losing versions do not grow storage or redraw the cursor. A push
therefore needs only one row write, with no shared envelope join.
Every synchronized wall-clock field uses the `UnixMillis` protocol type. It is
encoded as a plain integer but is explicitly measured in Unix milliseconds and
bounded to the signed 64-bit range shared by PostgreSQL and SQLite. The server
rejects LWW clocks beyond the allowed future-skew window.
Per-library reading and general-state revision heads are maintained by
transition-table statement triggers. A scanner batch advances each head once
per non-empty typed-state statement rather than rewriting the same head row for
every book property in that statement.

A newly scanned book publishes two state cells: lifecycle and physical
placement. Lifecycle carries the title and ordered author credits as initializer metadata, avoiding
a redundant metadata mutation for every ingest. It only fills a client row that
does not have metadata yet. Subsequent title or author-list edits—including changes
made while reactivating an existing book—continue to use the independent
metadata register, so those edits cannot accidentally change deletion or
resurrection ordering.

Physical placement is a path register: additions compete for a directory/name
path and removals affect only its current occupant. The canonical placement
upsert returns its winning old/new content hashes directly, so per-user blob
reference counts are updated without a second path projection. Ordinary state
updates such as reading positions do not touch placement or blob references.

Every typed account, library, preference, asset-manifest, and
synchronization body negotiates the versioned
`application/vnd.bokheim+protobuf; version=18` representation. The wire body
is a versioned protobuf envelope whose value preserves the Serde data model
and numeric enum variants. Bodies use identity encoding and retain the 8 MiB
request limit. JSON is neither accepted nor returned by this protocol;
unsupported media types, missing version negotiation, unsupported versions, and
compressed bodies fail before application logic runs. Raw book and thumbnail
endpoints continue to transfer their exact validated bytes.
Mutation `value` fields use the explicitly typed protobuf `MutationValue`
schema. The PostgreSQL relay stores those protobuf bytes without interpreting
them, keeping the service independent of client domain models. Protobuf field
numbers and enum values are stable from protocol version 14 onward; compatible
changes are additive and removed field numbers must never be reused.
Push and exchange pages are bounded to 1,000 mutations and clients also stop at
the 4 MiB encoded-mutation target, whichever limit is reached first.
Pull pages may cut across directory dependencies. SQLite clients therefore
materialize unresolved directories as hidden tombstoned placeholders and
activate the hierarchy only when its real parent state arrives. A child or book
placement can consequently commit on one page without violating foreign keys or
becoming visible before a dependency delivered by a later page.

Book and thumbnail routes retain their original bytes, content hashes, content lengths,
and byte-range semantics and never pass through the sync codec.

Upload and download traffic have independent, work-conserving deficit-round-
robin schedulers. Scheduling is by authenticated user rather than connection,
so parallel transfers cannot increase a user's share under contention. Configure
the global ceilings and burst allowance with:

```text
BOKHEIM_UPLOAD_BYTES_PER_SECOND=104857600
BOKHEIM_DOWNLOAD_BYTES_PER_SECOND=104857600
BOKHEIM_TRANSFER_BURST_BYTES=1048576
```

A direction set to `0` is deliberately unlimited. Defaults are 100 MiB/s in
each direction with a 1 MiB burst.

## Operational limits

The PostgreSQL pool, requests, notifications, transfers, and staged uploads
have bounded defaults with optional environment overrides:

```text
BOKHEIM_DATABASE_MAX_CONNECTIONS=32
BOKHEIM_DATABASE_MIN_CONNECTIONS=2
BOKHEIM_DATABASE_ACQUIRE_TIMEOUT_SECONDS=5
BOKHEIM_DATABASE_IDLE_TIMEOUT_SECONDS=600
BOKHEIM_MAX_STAGED_BOOK_UPLOADS=16
BOKHEIM_MAX_STAGED_BOOK_UPLOADS_PER_ACCOUNT=4
BOKHEIM_MAX_IN_FLIGHT_REQUESTS=512
BOKHEIM_MAX_THUMBNAIL_UPLOADS=16
BOKHEIM_MAX_THUMBNAIL_UPLOADS_PER_ACCOUNT=4
BOKHEIM_MAX_IN_FLIGHT_THUMBNAIL_BYTES=536870912
BOKHEIM_MAX_IN_FLIGHT_THUMBNAIL_BYTES_PER_ACCOUNT=268435456
BOKHEIM_NOTIFICATION_HEARTBEAT_SECS=60
BOKHEIM_NOTIFICATION_TIMEOUT_SECS=150
BOKHEIM_NOTIFICATION_AUTH_REVALIDATION_SECS=30
BOKHEIM_MAX_NOTIFICATION_CONNECTIONS=100000
BOKHEIM_MAX_NOTIFICATION_CONNECTIONS_PER_ACCOUNT=16
```

Database maximum/minimum connections accept `2..=256` and `0..=max`
respectively. Acquire timeout accepts 1–60 seconds and idle timeout accepts 30
seconds–24 hours. Staged uploads are
bounded both by count and declared bytes before temporary files are created;
the defaults allow 16 files/4 GiB globally and 4 files/2 GiB per account.
Capacity exhaustion returns HTTP 429 before the asset store reads the body.
Thumbnail uploads, including batches, have separate count and declared-byte
ceilings: 16 requests/512 MiB globally and 4 requests/256 MiB per account.
They return HTTP 429 without reading the body when any ceiling is exhausted
and have a two-minute total deadline. Exact `Content-Length` is required so
the complete request can be charged to the in-flight byte budget before it is
read. Book uploads retain their two-hour deadline.
Defaults are intended
for the current single-process deployment; raise them only after measuring
PostgreSQL, CPU, memory, and disk saturation together.
The HTTP ceiling includes requests executing handlers and requests waiting for
downstream capacity. Its default of 512 absorbs short reconnect bursts, while
the outer request deadline bounds time spent waiting. It does not increase
steady-state database throughput.

Notification WebSockets revalidate their access token every 30 seconds and
before sending account notifications or heartbeat pings. Expired, logged-out,
reset, or revoked sessions are closed. Browser connections authenticated by
the secure access-token cookie must supply an HTTP(S) `Origin` whose authority
exactly matches `Host` (and whose scheme matches Caddy's
`X-Forwarded-Proto`). Native clients authenticate with a bearer token and do
not need an `Origin` header. The revalidation interval accepts 5–300 seconds.

## Asset storage configuration

Local development stores assets in `./books` and `./thumbnails`. Production
deployments configure their common parent as an absolute, normalized path:

```text
BOKHEIM_ASSETS_DIR=/var/lib/bokheim/assets
```

The server derives `books/` and `thumbnails/` beneath that root. A relative or
non-normalized production root is a startup error. Before accepting traffic,
the server creates both directories and verifies that each supports create,
write, sync, and delete operations. See [`deploy/README.md`](deploy/README.md) for the bind
mount that safely exposes `/home/johan/data/bokheim` through the systemd home
sandbox.

Clients upload covers separately so a device can browse books without first
downloading them. Covers are private account data rather than globally shared
blobs: storage paths include the authenticated user. The server streams the
client's original bytes without image decoding or
re-encoding or format inspection, while enforcing a 4 MiB transfer limit.
Responses use `image/jpeg`, `X-Content-Type-Options: nosniff`, and
`Cache-Control: private, no-store` because cover bytes can change under a book
identity. Clients validate and decode within bounded image limits
before display. Covers from the former global layouts remain readable and are
copied unchanged into each user's scope on first access, so an upgrade does not
blank existing cloud covers.

## Network transport

The server defaults to plain HTTP on `127.0.0.1:8080` for local development and
tests. A non-loopback TCP binding is rejected unless explicitly acknowledged
with `SYNC_ALLOW_INSECURE_HTTP=true`.

The production deployment terminates public HTTPS in Caddy and uses a
permission-restricted Unix socket for the application boundary:

```text
SYNC_SERVER_SOCKET=/run/bokheim/sync.sock
```

Unix-socket mode trusts the single sanitized `X-Forwarded-For` value supplied by
Caddy. TCP mode always ignores forwarding headers and uses the direct peer
address. Embedded certificate management is deliberately not part of the
application. See [`deploy/README.md`](deploy/README.md) for the complete public
Internet deployment.

## Local server stress test

`tests/run_server_stress.sh` provisions a throwaway PostgreSQL cluster, starts
the real HTTP server on loopback, and exercises concurrent synchronization
through authentication, HTTP, PostgreSQL, and response decoding. It compares
independent-library writes with intentionally serialized same-library writes,
coalesced reading-position register updates, complete single-request position
exchanges, concurrent full pulls, and concurrent validated EPUB transfers.
It verifies that every acknowledged event is observable and every downloaded
blob exactly matches its upload:

```sh
servers/sync/tests/run_server_stress.sh
```

The harness reports throughput and p50/p95/p99/max request latency. Workload
size can be changed with `BOKHEIM_STRESS_WORKERS`,
`BOKHEIM_STRESS_REQUESTS_PER_WORKER`, and
`BOKHEIM_STRESS_EVENTS_PER_REQUEST`. Validated EPUB payload size is controlled
by `BOKHEIM_STRESS_BLOB_BYTES`. All values are bounded, and the workload binary
refuses non-loopback URLs so it cannot accidentally load-test the public service.

`BOKHEIM_STRESS_MODE=ingest` uses the production protobuf sync transport.

`BOKHEIM_STRESS_MODE=inventory` measures complete library state-inventory
reconciliation through the production HTTP and PostgreSQL path. It profiles
all-present, one-missing, and one-percent-missing inventories, validates every
returned cell, and reports cycle/request latency plus wire volume. Configure it
with `BOKHEIM_INVENTORY_CELLS` (default 10,000) and
`BOKHEIM_INVENTORY_REPETITIONS` (default 5):

```sh
BOKHEIM_STRESS_MODE=inventory BOKHEIM_INVENTORY_CELLS=100000 \
    servers/sync/tests/run_server_stress.sh
```
