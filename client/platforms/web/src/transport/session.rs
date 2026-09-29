//! Owned browser session callbacks with a cloneable byte sender.
use std::{cell::Cell, rc::Rc};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use web_sys::{MessageEvent, MessagePort, SharedWorker};

pub enum SessionMessage {
    Bytes(Vec<u8>),
    Reconnecting,
    Failed(String),
}

#[derive(Clone)]
pub struct SessionSender(Rc<Connection>);
struct Connection {
    worker: SharedWorker,
    port: MessagePort,
    closed: Cell<bool>,
}
impl SessionSender {
    pub fn new(worker: SharedWorker, port: MessagePort) -> Self {
        Self(Rc::new(Connection { worker, port, closed: Cell::new(false) }))
    }
    pub fn send(&self, bytes: &[u8]) -> Result<(), String> {
        if self.0.closed.get() {
            return Err("Worker connection closed".into());
        }
        let bytes = js_sys::Uint8Array::from(bytes);
        self.0.port.post_message_with_transferable(&bytes, &js_sys::Array::of1(&bytes.buffer())).map_err(|error| format!("{error:?}"))
    }
    pub fn close(&self) {
        if self.0.closed.replace(true) {
            return;
        }
        let _ = self.0.port.post_message(&"disconnect".into());
        self.0.port.set_onmessage(None);
        self.0.port.set_onmessageerror(None);
        self.0.worker.set_onerror(None);
        self.0.port.close();
    }
}

/// Dropping this owner also cleans up an interrupted connection attempt.
pub struct Session {
    sender: SessionSender,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _error: Closure<dyn FnMut(JsValue)>,
}
impl Session {
    pub fn new(sender: SessionSender, mut message: impl FnMut(SessionMessage) + 'static, mut error: impl FnMut(String) + 'static) -> Self {
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let data = event.data();
            let kind = super::field(&data, "kind").as_string();
            match kind.as_deref() {
                Some("reconnecting") => message(SessionMessage::Reconnecting),
                Some("failed") => message(SessionMessage::Failed(super::field(&data, "error").as_string().unwrap_or_else(|| "Worker connection failed".into()))),
                Some("diagnostic") => web_sys::console::info_1(&super::field(&data, "message")),
                _ => message(SessionMessage::Bytes(js_sys::Uint8Array::new(&data).to_vec())),
            }
        });
        let on_error = Closure::<dyn FnMut(JsValue)>::new(move |event| {
            error(super::field(&event, "message").as_string().filter(|value| !value.is_empty()).unwrap_or_else(|| "shared backend worker stopped or sent an unreadable message".into()));
        });
        sender.0.worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        sender.0.port.set_onmessageerror(Some(on_error.as_ref().unchecked_ref()));
        sender.0.port.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        sender.0.port.start();
        Self { sender, _message: on_message, _error: on_error }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.sender.close();
    }
}
