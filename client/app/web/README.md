# Browser worker runtime

The JavaScript loaders bootstrap WASM, buffer events until Rust installs its
handlers, and attach the coordinator MessagePort to each dedicated worker. Pre-WASM failures must also be reported here.
The database worker's equivalent loader lives in `apps/web-backend-worker` at the
workspace root. Generated wasm-bindgen modules are build output.

Production starts `startSharedCoordinator`. The WASM module also retains
`startNetworkWorker` for standalone transport adapters; production HTTP runs in
the shared coordinator. Generated browser-stream bindings remain in the module. Test constructors and adapters live in
`app/examples/support/coordinator_test_api.rs` and require the
`web-runtime-tests` feature, which the test runner enables. Service inspection
methods and public service reexports are also limited to that feature.
Background phases and replies are typed Rust values executed beside storage
on both native and browser. The coordinator owns browser timers and sends one
cycle request with a refresh flag; the database worker returns a success flag.
No background phase data or JSON crosses that port. Queue policies own their
request payloads and return them on completion or failure. CPU dispatch, sync storage and transfer execution use
owned byte vectors and Rust futures internally. Sync passes receive their
storage operation directly, including test runners. Active passes are tracked
by library key; only individual storage commands need request IDs. Network
routing uses a Rust callback installed by
the coordinator. The worker runtime installs no `bokheim*` JavaScript hooks.
The global fetch bridge remains because reqwest calls the browser fetch API.

Browser transport mechanisms live in `client/platforms/web/src/transport`:

- `port`, `endpoint`, `pending`: owned callbacks, worker creation, correlation,
  disconnect draining, and endpoint cleanup.
- `tab`, `lifetime`: page-owned workers, Web Locks, and page lifecycle handling.
- `relay`, `cpu_client`, `bridge`: byte/port forwarding and cancellable worker requests.
- `network_client`, `network_server`: Fetch routing, streamed responses,
  backpressure, cancellation, and transport shutdown.

The platform also owns `host` (tab election, generation-scoped worker ownership),
`cpu` (bounded opaque CPU requests), and `transfer` (shared transfer ports).
Application worker entry points, coordinator wiring, worker roles, and bundle URLs
live in `apps/web-backend-worker`. Its `connection.rs` is shared by the web UI and
worker probes so both use the shipping deployment configuration.

Backend `web` contains browser-facing adapters for application commands,
import capabilities, book readers, and sync services. Application policy stays
in platform-independent backend modules and shared client crates. Platform transport
accepts worker specifications, opaque byte payloads, and callbacks without importing
backend types.

The pre-WASM port adapter lives in `client/platforms/web/worker_transport.js`.
The application `apps/web-backend-worker/worker_endpoint.js` wrapper supplies the database ownership
lock name. Packaging copies both files beside the worker loaders.

Database startup, request handling and browser error events live in
`apps/web-backend-worker/src`. Its Rust entry point installs the fetch bridge
before opening storage. Coordinator and network-worker WASM downloads use native
fetch before the coordinator installs its own bridge.

From the client directory, build and run the WASM adapter tests:

```sh
REAL_DATABASE=1 node app/scripts/test-coordinator-policies.mjs
```

Then run the browser checks (requires Playwright and an installed browser;
set `BROWSER=chromium` and `BROWSER_EXECUTABLE` for Chromium):

```sh
node app/scripts/test-coordinator-browser.mjs
REAL_DATABASE=1 node app/scripts/test-coordinator-browser.mjs
```

The first uses a disposable storage fixture to exercise a real CPU worker and coordinator-owned HTTP.
The second opens production storage in a fresh browser origin/profile and checks
startup, binary requests, metadata timers, database recovery after an unhandled
rejection, and explicit failure delivery after a fatal coordinator error.
Neither uses the user's application database. Set `PLAYWRIGHT_MODULE` and
`PLAYWRIGHT_BROWSERS_PATH` when Playwright is installed outside the default paths;
`COORDINATOR_TEST_TARGET` selects a reusable Cargo target directory.
`COORDINATOR_TEST_OUTPUT` selects an isolated bindings directory for both the
builder and tests.

The storage and sync browser contracts accept `WORKER_TEST_PROFILE` to reuse an
optimized Cargo profile (for example `web-release`). The sync contract also
accepts `WORKER_TEST_DIST` for an isolated directory containing production worker
bundles, so verification need not replace the installed web app. The full worker
contract (`test-web-workers.mjs`) accepts the same override. All worker scripts
share Cargo invocation, immediate binding generation and browser launch through
`app/scripts/worker-test-support.mjs`. Bindings are generated immediately
after each build to avoid confusing production and test feature artifacts.

