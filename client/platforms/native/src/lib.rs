//! Native filesystem and device integration used by client services.
#![cfg(not(target_arch = "wasm32"))]
pub mod cpu_host;
mod database_diagnostics;
pub mod discovery;
pub mod file_fingerprint;
pub mod sqlite;

pub mod filesystem;

#[cfg(feature = "websocket")]
pub mod socket;

/// Native databases do not require browser VFS capacity reservations.
pub async fn reserve_database_capacity(_additional: usize) -> Result<(), String> {
    Ok(())
}
