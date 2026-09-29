# Platform boundary audit — 2026-09-15

## Ownership after extraction

| Area | Mechanisms | Application responsibility |
| --- | --- | --- |
| Browser connections | `web::transport::{session,tab,endpoint,port,scope}` | Backend requests, worker roles, election and subscription recovery |
| Browser network | `web::transport::{socket,network_client,network_server,upload}` | Authentication, heartbeat/retry policy, verified uploads and HTTP status classification |
| Import | `web::transport::import_io` and `web/import_io.js` | Backend journal ordering, checkpoints, source validation and recovery |
| Media and book ranges | `web::transport::{resources,range}` | Playback leases, renewal requests, reader seeking and range-cache policy |
| Native files | `native::{filesystem,file_fingerprint,discovery}`; Android book fallback | Library scanning, managed directory markers, collision naming, trash/restore and reconciliation |
| Runtime and SQLite | Target-selected runtime and platform SQLite services | Backend transactions, database notifications and work scheduling |

## Cleanup decisions

- Removed direct `wasm-bindgen`, `wasm-bindgen-futures`, `js-sys` and `web-sys`
  dependencies from `asset-transfer`; its upload adapter now calls the platform.
- Reduced backend `web-sys` features to the types used by its remaining adapters,
  worker entry points and integration probes.
- Kept the public backend host re-exports: production worker entry points and
  integration probes still call them. Removing them would be an API migration,
  not dead-code cleanup.
- Kept the desktop directory crate available on all backend targets.
  `AppDataLocation::SharedDesktop` is a shared API and calls those portable
  directory helpers even when compiled for WASM. Restricting the dependency to
  native targets breaks the production web build.
- Kept explicit WASM randomness feature dependencies: these configure transitive
  `getrandom` versions and need not have direct Rust call sites.

## Remaining platform-specific code

The backend is not completely free of browser types. Its `web_runtime` modules
still adapt JS envelopes to application commands and implement scheduling and
host election. Worker examples now delegate their global callback installation to
`web::transport::scope`, retaining only command dispatch and coordinator policy.

Native notification connections and TLS live in `native::socket` behind the
`websocket` feature. Authentication, connection/heartbeat deadlines and reconnect
policy remain in backend. Tungstenite remains a backend dev-dependency for its
existing protocol fixtures, but production backend code uses the platform API.

The shared-WASM-memory wait/notify mechanism lives in `web::signal`. Book
access retains request/reply queues, reader deadlines and cancellation policy,
and supplies the wait duration. Its unused direct browser-binding dependencies
have been removed. `library-files` still exposes a browser Blob capability for
verified uploads; exposing that capability is not itself an ownership leak.

Native filesystem calls inside library layout/reconciliation algorithms remain
with those algorithms. The useful reusable durability and no-clobber mechanisms
have been extracted; wrapping every `std::fs` call would add indirection without
separating policy.

## Enforcement and targeted validation

- `scripts/checks/backend-boundaries.py` checks transitive dependency direction,
  cross-crate source includes, and browser mechanism ownership in migrated files.
- `.github/workflows/client-platforms.yml` runs architecture checks, native
  platform release tests, a standalone web platform release check, and an Android
  platform cross-check on relevant pull requests and main-branch pushes.
- Validation is scoped to platform crates and architecture tests. Application/UI
  builds, browser integration runs and packaging checks are excluded.

The workflow retains the published sibling repository pins needed to resolve the
workspace's path dependencies; it does not build the sibling UI crates.

## Worker-adapter duplication review

CPU, sync, transfer and import already share port ownership, reply envelopes and
pending-request utilities. Their remaining queues are not interchangeable: CPU
serializes jobs, sync permits one storage request per running keyed job, transfer
tracks shared subscribers and an owner, and import correlates typed database
responses while sequencing durable jobs. Those semantics remain with their
respective policies.

