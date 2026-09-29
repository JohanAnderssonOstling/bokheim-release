//! Per-request cancellation over a dedicated reply port.
use super::{byte_reply, field, object, BytesFuture, Port};
use std::rc::Rc;
use wasm_bindgen::{JsCast, JsValue};

/// A queued source owns its endpoint until transfer succeeds.
#[derive(Default)]
pub struct Source(JsValue);
impl Source {
    pub fn new(value: JsValue) -> Self {
        Self(value)
    }
    pub fn value(&self) -> JsValue {
        self.0.clone()
    }
    pub fn transferred(&mut self) {
        self.0 = JsValue::UNDEFINED;
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        if let Ok(port) = field(&self.0, "port").dyn_into::<web_sys::MessagePort>() {
            port.close();
        }
    }
}

/// Cancellation drops the operation future, which releases queued domain work.
pub fn serve(port: web_sys::MessagePort, response: BytesFuture) {
    let (cancel, cancelled) = futures_channel::oneshot::channel::<()>();
    let cancel = Rc::new(std::cell::RefCell::new(Some(cancel)));
    let malformed = cancel.clone();
    let owner = Port::listen(
        port,
        move |_message| {
            // The dedicated caller-to-server channel only carries cancellation.
            if let Some(cancel) = cancel.borrow_mut().take() {
                let _ = cancel.send(());
            }
        },
        move || {
            if let Some(cancel) = malformed.borrow_mut().take() {
                let _ = cancel.send(());
            }
        },
    );
    wasm_bindgen_futures::spawn_local(async move {
        if let Some(result) = client_platform_runtime::pending::until_cancelled(response, cancelled).await {
            let (message, transfer) = byte_reply(&[], result);
            let _ = owner.raw.post_message_with_transferable(&message, &transfer);
        }
    });
}
