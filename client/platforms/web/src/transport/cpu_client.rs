use super::{error_message, object, read_byte_reply, BytesFuture, Port};
use futures_channel::oneshot;
use js_sys::{Array, Uint8Array};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use web_sys::{DedicatedWorkerGlobalScope, MessageChannel};

type Reply = Rc<RefCell<Option<oneshot::Sender<Result<Vec<u8>, String>>>>>;

/// The pending sender is the single source of truth for settlement and cancellation.
struct ReplyPort {
    port: Port,
    reply: Reply,
}
impl Drop for ReplyPort {
    fn drop(&mut self) {
        if self.reply.borrow().is_some() {
            let _ = self.port.raw.post_message(&object(&[("kind", "cancel".into())]));
        }
    }
}

/// Each CPU request owns its reply port; no global listener or correlation map.
pub fn request(bytes: Vec<u8>) -> Result<BytesFuture, String> {
    request_with_source(bytes, JsValue::UNDEFINED)
}

pub fn request_with_source(bytes: Vec<u8>, source: JsValue) -> Result<BytesFuture, String> {
    let channel = MessageChannel::new().map_err(|error| error_message(&error))?;
    let (reply, response) = oneshot::channel();
    let reply = Rc::new(RefCell::new(Some(reply)));
    let malformed = reply.clone();
    let pending = reply.clone();
    let port = Port::listen(
        channel.port1(),
        move |data| {
            if let Some(reply) = reply.borrow_mut().take() {
                let _ = reply.send(read_byte_reply(&data));
            }
        },
        move || {
            if let Some(reply) = malformed.borrow_mut().take() {
                let _ = reply.send(Err("Invalid CPU response".into()));
            }
        },
    );
    let port = ReplyPort { port, reply: pending };
    let bytes = Uint8Array::from(bytes.as_slice());
    let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let message = object(&[("transport", "cpu_request".into()), ("port", channel.port2().into()), ("bytes", bytes.clone().into()), ("source", source.clone())]);
    let transfer = Array::of2(&channel.port2(), &bytes.buffer());
    if !source.is_undefined() {
        transfer.push(&super::field(&source, "port"));
    }
    if let Err(error) = scope.post_message_with_transfer(&message, &transfer) {
        channel.port2().close();
        return Err(error_message(&error));
    }
    Ok(Box::pin(async move {
        let _port = port;
        response.await.unwrap_or_else(|_| Err("CPU connection closed".into()))
    }))
}
