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

The client tests live in
`client/library-backend/src/sync/library_sync/tests/convergence.rs` and use the
production `LibrarySync`, sync engine, HTTP transport, SQLite database and
PostgreSQL service. Client restarts close and reopen the same database; delayed
responses and lost acknowledgements are scheduled explicitly without replacing
server responses with fixtures. Every scenario creates an isolated library.
Cloud asset storage is disabled for these state tests; upload admission and
filesystem byte handling are covered separately.

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
