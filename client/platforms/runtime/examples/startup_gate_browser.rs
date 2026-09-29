#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn run_contract(abandon: bool) -> bool {
    client_platform_runtime::executor::startup_gate_contract(abandon).await
}
