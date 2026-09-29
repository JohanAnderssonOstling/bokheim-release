# Client platform runtime

Owns native/browser execution, bounded storage scheduling, and generic coordinator
queues. It has no dependency on library persistence or application services.

`coordinator` owns bounded payload queues:

- `SyncJobs` coalesces requests by library and preserves every reply recipient.
- `TransferJobs` shares execution among subscribers and retains cancelled owners
  until execution finishes.
- `CpuJobs` serializes CPU work and returns owned jobs when a worker fails.

Browser adapters supply message ports and responses. Application shutdown order
remains in backend's `runtime/coordinator_lifecycle.rs` because it names the
application's workers and controls their teardown.

```sh
cargo test --release -p client-platform-runtime --lib
```
