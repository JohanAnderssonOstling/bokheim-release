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
only an Argon2 password hash plus a keyed hash of a six-digit verification PIN.
Registering an address that is still pending replaces its password hash, so the
latest registrant's password wins. Verification requires that password together
with the PIN: the PIN proves control of the mailbox, the password proves the
verifier chose the account's password. PIN redemption
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

## Public account deletion

`https://bokheim.se/delete-account/` posts JSON to
`POST /api/auth/delete-account` with `email`, `password`, and
`confirmation: "DELETE"`. This standalone website endpoint requires the current
password; cookies and existing bearer sessions do not authorize deletion. It
allows five attempts per email and twenty per client address per five minutes.

The application's Settings → Account → Delete account action calls the same
endpoint with the current account email, password and explicit confirmation.
It clears persisted credentials only after the server confirms deletion, and
preserves device-local libraries.

Deletion removes the actual `users` row in a transaction. Foreign-key cascades
remove libraries, synced metadata and reading state, OPDS settings, sessions,
password-reset tokens, storage quota, upload reservations, and blob ownership
and access records. Pending registrations and queued account emails for that
email address are also removed. The identity cache is invalidated after commit
so cached access tokens stop working immediately. After commit, deletion wakes
asset cleanup. Shared book files are removed when no account owns them; covers
are removed when no live library in their account retains the book. Files
already downloaded to devices remain on those devices.

Permanent book deletion, library deletion and disabling cloud storage also wake
cleanup. Books in the trash retain their files so they can be restored. A sweep
runs at startup and every minute to retry failures and reclaim published files
left without ownership records. Uploads are staged before publication; database
advisory locks protect publication and ownership checks against concurrent
cleanup. No database migration is required for cleanup.

Deploy the sync server before publishing the static page in `bokheim-site`.

## Configured test account

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

Global deduplication is not an authorization capability. A book is charged to
an account only by that account uploading its complete BLAKE3-matching body, and
only a charged book can be downloaded. Synchronized lifecycle references are
client-declared and grant no access. A manifest entry the account has not
uploaded is reported as requiring upload whether or not another account has
stored the same hash; the store then keeps its single existing copy. An account
that drops its last reference to a book and later adds it back uploads it again.

Uploads require an exact `Content-Length`. The server reserves that many bytes
before storage, validates the completed asset, then atomically converts the
reservation into used bytes. Failed and abandoned reservations are released;
removing the user's final synchronized reference refunds the charge.
Synchronized values are canonical PostgreSQL state, with one `sync_state` row
per logical cell. Every kind, including reading positions, shares this table.
Rows retain opaque value bytes, the state address, book ownership, the original
four-part `VersionKey`, and a globally ordered `server_seq`. The server recognizes
book lifecycles for quota and lifecycle-first delivery; clients interpret the payload.
Winning updates allocate a delivery revision. Exact retries leave the committed
head unchanged; losing updates republish the stored winner so the sender converges.

Every synchronized wall-clock field uses the `UnixMillis` protocol type. It is
encoded as a plain integer but is explicitly measured in Unix milliseconds and
bounded to the signed 64-bit range shared by PostgreSQL and SQLite. The server
rejects LWW clocks beyond the allowed future-skew window.
A transition-table statement trigger maintains only the per-library committed
`current_revision`. The library lock is held before revision allocation and through
commit. Pulls deliver lifecycles before fields using the lifecycle and field cursors.
A purge removes every field owned by its book; placement registers describe
membership in a directory, and ordinary field edits do not change quota references.

Every typed account, library, preference, asset-manifest, and
synchronization body negotiates the versioned
`application/vnd.bokheim+protobuf; version=29` representation. The wire body
is a versioned protobuf envelope whose value preserves the Serde data model
and numeric enum variants. Bodies use identity encoding and retain the 8 MiB
request limit. JSON is neither accepted nor returned by this protocol;
unsupported media types, missing version negotiation, unsupported versions, and
compressed bodies fail before application logic runs. Book endpoints and thumbnail
GETs continue to transfer their exact bytes. Thumbnail PUTs use the typed pair
contract described below.
Library state values serialize their canonical domain types directly through
Serde into the generic protobuf envelope. There is no parallel protobuf domain
model or separate kind catalogue: entry kinds come from the canonical enum's
Serde variant names. The PostgreSQL relay retains opaque entry bytes, their state address, the
book each value belongs to (`content_hash`), and the original winning version.
A book lifecycle holds its book's bytes for quota unless its version is a
purge; the relay needs no other kind-specific facts. Publications add the publisher sequence for exact retry acknowledgements; deliveries add the server
revision for pagination. Forwarding never changes the original winning version.
Typed publications and versioned state define their wire serialization directly;
callers do not construct parallel transport envelopes. Book records contain the
canonical `BookDetails` plus their separately updated description. Reader
annotations are one flat `Annotation` carrying its id, book identity, content,
modification time, and deletion flag; the client's stored annotation blob
serializes that same type. Existing flat JSON and positional binary layouts
remain readable and writable without converting between mirrored value types.

