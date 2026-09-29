//! Adapt browser history traversal to the same action used by Android Back.

use std::{cell::Cell, rc::Rc};

use browser_ui::DismissSettings;
use gpui::{App, Window};
use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};

#[wasm_bindgen(module = "/src/browser_back.mjs")]
extern "C" {
    #[wasm_bindgen(catch, js_name = installBrowserBack)]
    fn install_browser_back(callback: &JsValue) -> Result<(), JsValue>;
}

pub(super) fn install(window: &Window, cx: &mut App) {
    // Global bubble listeners run only after every focused view has declined
    // the action. Observe that fallback without changing normal propagation.
    let unhandled = Rc::new(Cell::new(false));
    let fallback = unhandled.clone();
    cx.on_action(move |_: &DismissSettings, cx| {
        fallback.set(true);
        cx.propagate();
    });

    let mut window = window.to_async(cx);
    let callback = Closure::new(move || -> bool {
        window
            .update(|window, cx| {
                let Some(focus) = window.focused(cx) else {
                    return false;
                };
                unhandled.set(false);
                // FocusHandle dispatch is synchronous, so we can return whether
                // GPUI consumed Back before deciding to rearm browser history.
                focus.dispatch_action(&DismissSettings, window, cx);
                !unhandled.get()
            })
            .unwrap_or(false)
    });
    // JavaScript owns the callback for the lifetime of its event listeners.
    if let Err(error) = install_browser_back(&callback.into_js_value()) {
        log::warn!("failed to install browser Back handler: {error:?}");
    }
}
