pub mod app;

pub use app::AppClient;

#[cfg(not(target_arch = "wasm32"))]
mod launch;
#[cfg(not(target_arch = "wasm32"))]
pub use launch::BackendLaunch;
