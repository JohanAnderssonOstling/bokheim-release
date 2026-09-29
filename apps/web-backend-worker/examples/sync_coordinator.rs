#![cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(feature = "web-runtime-tests")]
#[path = "support/coordinator_test_api.rs"]
mod test_api;

#[wasm_bindgen(js_name = startNetworkWorker)]
pub fn start_network_worker() -> Result<(), JsValue> {
    app::host::web::network_server::start()
}

#[wasm_bindgen(js_name = startSharedCoordinator)]
pub fn start_shared_coordinator() -> Result<(), JsValue> {
    app::host::web::network_client::install()?;
    let coordinator = hosts::SharedCoordinator::new();
    let failed = coordinator.clone();
    client_platform_web::transport::scope::shared_connections(move |message| failed.fail(&message), move |port| coordinator.connect(port))
}

#[cfg(all(target_arch = "wasm32", feature = "web-runtime-tests"))]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn serve_network_contract(port: web_sys::MessagePort, request: JsValue) -> Result<(), JsValue> {
    app::host::web::network_server::serve(port, request, None)
}

#[path = "../src/coordinator.rs"]
mod coordinator;
#[path = "../src/hosts.rs"]
mod hosts;
use app::host::web::{BackgroundService, ImportQueue, SyncService};
use client_platform_web::transport::{ClientRelay, byte_reply, cpu::CpuService, error_message, failure, field, network_client, network_server, object, transfer::TransferService};
use coordinator::Coordinator;

/// Application worker topology configuration.
#[derive(Clone, Copy, Debug)]
struct WorkerOptions {
    cpu: client_platform_web::transport::cpu::CpuWorkerOptions,
    sync: app::host::web::SyncOptions,
}
impl Default for WorkerOptions {
    fn default() -> Self {
        Self { cpu: Default::default(), sync: Default::default() }
    }
}
