# Asset transfer

Owns transfer jobs, queueing, history, execution policy, and book/thumbnail transfers.

- `books`: verified uploads and downloads, account/revision validation,
  checksum verification, conditional placement, and browser Blob uploads.
- `thumbnails`: upload/download batches, response validation, local variant recovery,
  and invalid-thumbnail regeneration requests. Hosts supply resizing and notifications.
- `planning`: bounded candidate pages, manifest negotiation, thumbnail availability,
  and requested-download job selection.
- `runner`: claims and negotiates batches, isolates file failures, retries work,
  pauses on authentication failure, and waits for scheduled attempts. `TransferHost`
  supplies execution, durable settlement, credential refresh, and notifications.
- `policy`: size-dependent upload deadlines and progress throttling.
- `deadline`: request timeout execution with cancellation cleanup.
- `TransferError`: shared authentication, retryable, and rejected outcomes.

Backend supplies a configured database, an asset store, HTTP client, library ID,
a credential callback, and notification callbacks. Account refresh, application
activity, and cross-tab request coordination stay in backend.

Execution checks the account after network work and before committing results.
Downloads recheck placement after waiting for a book lease. Uploads only
complete the captured local revision. SQL belongs to the library persistence
features; this crate contains no inline SQL.

Run the standalone service suite with:

```sh
cargo test --release -p asset-transfer --lib
```

Backend integration tests additionally exercise account changes, concurrent
book changes, checksums, placement races, and filesystem failures. Standalone
runner tests cover authentication resumption, batch execution, neighboring file
failures, and failed rejection settlement.

## History persistence

Persistent queues use one dedicated history writer. Completion enqueues a bounded
snapshot while holding the queue lock; serialization, file writes, `sync_all`, and
atomic replacement run outside that lock. Only the newest pending full snapshot is
retained, so a slow disk cannot create an unbounded backlog or reorder snapshots.

History is visible in memory immediately. `TransferQueue::flush_history()` waits
for persistence and reports errors without holding the queue lock; it can retry a
failed save. Flush before reopening the same path. The application flushes during
shutdown, and final queue teardown drains and joins the writer. Abrupt process
termination can lose recent history entries; durable transfer work remains owned
by library persistence. Browser queues continue using in-memory history.

Run the release filesystem latency comparison explicitly:

```sh
cargo test --release -p asset-transfer --lib benchmark_history_completion_latency -- --ignored --nocapture
```

## Candidate snapshots

Planning reads at most 32 candidates in one library-owned SQL statement, including
placement paths, upload eligibility, requests, rejections, and pending file work.
A short read transaction captures the account and fixed work watermark. Native
filesystem inspection runs on a blocking worker after releasing that transaction
and the storage command. AssetStore still resolves physical source paths itself.

Settlement batches exact work IDs under a write transaction, rechecks the account
and pending file work, and preserves newer enqueues. Requests remain queued until
execution completes.

Compare candidate metadata reads for 10,000 books in release mode:

```sh
cargo test --release -p client-library benchmark_asset_candidate_pages -- --ignored --nocapture
```

Set `ASSET_PLANNER_EXPLAIN=1` to also print the candidate query plan.
This measures database candidate reads only, excluding filesystem inspection,
cleanup, and transfers.
