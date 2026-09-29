//! Web Locks lifetime primitives. Host election and recovery belong to callers.
use js_sys::Function;
use wasm_bindgen::prelude::*;
#[wasm_bindgen(module = "/src/transport/lifetime.js")]
extern "C" {
    #[wasm_bindgen(catch, js_name = holdTabLock)]
    pub(super) fn hold_tab_lock(name: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = observeTabLock)]
    pub fn observe_tab_lock(name: &str, released: &Function) -> Result<Function, JsValue>;
}
