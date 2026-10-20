# Real-server convergence tests

Run from the repository root:

```sh
bash client/app/tests/run_sync_e2e.sh
```

The runner requires Rust, PostgreSQL command-line tools (`initdb`, `pg_ctl`,
`createdb`), `curl`, and `rg`. It builds and runs **release** binaries only,
starts a temporary PostgreSQL database and HTTP sync server, runs the server
integration tests, and then runs the client convergence tests. Temporary
processes and data are cleaned up on exit. `TMPDIR` selects the temporary data
location. `SYNC_E2E_DATABASE_URL`, if supplied, must name a disposable test
database; the server tests apply schemas there.

The current real-server client regression lives in
`client/library-backend/crates/library-sync/src/engine_tests.rs`. It uses the
production sync engine, HTTP transport, SQLite databases and PostgreSQL service
to verify directory creation, a rename sent back from a second device, database
restart, empty outboxes and a stable extra synchronization. Each run creates an
isolated library. Server account, database and registration integration tests
also run against the temporary PostgreSQL instance.

## Historical pre-refactor scenarios

The scenarios below document the former client convergence suite, which was
removed during the storage refactor. They are not claims about the current
real-server test's coverage. Current database merge and sync-engine regressions
also run in the shared suite.

Coverage:

- An offline client misses two complete purge/readd cycles and returns with
  previously acknowledged annotation state but no pending publication.
- Concurrent legal folder moves create a cycle when combined, alongside a book
  move, trash/restore and competing annotation edits/deletion.
- The server commits an annotation but its response is lost; after another
  client purges the book, the restarted sender retries the original publication.
- A response from the old book lifetime is delivered after purge/readd; more
  than 1,000 canonical changes force actual push batching and pull pagination.
- Concurrent purge and restore are delivered in both orders, with a fresh
  observer joining between exchanges and both authors restarting with outboxes.

The bounded settling loop compares full canonical winner identities and bodies,
folder intents and repaired trees, book projections, placements, and annotations.
Device-local physical filenames, file fingerprints, download flags, row IDs and cleanup work are
excluded because they describe local storage. A fresh client participates in
every scenario. Every visible folder must reach the root without cycles or missing parents.
All outboxes must be empty and delivery cursors must agree. An additional sync
must leave snapshots and cursors unchanged. Failure to settle within 12 rounds fails the
test rather than hiding a publication loop.

These are deterministic scenario tests, not an exhaustive proof or a randomized
network simulator. They cover client database restarts, not server process
crashes or filesystem cleanup completion. The tests intentionally do not require
old annotations to survive a purge/readd; they require all clients to agree.

## Regression exposed by this suite

`production_http_postgres_concurrent_restore_before_losing_purge_converges`
exercises a restore whose version beats a concurrent purge. Both clients had
already pulled the old book fields. The restoring client publishes first; the
purging client has physically removed its book and canonical fields, then pushes
its losing purge. Previously the server republished only the winning lifecycle, leaving unchanged
book facts and metadata behind that client's cursor. Repeated exchanges could
not repair the incomplete book. The server now republishes every current
book-owned field alongside the lifecycle, advancing delivery revisions in both
channels while preserving the original values and conflict versions.

The test fixes authoring times through the local SQL clock, uses normal mutation
triggers, and asserts that restore wins in both delivery orders. The purge-first
case is separate so either regression can fail independently. Both orders must converge with empty outboxes and a stable extra sync.
A PostgreSQL regression additionally checks all book-owned field kinds, unchanged
winner identities, unrelated-book isolation, rejected purges and duplicate
suppression.

## Trash membership

Book trashing preserves present folder placements and changes the book lifecycle.
Restore uses those retained placements, without timestamp-based inference.
Explicit folder removals remain removed. A restore to another folder is an
explicit placement move. Scanner and filesystem queries exclude trashed books,
and their local downloaded flags are cleared without changing placement intent.

Database regressions also exercise concurrent Trash and move/copy operations,
including folder Trash, in opposite delivery orders with duplicate replay.
Book lifecycle controls visibility even when a concurrent move leaves a retained
placement outside the trashed folder. Ordinary restore preserves placement
versions and local file metadata; recovery may resend the original versions.
A concurrent placement removal survives restore. If that removal arrives after
planning but before commit, the stale restore plan fails atomically and must be
rebuilt from the remaining placements.

## Seeded mixed operations

`production_http_postgres_seeded_mixed_lifetimes_converge` runs eight seeds by
default, each with 64 commands across three independently stored devices. It
mixes book trash/restore/purge/readd, planned restore, placement moves/copies and
removals, folder trash/restore, annotation edits/deletion, and reading progress.
Commands use each device's local state, so offline commands can conflict with
changes already accepted by the server. The default seed set must exercise every
listed operation category.

The schedule also loses committed HTTP responses, reopens databases with pending
requests, verifies durable publication identity, and leaves devices offline across
multiple edits. A fresh fourth device joins at the end. Settling requires equal
canonical winner identities, projections, annotations and cursors, empty outboxes,
and an additional sync that changes nothing. Visible folders must remain rooted.
Purge admission and response contents come from the real service, not a simulated
max-register oracle. Physical bytes remain outside this state suite.

Run a larger sweep or reproduce a reported schedule:

```sh
TMPDIR=/dev/shm BOKHEIM_CONVERGENCE_CASES=64 bash client/app/tests/run_sync_e2e.sh
TMPDIR=/dev/shm BOKHEIM_CONVERGENCE_SEED=7 bash client/app/tests/run_sync_e2e.sh
```

Seeds fix scheduling choices; real UUIDs and authoring clocks are still generated
by production code. Failures include the seed, device, step and completed actions.
The existing database-only randomized suite can be swept independently:

```sh
TMPDIR=/dev/shm BOKHEIM_CONVERGENCE_CASES=128 cargo test --release -p library-database --lib randomized_convergence_tests
```

The mixed sweep found that trashing a live book could fail when a retained
placement pointed into a suppressed folder. Cleanup now uses the remembered
materialized path where available and skips hidden placements with no physical
destination. A focused database regression checks both cases and verifies that
placement intent remains unchanged.

## Targeted restore and filesystem edge cases

Additional database tests cover complementary offline removals that leave a
trashed book without placements, a stale restore plan across purge/readd, and
purging a folder while the book retains a copy elsewhere. The stale plan is
rejected atomically when the current book is live or its placements differ;
it may still apply when the same content hash is trashed again with matching
placements, consistent with the shared hash identity.

Real-file tests cover a filename reused by another book (both registered and
not yet scanned), and failure after trash/restore publishes bytes but before
SQLite acknowledges the job. The latter injects an abort in the acknowledgement
transaction, closes/reopens the database and store, and retries twice. This tests
that durable crash window, not power-loss guarantees of the underlying filesystem.

When all placements are gone, ordinary Restore now plans an explicit root
placement. Planning is read-only; commit revalidates membership and publishes the
new placement with the present book lifecycle. Removed folder memberships remain
removed. Database and real-server tests cover concurrent fallback restores and a
competing move/removal, duplicate deliveries, restarts and fresh observers.
Filesystem tests check byte recovery and an occupied root filename.

A confirmed identity mismatch at an old cleanup path now retires that book's
stale job and remembered path without changing replacement bytes or another
book's projection. Hashing, path validation and I/O failures remain errors with
pending work. Tests cover registered and unregistered replacements, restart,
blocked Trash storage and successful retry after repairing storage.
