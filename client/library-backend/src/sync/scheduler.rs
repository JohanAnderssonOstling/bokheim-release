//! Compatibility re-export for library-owned worker scheduling.

// The glob is live only on wasm, where `super::scheduler::run_timed`
// resolves through it; native code names `library_runtime::scheduler` directly.
#[cfg(target_arch = "wasm32")]
pub use library_runtime::scheduler::*;
