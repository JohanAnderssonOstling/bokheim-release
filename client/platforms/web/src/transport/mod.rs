//! Browser worker transport. Payloads are opaque; callers own application policy.
pub mod bridge;
pub mod cpu;
pub mod cpu_client;
pub mod endpoint;
pub mod network_client;
pub mod network_server;
pub mod pending;
pub mod port;
mod relay;
pub use port::Port;
pub use relay::ClientRelay;
pub type BytesOperation = std::rc::Rc<dyn Fn(Vec<u8>) -> BytesFuture>;
pub type BytesFuture = futures_util::future::LocalBoxFuture<'static, Result<Vec<u8>, String>>;

use js_sys::{Array, ArrayBuffer, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::MessagePort;

pub fn failure(error: &str) -> JsValue {
    let message = Object::new();
    Reflect::set(&message, &"kind".into(), &"failed".into()).expect("plain object field");
    Reflect::set(&message, &"error".into(), &error.into()).expect("plain object field");
    message.into()
}

/// Transfer owned buffers instead of copying book bytes between workers.
pub fn forward(port: &MessagePort, data: &JsValue) -> Result<(), JsValue> {
    let transfer = Array::new();
    if data.is_instance_of::<ArrayBuffer>() {
        transfer.push(data);
    } else if ArrayBuffer::is_view(data) {
        transfer.push(&Reflect::get(data, &"buffer".into())?);
    }
    port.post_message_with_transferable(data, &transfer)
}

pub fn field(value: &JsValue, name: &str) -> JsValue {
    Reflect::get(value, &name.into()).unwrap_or(JsValue::UNDEFINED)
}

pub fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let object = Object::new();
    for (name, value) in fields {
        Reflect::set(&object, &(*name).into(), value).expect("plain object field");
    }
    object.into()
}

/// The common byte/error envelope used by CPU, sync storage and transfer replies.
pub fn read_byte_reply(message: &JsValue) -> Result<Vec<u8>, String> {
    protocol::byte_result(message).unwrap_or_else(Err)
}

pub fn byte_reply(fields: &[(&str, JsValue)], result: Result<Vec<u8>, String>) -> (JsValue, Array) {
    let message = object(fields);
    let transfer = Array::new();
    let (key, value) = match result {
        Ok(bytes) => {
            let bytes = js_sys::Uint8Array::from(bytes.as_slice());
            transfer.push(&bytes.buffer());
            ("bytes", bytes.into())
        }
        Err(error) => ("error", error.into()),
    };
    Reflect::set(&message, &key.into(), &value).expect("plain object field");
    (message, transfer)
}

pub fn error_message(error: &JsValue) -> String {
    error.as_string().or_else(|| field(error, "message").as_string()).unwrap_or_else(|| format!("{error:?}"))
}

#[cfg(feature = "web-runtime-tests")]
pub use port::lifecycle_contract as port_lifecycle_contract;
pub mod lifetime;
pub mod tab;

pub mod session;

pub mod range;

pub mod import_io;
pub mod resources;
pub mod scope;
pub mod socket;

pub mod upload;

pub mod protocol;
pub mod request;

pub mod control;

pub mod host;

pub mod transfer;
