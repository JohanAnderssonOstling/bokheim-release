use wasm_bindgen::{prelude::*, JsCast};
#[derive(Clone)]
pub struct Scope(web_sys::DedicatedWorkerGlobalScope);
impl Scope {
    pub fn current() -> Self {
        Self(js_sys::global().unchecked_into())
    }
    pub fn post_message(&self, message: &JsValue) -> Result<(), JsValue> {
        self.0.post_message(message)
    }
    /// Retained by the worker global for its lifetime; replacement releases the JS callback.
    pub fn receive(&self, mut callback: impl FnMut(JsValue) + 'static) {
        let handler = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| callback(event.data())).into_js_value();
        self.0.set_onmessage(Some(handler.unchecked_ref()));
    }
}

impl Scope {
    pub fn post_message_with_transfer(&self, message: &JsValue, transfer: &js_sys::Array) -> Result<(), JsValue> {
        self.0.post_message_with_transfer(message, transfer)
    }
}

/// Installs SharedWorker entry points for the lifetime of the worker global.
pub fn shared_connections(failed: impl Fn(String) + 'static, mut connect: impl FnMut(web_sys::MessagePort) + 'static) -> Result<(), JsValue> {
    let failed = std::rc::Rc::new(failed);
    for event_name in ["error", "unhandledrejection"] {
        let failed = failed.clone();
        let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            event.prevent_default();
            let message = js_sys::Reflect::get(&event, &"message".into())
                .ok()
                .and_then(|value| value.as_string())
                .or_else(|| js_sys::Reflect::get(&event, &"reason".into()).ok().map(|reason| js_sys::JsString::from(reason).as_string().unwrap_or_else(|| "Shared coordinator failed".into())))
                .unwrap_or_else(|| "Shared coordinator failed".into());
            failed(message);
        })
        .into_js_value();
        js_sys::global().unchecked_into::<web_sys::EventTarget>().add_event_listener_with_callback(event_name, handler.unchecked_ref())?;
    }
    let handler = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
        if let Ok(port) = event.ports().get(0).dyn_into::<web_sys::MessagePort>() {
            connect(port);
        }
    })
    .into_js_value();
    js_sys::Reflect::set(&js_sys::global(), &"onconnect".into(), &handler)?;
    Ok(())
}
