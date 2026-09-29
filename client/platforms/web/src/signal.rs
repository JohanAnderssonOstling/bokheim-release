//! Shared-WASM-memory wakeups; reader scheduling and cancellation stay with callers.
use std::sync::atomic::{AtomicI32, Ordering};
use wasm_bindgen::prelude::*;
#[wasm_bindgen(inline_js = r#"
export function fileSignal(memory, address) {
    const signal = new Int32Array(memory.buffer, address, 1);
    Atomics.add(signal, 0, 1);
    Atomics.notify(signal, 0);
}
export function waitFileSignal(memory, address, observed, timeout) {
    if (typeof WorkerGlobalScope === 'undefined' || !(globalThis instanceof WorkerGlobalScope))
        throw new Error('Blocking file reads require a parser worker');
    return Atomics.wait(new Int32Array(memory.buffer, address, 1), 0, observed, timeout);
}
export async function waitFileSignalAsync(memory, address, observed, timeout) {
    await Atomics.waitAsync(new Int32Array(memory.buffer, address, 1), 0, observed, timeout).value;
}
export function checkFileBridge(memory) {
    if (!(memory.buffer instanceof SharedArrayBuffer) || typeof Atomics.waitAsync !== 'function')
        throw new Error('Remote files require shared memory and Atomics.waitAsync');
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = fileSignal)]
    fn notify(memory: JsValue, address: u32);
    #[wasm_bindgen(js_name = waitFileSignal, catch)]
    fn wait(memory: JsValue, address: u32, observed: i32, timeout: f64) -> Result<String, JsValue>;
    #[wasm_bindgen(js_name = waitFileSignalAsync)]
    fn wait_async(memory: JsValue, address: u32, observed: i32, timeout: f64) -> js_sys::Promise;
    #[wasm_bindgen(js_name = checkFileBridge, catch)]
    fn check(memory: JsValue) -> Result<(), JsValue>;
}

#[derive(Default)]
pub struct Signal(AtomicI32);
impl Signal {
    pub fn notify(&self) {
        notify(wasm_bindgen::memory(), self.0.as_ptr() as u32);
    }
    pub fn observed(&self) -> i32 {
        self.0.load(Ordering::SeqCst)
    }
    pub async fn wait_async(&self, observed: i32, timeout: std::time::Duration) {
        let _ = wasm_bindgen_futures::JsFuture::from(wait_async(wasm_bindgen::memory(), self.0.as_ptr() as u32, observed, timeout.as_secs_f64() * 1000.0)).await;
    }
}
impl Signal {
    pub fn check() -> Result<(), String> {
        check(wasm_bindgen::memory()).map_err(|e| format!("remote file bridge unavailable: {e:?}"))
    }
    pub fn wait(&self, observed: i32, timeout: std::time::Duration) -> Result<String, String> {
        wait(wasm_bindgen::memory(), self.0.as_ptr() as u32, observed, timeout.as_secs_f64() * 1000.0).map_err(|e| format!("remote file wait failed: {e:?}"))
    }
}
