//! Browser connection and failure events after the WASM runtime has loaded.
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{ErrorEvent, MessageEvent, PromiseRejectionEvent};

fn detail(error: JsValue) -> String {
    let field = |key: &str| js_sys::Reflect::get(&error, &key.into()).ok().and_then(|value| value.as_string());
    match (field("message"), field("stack")) {
        // Firefox stacks contain locations without the error message.
        (Some(message), Some(stack)) if !stack.contains(&message) => format!("{message}\n{stack}"),
        (_, Some(stack)) => stack,
        (Some(message), None) => message,
        (None, None) => error.as_string().unwrap_or_else(|| format!("{error:?}")),
    }
}

pub fn install() -> Result<(), JsValue> {
    let scope = super::worker_scope();
    let error = Closure::<dyn FnMut(ErrorEvent)>::new(|event: ErrorEvent| {
        let error = if event.error().is_null() || event.error().is_undefined() { event.message() } else { detail(event.error()) };
        super::post_control("backend_failed", Some(&format!("Database worker failed: {error}")));
    })
    .into_js_value();
    scope.add_event_listener_with_callback("error", error.unchecked_ref())?;
    let rejection = Closure::<dyn FnMut(PromiseRejectionEvent)>::new(|event: PromiseRejectionEvent| {
        super::post_control("backend_failed", Some(&format!("Database worker failed: {}", detail(event.reason()))));
    })
    .into_js_value();
    scope.add_event_listener_with_callback("unhandledrejection", rejection.unchecked_ref())?;
    let message = Closure::<dyn FnMut(MessageEvent)>::new(|event: MessageEvent| {
        let data = event.data();
        let transport = js_sys::Reflect::get(&data, &"transport".into()).ok().and_then(|value| value.as_string());
        if transport.as_deref() == Some("connect") {
            if let Ok(port) = js_sys::Reflect::get(&data, &"port".into()).and_then(|port| port.dyn_into()) {
                super::connect(port);
            }
        }
    })
    .into_js_value();
    scope.set_onmessage(Some(message.unchecked_ref()));
    Ok(())
}