## Ownership boundaries

### Imported files at startup

Books and thumbnails use ordinary OPFS files. The SQLite asset index stores
logical names and physical paths, never thumbnail bytes. New files are flushed
before their index entry is published. Reads prepare an access handle
asynchronously and release it when their last reader closes; thumbnail reads
return bytes after closing their reader. Native thumbnails remain filesystem
files.

Storage assumes fresh libraries. Older pooled assets and thumbnail BLOBs are
not migrated or read through compatibility paths. Removing an asset deletes its
index row; no deletion tombstone is needed.

Private files abandoned by interrupted writes are removed at bootstrap by name,
without opening their contents. Availability follows the committed index.
Reader preparation runs outside SQLite transactions, including thumbnail
generation; actual reader `Read`/`Seek` operations stay synchronous.

An imported generation is pinned before its handle is opened. Concurrent opens
share one access handle; its last reader closes it. An opening task owns the pin
until completion even if the caller cancels, so a late handle cannot leak or
race deletion. Staging writers also pin their source until book finishes.
Browser storage accounting sums immutable lengths recorded in the asset index at book; it does not open or stat OPFS files. Native accounting resolves current placements and reads file metadata after relevant changes.

Folder-import format selection, destination matching, structured failures, progress
deltas, sequential batch execution, progress cadence, and finalization are shared
by native readers and staged imports through `import_workflow`. `staged_import` owns the copy barrier, checkpoint recovery, and
metadata ordering over typed storage and library interfaces; it compiles on native
and WASM targets. `import_journal` owns durable outcomes. `browser_import` adapts
browser files, OPFS, progress callbacks, and typed worker requests to these interfaces.
`web::import_worker` owns the serial browser job queue and request transport.
JavaScript supplies file capabilities and invokes the workflow entry point.

Settings snapshots only read cached state. One background task refreshes after
relevant events; successful work returns to waiting for events. Only failures
schedule a retry deadline. Cover generation and transfer progress do not request
settings refreshes. Book retirement emits a storage-change event so purge
completion updates totals even after its metadata command has returned.

The asset index records `byte_length` for every published generation. Existing
browser storage requires a reset for this schema change; there is no migration.

The import, network and coordinator transports share correlation IDs and pending
request draining in `client-platform-runtime::pending`. Disconnect callbacks run after the
registry is drained, allowing cancellation and new registrations without a
borrow conflict. Each transport retains its own scheduling and streaming rules.

Each tab connects through `WebConnection`, which holds its lifetime Web Lock
and creates dedicated workers on coordinator request. Chromium does not need a
Worker constructor inside SharedWorkerGlobalScope. The coordinator communicates
directly with those workers through transferred MessagePorts.

One dedicated database worker holds `bokheim-database-owner-v1` exclusively.
Closing its hosting tab releases the lock and triggers election of a replacement
host. Client ports survive, subscriptions reconnect and refresh their current state,
and interrupted requests fail explicitly. Mutations are never replayed automatically because an interrupted
write may already have committed. A document restored from the back/forward
cache reloads to establish a fresh connection after its workers were released.

The database worker owns SQLite, published asset handles, and the complete background workflow. The
coordinator retains shared timers, sync/transfer deduplication and tab lifecycle.
CPU codecs and storage remain in dedicated workers. Browser HTTP streams run
asynchronously in the coordinator, isolated from CPU work and storage operations.

Within a bridge request, the returned future owns the cancellation sender.
Dropping it stops spawned work; a coordinator failure completes the reply and
closes the receiver. The pending registry holds weak cancellation callbacks so
whole-worker failure can notify live requests without retaining them. Failure
takes the registry and consumes its callbacks outside the registry borrow;
callbacks have one owner and need no reference counting or temporary vector.
Sync request variants carry their own initial bytes and storage operation, so
request kind and payload cannot disagree. The input buffer is moved out before
the runner is retained. Common byte/error envelopes are handled in one place.

`SyncEngine::synchronize` awaits each storage operation, and `CoordinatorStore`
awaits each page/chunk command. A pass therefore owns one pending storage reply,
with an ID to reject late replies. Overlapping calls are rejected without
replacing the active reply; different libraries can still execute concurrently.

Each database-side CPU request owns a dedicated reply port. The coordinator
replies on that port and closes it; there is no database-side CPU ID counter,
pending map or global response listener. CPU queue IDs remain internal to the
coordinator to reject unexpected worker responses and handle worker restarts.
The out-of-order caller, unexpected-worker-reply and restart tests cover these
separate responsibilities.