Repeated browser-event unpacking now lives in `Port::listen`; CPU endpoint event
conversion and error-event normalization live in `OwnedEndpoint::listen`.
The raw endpoint constructor is private. Raw port access and the lower-level
`Port::new` remain public because existing host/transport adapters use them.
Domain cancellation messages and protocol failure markers remain distinct;
normalizing them into one generic request runner would lose useful semantics.
Boundary checks reject event handling returning to the CPU/sync/transfer adapters.
The existing opt-in port lifecycle contract also checks payload delivery through
`listen` and destruction from within its own callback.

## Final boundary and public API review

- Enrichment CPU-capacity detection now lives in the target-selected platform
  runtime executor. Browser lookup supports both window and worker globals and
  falls back to one CPU for missing or invalid capacity. Backend retains the
  enrichment and download concurrency limits.
- Native SQLite diagnostics are private implementation details behind `sqlite`;
  callers continue to use its initialization and command-reporting API.
- Removed backend's unused `tokio-util` and `futures-executor` dependencies.
  The runtime retains its own executor dev-dependency for local unit tests.
- Android's shared-storage no-clobber copy fallback remains separate from native
  atomic persistence: it handles provider restrictions and cleans up incomplete
  copies. Desktop directory discovery and Kobo mount discovery serve different
  locations and have no duplicate implementation worth merging.
- Remaining backend OS conditionals select Android book capabilities and
  Kobo device APIs. Filesystem operations express library layout/reconciliation;
  enrichment timing controls and retry durations remain application policy.
  Browser adapters still use JS protocol types; this is not a claim that backend
  contains no platform-specific glue.
- Architecture checks also reject CPU-capacity probing returning to enrichment.

## Typed worker protocols and request ownership

`web::transport::protocol` decodes CPU requests/events, sync replies and
cancellation, transfer completion, bridge capability commands, and import
initialization/submission. Existing wire field names and byte-buffer transfers
are retained. Import database commands/replies retain their existing typed
MessagePack protocol.

Missing or invalid IDs, non-byte payloads, ambiguous success/error replies and
unexpected bridge capabilities now produce protocol errors. CPU failures drain
its queue; malformed sync messages abort the pass; transfer errors use existing
connection-failure cleanup; malformed import database messages fail the worker.
Valid late sync/import responses still have no effect once their request is gone.
Request counters fail on exhaustion instead of wrapping to another request ID.

Storage callbacks and one-shot transfer executors are explicit bridge
capabilities; CPU book requests require a source port when a source is
provided. Backend retains payload interpretation, scheduling, and book
policy. No blanket platform-service abstraction was added around ordinary domain
operations.

Pending reply maps now live in `runtime::pending` and are re-exported by the web
transport. Import requests hold a registration guard so dropping their future
removes the pending reply. Disconnect draining releases map borrows before
settling replies, allowing safe reentrant submissions. Sync retains its single
active storage command and CPU retains serial scheduling.

Native unit tests exercise wire invariants and request cleanup alongside the
existing coordinator failure/cancellation tests. Browser adapters and worker
entry points are release type-checked; browser integration is not run.

## End-to-end cancellation and worker lifecycle hardening

- CPU callers send cancellation on their dedicated reply port when dropped before
  settlement. The coordinator races the reply against cancellation, dropping the
  operation and removing queued work. Work already dispatched retains the CPU
  slot until completion. This is cooperative message delivery, not preemption.
- Queued book sources own their port until successful transfer. Rejection,
  cancellation, queue failure and failed transfer close that endpoint. The caller's
  existing book-server owner releases its reader lease when its future is
  dropped. Successful transfer hands source cleanup to the receiving worker.
- CPU callback generations reject events and errors from replaced connections.
  CPU startup defaults to 5 seconds; database startup defaults to 15 seconds.
  Hosts can override both through `SharedCoordinator::with_options` and
  `WorkerOptions`; overrides survive host replacement.
  Existing failure paths settle waiting work. Expired timers cannot fail a ready
  CPU or a replacement generation. Host readiness callbacks also check generation.