Protocol 23 requires clients and servers to upgrade together. Clients retain a
read-only decoder for historical typed payloads already stored by the relay;
new writes use direct domain serialization. SQLite schema 22 removes UUID identities
from the outbox and authored versions. Queue generations use the publisher's monotonic
sequence; forwarded state retains its original authored version. Push responses list
only rejected sequences, and clients acknowledge exactly the submitted sequences
that were accepted or permanently rejected. Transient rejections remain queued.
Server schema 4 merges reading state into `sync_state` and removes unused channel
revision columns. The explicit upgrade command applies schema 1's owner migration,
schema 3's UUID removal, schema 4's row-preserving table merge, and schema 5's
content hash consolidation as needed. Schema 5 removes the book-to-blob revision
mapping and redundant claim hash; it rejects differing legacy mappings until
those identities have been migrated or the books reimported.
Unfrozen queue entries obtain their payload and version from canonical state.
Push and exchange pages are bounded to 1,000 mutations and clients also stop at
the 4 MiB encoded-mutation target, whichever limit is reached first.
Pull pages may cut across directory dependencies. SQLite clients therefore
materialize unresolved directories as hidden tombstoned placeholders and
activate the hierarchy only when its real parent state arrives. A child or book
placement can consequently commit on one page without violating foreign keys or
becoming visible before a dependency delivered by a later page.

Protocol 29 binds each pull cursor to a persisted server history UUID. The
generation is returned even for empty libraries and is committed with both
client cursor revisions. A different generation, or a nonzero checkpoint with
no generation, returns HTTP 410 before a push can commit. Clients use their
durable cursor recovery: pull from zero, then republish retained canonical
cells without changing their original authored versions. Server schema 11
adds the history singleton; repeated upgrades and ordinary startups keep its
ID. SQLite's history migration, introduced in schema 38, upgrades versions
36 and 37 without removing state or
outbox work. HTTP clients and servers must upgrade together; opaque domain
payloads stored with protocol 28 remain readable.

After restoring or replacing PostgreSQL history, run `sync-server
rotate-sync-generation` while synchronization traffic is stopped. This changes
only the history UUID, not values, authored versions, or revision numbers.
It must run after every restore, even when the restored revision head has
already passed clients' checkpoints. See the [restore procedure](deploy/README.md#database-recovery-and-synchronization-history).

Book and thumbnail routes retain their original bytes, content hashes, content lengths,
and byte-range semantics and never pass through the sync codec.

Books upload through individual `PUT /api/libraries/{library_id}/blobs/{hash}`
requests with an exact `Content-Length`. The URL hash identifies and verifies
the complete bytes; downloads use the same content hash as their ETag.
Upload negotiation remains batched, but file bytes have no batch framing or
book-batch endpoint. Native clients run at most four independent PUTs at a time;
each file commits and retries separately, regardless of its size. Browser
clients retain their individual OPFS upload path. Quota reservations and server
staging limits apply to each PUT.

Thumbnail upload and batch download entries carry both `bytes` (the full-size
image) and `browse_bytes` (the browse-size image), keyed by book identity. Clients
generate both variants. The server stores them in one `.pair` file published by
atomic rename, and presence revisions describe that entire pair. Downloads
return the stored images without resizing. Each variant is limited to 4 MiB;
batches allow 16 pairs and 128 MiB of image bytes, plus 1 MiB of wire overhead.
A single-thumbnail PUT accepts the same typed pair entry and requires its book
identity to match the URL; GET and HEAD still expose the full-size JPEG.

Existing loose thumbnail files are not treated as complete pairs by presence
or batch download. Clients with the source image reupload both variants; no
book-to-cover-hash mapping or server-side image conversion is introduced.

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
reset, or revoked sessions are closed. Native clients authenticate with an
Authorization bearer token. Browser WebSockets send the token through the
`bearer.*` subprotocol on `/api/notifications`; credentials are never included
in the URL or echoed as the negotiated subprotocol. The revalidation interval
accepts 5–300 seconds. HTTP requests use the same bearer-token API on every client;
there are no separate cookie-authenticated `auth/web/*` routes.

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

Schema 6 stores content hashes as 32-byte bytea values. The explicit schema
upgrade converts hexadecimal columns; transport version 26 and storage
version 15 require coordinated client/server updates. JSON remains hexadecimal.

Schema 7 also stores book register keys as raw digests and other register keys
as UTF-8 bytea values. JSON register addresses retain their existing strings.