An absent port is the authoritative closed state for background endpoints and
network requests. Closing the background command channel wakes pending reads;
port checks also reject buffered commands after shutdown. HTTP registries retain
requests across header delivery and streaming, and worker failure takes the
client registry before rejecting requests. The server's in-flight read flag and
the client's pull completion remain: they enforce one outstanding body read and
backpressure, rather than duplicating the closed state.

After building the release web bundle, verify real ownership and handover:

```sh
node app/scripts/test-worker-endpoints.mjs
THUMBNAIL_FIXTURE=app/tests/fixtures/storage-contract.epub node app/scripts/test-chromium-handover.mjs
node app/scripts/test-web-workers.mjs
```

Run these with both Chromium and Firefox. Handover uses production SQLite and
AppClient in a disposable origin, checks one owner, preserves subscriptions,
rejects interrupted writes without replay, and generates covers across host
replacement. `WORKER_PROBE_DIR` optionally reuses freshly built probe bindings.

Upload planning does not read book contents. Each upload worker verifies once,
negotiates original-file uploads individually, then retains the verified reader
through upload. Edited revisions bypass negotiation and upload their current bytes.

Browser book uploads verify checksums in bounded chunks, then pass the pinned OPFS
`File` through the coordinator to browser fetch. Neither the database worker nor
the network bridge materializes the whole upload in memory. The verified file
stays pinned until the request completes; dropping an upload aborts fetch.

## Folder import memory

Folder selection retains file handles and a directory manifest. Native clients
create the directory tree and import one reader at a time. Browser clients transfer
`File` capabilities to the import worker, which copies and hashes bounded 1 MiB
chunks into OPFS before requesting metadata insertion. Single-file browser imports
use that same workflow; they do not send file chunks through the database worker.
See Folder imports below for checkpointing and recovery.

CPU metadata and cover jobs use a seekable range source. One 256 KiB shared
buffer and a direct MessagePort connect the CPU parser to the storage owner.
The SharedWorker brokers the port; the two dedicated workers exchange the
shared buffer directly within their host tab’s agent cluster;
only the CPU worker waits synchronously, and reads have a failure timeout. No
book-sized input is assembled in the CPU worker. PDF inspection loads
Info/XMP and the objects needed for the first twenty pages of text, skipping
image/attachment contents and embedded font programs. Metadata object, graph,
and decompression limits bound pathological documents independently of file size.
Selecting browser files retains handles without reading their contents; dropping
an unopened selection releases its handles. Native single-file pickers retain
their existing API.

Cover decoding consumes existing RGB JPEG buffers instead of copying them.
Other raster formats are reduced to cover dimensions before RGB conversion,
avoiding a second full-size pixel allocation for RGBA and 16-bit sources. Those
formats still decode the original image; this is not streaming image decoding.

Rendering/playing an opened book is a separate path and is not changed here.

`node app/scripts/test-folder-import-memory.mjs` verifies commit-before-open
ordering against real SQLite with twelve 4 MiB books, checks folder
structure, and reports WASM memory in the tab, coordinator and dedicated workers.
Run with `BROWSER=chromium` and `BROWSER=firefox`; the usual `WORKER_PROBE_DIR`,
`WORKER_TEST_DIST`, Playwright and executable overrides apply.

`node app/scripts/test-book-range-memory.mjs` compares 4 MiB and
128 MiB EPUB/PDF imports, checks failed-session cleanup, samples each worker's
WASM memory, verifies EPUB covers, and counts metadata/cover range reads.
Its HTTP fixture uses bounded range requests to match the folder picker’s File
slices. Set `MEMORY_TEST_TMP` to a directory with room for the generated
fixtures. The same browser overrides apply. These measurements cover WASM linear memory, not total browser RSS.

`node app/scripts/test-file-picker-memory.mjs` checks that selecting a
128 MiB file reads no contents, range reads stay bounded, and released tokens
cannot be read. It uses the GPUI JavaScript source (`GPUI_ROOT` can override the
sibling checkout).

`node app/scripts/test-cover-decode-memory.mjs` runs the production CPU WASM
on a 24-megapixel RGBA PNG. `BASELINE_DIST` optionally compares an older bundle;
the usual browser and `WORKER_TEST_DIST` overrides apply. The measurement is
WASM linear memory, not total browser RSS.

## Duplicate imports

After hashing, imports reuse a successful local inspection for the same content
hash and format within the library. A device-local, versioned marker is recorded
in the same transaction as the imported metadata. Remote metadata alone does not
qualify; missing markers, changed formats, and tombstoned books require inspection.
The cached path preserves existing metadata edits and audiobook data while still
committing the requested folder placement and any authority association.

Ready cover files are reused. Missing variants request repair without resetting
pending retry deadlines or the known-no-cover state. Copying and hashing still
read the selected file to establish its identity.