- Typed control decoding now covers both sides of background cycles, host
  registration/worker creation/failure, and network pull/cancel/headers/chunks.
  Invalid IDs and durations are rejected; HTTP chunks and completion cannot precede
  headers, and duplicate headers fail the request. Fetch payload/options are
  validated before browser calls. Application import presentation stays opaque at
  the tab transport boundary.
- Release runtime unit tests cover cancellation races, stale generations, startup
  failure recovery and bounded timer inputs. The database lifecycle has isolated
  optimized unit tests. Browser port delivery and binding behavior are type-checked
  only; UI builds and browser integration remain excluded.

## Stability and input-memory admission

CPU and sync queues now enforce both their existing job counts and admitted input
byte limits. Defaults are 64 MiB CPU and 16 MiB sync, configurable with
`WorkerOptions` through `SharedCoordinator::with_options`; these are
initial limits, not benchmark-derived optima. Reservations follow request ownership
through dispatch, coalescing, cancellation and failure. Already-running CPU work
retains its reservation when its caller cancels. Oversized requests are rejected
separately from temporary queue saturation.

These limits cover queued and running input payloads, not response sizes, transient
message decoding allocations, source files, caches or total browser memory. Transfer
coordination stores ports rather than book contents and keeps its existing
job/subscriber limits. Import checkpoints and write retry semantics are unchanged.

CPU/database startup timers are cancelled on readiness, failure and host suspension;
the existing runtime timer destructor clears the browser timer and callback. Sync
requests now pass their decoded Rust buffer directly to the queue rather than
copying through another JS byte array.

Isolated release tests exercise repeated cancellation/startup failure/replacement,
late replies, transfer owner/subscriber cleanup, coalesced sync accounting, byte
limit overflow, rejected requests, dropped queues and startup timer cancellation.
Browser bindings are type-checked only; no UI/integration runs or performance
benchmark claims are made by this pass.

## Worker failures and host departure

The experimental failure classification, automatic restart backoff, retry counters,
jitter and stable-period reset have been removed. Worker failures settle pending
work and report the error. A normal host-tab departure still elects another
available host. CPU workers remain demand-started: a subsequent explicit request
may create a fresh worker after a failure. Failed operations are never replayed.
Startup deadlines, cancellation cleanup, byte limits and generation checks remain.

## Simplification pass

- CPU reply cancellation uses the pending sender as its only settlement state;
  the extra shared boolean and exported reply-port wrapper are gone. Its small
  private owner stays beside the sole caller.
- Host failure reporting and teardown share one implementation. Ordinary host
  departure explicitly stops the old host and elects a new one, without an
  optional-error restart API.
- Removed the forwarding-only CPU endpoint constructor. Book source
  validation is shared by both CPU request decoders.
- Byte-envelope inspection no longer copies buffers when a transfer adapter only
  forwards the reply. Consumers that need Rust bytes still copy at that boundary.

Platform crate boundaries, startup/queue defaults and cancellation semantics are
unchanged by this pass. Targeted checks cannot establish browser runtime behavior.

## Constructor, lifecycle and API consolidation

`WorkerOptions` replaces the separate startup and byte-limit configuration types.
`SharedCoordinator::with_options` is the single configurable host constructor;
`new` supplies defaults. CPU and sync construction consume the same options value,
including job counts. Existing worker test factory/runner entry points remain.

A CPU connection now owns its endpoint, generation and optional startup cancellation
handle together. Absence of a connection means stopped; consuming its startup
handle marks readiness and cancels the timer. The separate lifecycle mirror was
removed. Queue readiness still controls scheduling inside the queue policy.
Native lifecycle tests exercise the same generic connection owner used by WASM.

Internal backend endpoint/pending aliases were removed in favor of direct owner
imports. Test-contract forwarding functions are re-exports. Byte conversion and
request-ID helpers are private to the web transport unless externally needed.
Defaults and wire formats remain unchanged; internal constructor/configuration
call sites and this audit were migrated to the consolidated API.
