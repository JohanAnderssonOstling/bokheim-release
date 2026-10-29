# Local wasm-streams patch

Upstream: crates.io `wasm-streams` 0.4.2, MIT OR Apache-2.0.

With wasm-bindgen 0.2.129 and `panic=unwind`, exported methods and
`future_to_promise` require unwind-safe state. Upstream's stream adapters
capture arbitrary user streams, readers, and sinks behind mutable shared state.
They cannot guarantee that an interrupted operation is safe to resume.

The three Rust-to-JavaScript adapters use a common `wasm-callback::AbortOnPanic`
wrapper. Every state access, future poll, and destructor has a guard that
aborts before a panic can escape to an outer catch boundary. The wrapper can
therefore implement the unwind-safety markers without asserting those traits
for user state. Ordinary stream errors still reject promises; cancellation,
pending reads, backpressure, and successful completion retain their behavior.
The application keeps `panic=unwind`; the patch preserves abort behavior only
at these stream boundaries. The shared helper also serves the application transport callbacks.
Native subprocess tests verify that an outer
`catch_unwind` cannot resume after guarded access, poll, or drop panics.

Remove this override when the dependency graph can use an upstream release
with compatible unwind support.

Upstream 0.5.0 was also checked with wasm-bindgen 0.2.129: its promises address
unwind safety, but its exported adapter types still fail the unwind-safety
requirements (17 compilation errors). Reqwest 0.12 selects the 0.4 series.
Upstream 0.6.0 subsequently fixed the exported types, and 0.7.0 retains that
fix. They cannot directly satisfy Reqwest 0.12's `^0.4` dependency; removing
this override therefore also requires resolving that version constraint.
See https://github.com/MattiasBuelens/wasm-streams/blob/v0.7.0/CHANGELOG.md.
The js-sys executor issue is fixed by the upstream bindings update; js-sys
does not need a local patch.
