# Web backend application

This package owns the browser application's worker topology and entry points:

- `src/lib.rs`: database worker startup and application request dispatch.
- `examples/sync_coordinator.rs`: shared coordinator entry point.
- `src/coordinator.rs` and `src/hosts.rs`: wiring backend services to an elected browser host.
- `examples/cpu_worker.rs`: CPU and import worker entry points.
- Worker loader `.js` files: application bootstrap and database ownership configuration.
- `connection.rs`: deployment identity and worker script URLs, shared with the web UI and worker probes.

`client-platform-web` owns worker creation, tab election, lifetime locks, ports,
CPU execution queues, transfer coordination, storage mechanisms, and network I/O.
It accepts opaque payloads and callbacks and has no dependency on the backend.

`app` owns application commands and workflows. Native and staged
imports share `import_workflow`; `staged_import` uses typed storage and library
interfaces for copy ordering, checkpoints, and recovery. `web` and
`browser_import` translate browser capabilities and messages into backend operations.
Sync uses the same `sync-engine` on native and browser targets. Request correlation
and disconnect draining use `client-platform-runtime::pending`.

Build the complete optimized application from the workspace root:

```sh
BOKHEIM_WEB_OUTPUT_DIR=/tmp/bokheim-web ./apps/web-gpui/scripts/build.sh
```

Worker contracts and browser checks remain under `client/app/scripts`.
`worker-test-support.mjs` selects this package for the CPU and coordinator examples;
backend probes remain in `app`.
