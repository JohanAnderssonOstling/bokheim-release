# Client platform crates

Platform services live here; the backend composes them with library, account,
and synchronization logic. None of these crates depends on `app`.

| Crate | Responsibility |
| --- | --- |
| `android` | Private book locks and shared-storage book fallback. |
| `web` | OPFS storage plus worker creation, owned ports, cancellation, Fetch transport, and tab lifetimes. |
| `desktop` | Conventional application, Documents, and home directory locations. |
| `kobo` | Mounted-device discovery, destination validation, and file copying; optional on-device hardware services through the `device` feature. |
| `native` | Shared native file fingerprints, discovery OS helpers, and SQLite connection behavior and diagnostics. |
| `runtime` | Target-selected async execution, cancellation, timers, and bounded storage scheduling. |

Android, desktop, and Kobo share native services. Browser execution uses local
futures and worker timers; native execution uses Tokio. A single runtime crate
keeps task ownership and queue policy consistent without introducing dependencies
between platform implementations.

Backend target dependencies select Android and web services. Its `kobo` feature
forwards to `client-platform-kobo/device`; mounted-device transfer is available
on native hosts through `app::host::kobo` without enabling hardware access.
The app owns registry and manifest validation, transfer exclusions, and publishing
the manifest after copying content. Desktop location resolution
uses the portable `dirs` API, preserving the existing `SharedDesktop` behavior.

Shared application policy has explicit homes:

- `client/app/crates/library-registry` owns registered libraries and app state.

- `client/library` owns database mutations and durable library state.
- `client/library-backend/crates/library-files` owns scanning and asset reconciliation policy.
- `client/library-backend/crates/book-access` owns book-cache policy.
- `client/app/src/storage` coordinates connections, write notifications,
  and adapters for those shared services.
- `client/app/src/library/events.rs` and `discovery.rs` connect domain events
  and scan progress to the application.
- `client/app/src/contracts` contains opt-in browser integration contracts.

`client/app/src/platform` only selects platform services. Native and browser
SQLite modules supply connection-opening behavior, single-owner requirements,
and diagnostics; transaction and notification policy stays in shared storage.
Browser worker mechanisms live in `web/src/transport`. Its session owner handles
byte transfers, browser callbacks, error decoding, and connection cleanup, including
cancelled startup; typed requests and subscription recovery remain in backend. Backend supplies worker
roles, application requests, scheduling, and host-election policy through
callbacks; its existing host API re-exports remain stable. Native file operations remain in
`library-files` alongside their domain orchestration.

The browser storage contract accepts an outbox-check callback so it can exercise
the backend database adapter without a dependency back into the backend.

Run the platform regression tests with:

```sh
cargo test --release -p client-platform-android -p client-platform-web \
  -p client-platform-desktop -p client-platform-kobo \
  -p client-platform-native -p client-platform-runtime
```

Android filesystem fallback tests also run on a native host. Browser OPFS and
timer contracts run through `client/app/scripts/test-web-storage.mjs`.

Shared-memory range transport lives in `web/src/transport/range.rs`, including
source handles and synchronous/asynchronous callback ownership. Book
seeking, range caching, authentication, and fetch policy remain in backend.

## Remaining adapter boundary

The web transport modules also own binary WebSocket connections, Blob URLs,
async JavaScript callbacks, import file handles, dedicated-worker global
callbacks, and cancellable Blob uploads. Backend chooses notification protocols,
authentication, heartbeat/reconnect policy, playback leases and renewal requests,
and import journal ordering. Asset transfer retains verification and HTTP status
classification. JavaScript values can still cross these application interfaces;
that alone does not make their application policy a platform service.

Native filesystem auditing extracted directory durability and no-clobber file
book into `native::filesystem`. Android keeps its specialized book
fallback. Scanning, reconciliation, trash/restore decisions, managed directory
markers, collision naming and content verification remain in `library-files`:
those operations depend on library identity and durable library state. Ordinary
`std::fs` calls within those algorithms do not warrant one-function wrappers.

`scripts/checks/backend-boundaries.py` checks dependencies and prevents direct
browser resource/callback ownership from returning to the extracted adapters.

See [the consolidation audit](AUDIT.md) for remaining platform-specific code,
dependency decisions, and targeted CI/release checks.

Native WebSocket connections are available through `native::socket` with the
`websocket` feature. `web::signal` owns Atomics wakeups over shared WASM memory;
callers supply wait durations. Worker entry points use `web::transport::scope`
for global callback installation. These boundaries are enforced by the architecture
checks; isolated signal tests run with `node --test client/platforms/web/tests/signal.mjs`.