`node app/scripts/test-duplicate-import.mjs` uses the production worker and
`worker_probe` bindings (`WORKER_PROBE_DIR`) to verify that a second identical
import performs zero metadata/cover range reads. The usual browser and
`WORKER_TEST_DIST` overrides apply.

## Book range cache

The browser CPU worker retains up to four 64 KiB fragments per book job
(256 KiB total). Both native remote readers and browser book readers use
Rust's `range_cache` for lookup and eviction. Browser reads copy directly into
the parser's output buffer; JavaScript owns only the bounded transport buffer.
Reader disposal releases the cache. Reads reject closed or failed sources,
including cache hits, and preserve short-read and end-of-file behavior.

`COORDINATOR_TEST_OUTPUT=<release bindings directory> node
app/scripts/test-book-source.mjs` exercises the actual Rust reader,
including eviction, short reads and source closure. The bindings must contain
`cpu_worker` built with `web-runtime-tests`.

The old `benchmark-book-cache.mjs` targets the removed JavaScript cache
and needs updating before it can compare the Rust implementation. Its historical
measurements do not describe the current implementation.

## Folder imports

The selecting tab transfers browser `File` capabilities to the shared coordinator.
`web::import_queue` retains the selection and broadcasts progress to connected tabs.
The elected tab hosts a separate import worker, which copies bounded 1 MiB chunks
straight to `__import_objects/<job UUID>/<file index>` in OPFS and computes BLAKE3
in its own WASM instance. File bytes never pass through the database worker.

The manifest records source paths, empty directories, sizes and modification times.
A flushed journal checkpoints each completed file and each metadata outcome. No
library-directory or book insertion starts until **every selected book has been
copied**. The database then adopts immutable file references without another copy,
inspects books through the CPU worker, and commits metadata. Folder IDs are
deterministic per job/path; book placement upserts make an unacknowledged commit
safe to retry. Failed copies pause before database insertion; malformed books are
reported individually in the second phase. Unreferenced copies are cleaned up
only after their outcomes have been checkpointed.

Host replacement retains the selection in the coordinator, restarts the import
worker, skips completed copies and resumes metadata work. Fully staged unfinished
jobs can also be discovered after all tabs were closed. Original `File`
capabilities are not durable across browser shutdown: an incomplete copy then
requires selecting the source folder again. Progress distinguishes copying
(bytes and files) from adding to the library (processed files and failures).

```sh
node --test app/web/import_worker.test.mjs app/web/import_queue.test.mjs
node app/scripts/test-folder-import-handover.mjs
node app/scripts/test-folder-import-memory.mjs
```

The browser checks use disposable storage and accept `WORKER_TEST_DIST` and
`WORKER_PROBE_DIR`. The handover test closes the host in each phase, checks the
whole-folder barrier, and reopens an imported book after database restart.

### Audiobook playback sources

WASM audiobook resolution returns a source descriptor instead of book
bytes. A downloaded M4B uses a browser Blob URL backed by its immutable OPFS
file. The backend subscription pins that generation and revokes the URL when
playback closes or the tab disconnects. Cancelling source preparation also
releases its lease.

Remote M4Bs use `audiobook_service_worker.js`, packaged at the app root. It
handles only `.bokheim-audio/` media URLs, serves byte-range responses with
bounded chunks, and asks the owning tab to read each range through the backend.
Account credentials and refresh remain in the authenticated backend API; URLs
contain only a random playback identifier. Seeking does not require a complete
download, and streamed fragments are not marked as downloaded books.
Range readers discover and pin the uploaded revision ETag separately from the
permanent book identity, so metadata edits cannot mix bytes from different revisions.

Duration, chapters, narrator and cover use the local metadata/thumbnail cache.
Missing metadata is parsed on the CPU worker through bounded range reads and
saved for subsequent opens. A known missing cover is cached too.

Browser verification (provide a playable M4B longer than 80 seconds):

```sh
node client/app/scripts/test-audiobook-streaming.mjs /path/to/book.m4b
BROWSER=chromium node client/app/scripts/test-audiobook-local.mjs /path/to/book.m4b
```

The local test builds an optimized WASM probe and uses the existing optimized
web bundle. It blocks book reads on reopening to verify metadata reuse,
and checks playback, seeking to 70% of the book, reopening at the backend-persisted
position, purge pinning and release. The streaming test checks real playback
with bounded requests, far seeking, reopening and range/cleanup behavior. Both
support `PLAYWRIGHT_MODULE`; the local test also supports `BROWSER_EXECUTABLE`,
and the streaming test supports `CHROMIUM_PATH`.
