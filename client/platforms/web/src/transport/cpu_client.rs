use super::{error_message, object, read_byte_reply, BytesFuture, Port};
use crate::reply::{send_once, OwnedReply};
use async_channel::bounded;
use js_sys::{Array, Uint8Array};
use wasm_bindgen::prelude::*;
use web_sys::{DedicatedWorkerGlobalScope, MessageChannel};

/// Each CPU request owns its reply port; no global listener or correlation map.
pub fn request_with_source(bytes: Vec<u8>, source: JsValue) -> Result<BytesFuture, String> {
    let channel = MessageChannel::new().map_err(|error| error_message(&error))?;
    let (reply, response) = bounded(1);
    let malformed = reply.clone();
    let port = Port::listen(
        channel.port1(),
        move |data| {
            send_once(&reply, read_byte_reply(&data));
        },
        move || {
            send_once(&malformed, Err("Invalid CPU response".into()));
        },
    );
    let reply = OwnedReply::new(response, move || {
        let _ = port.raw.post_message(&object(&[("kind", "cancel".into())]));
    });
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
        let result = reply.receive().await.unwrap_or_else(|_| Err("CPU connection closed".into()));
        drop(reply);
        result
    }))
}
