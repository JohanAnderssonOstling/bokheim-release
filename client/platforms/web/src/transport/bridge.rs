//! Database-worker endpoints for coordinator messages. Owns cancellation and ports.

use super::protocol::BridgeEvent;
use super::{byte_reply, error_message, field, object, BytesFuture, Port};
use crate::reply::{send_once, OwnedReply};
use client_platform_runtime::pending::OnDrop;
use js_sys::{Array, Uint8Array};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use web_sys::{DedicatedWorkerGlobalScope, MessageChannel, MessageEvent};

type Cancel = Box<dyn FnOnce(JsValue)>;
struct Bridge {
    scope: DedicatedWorkerGlobalScope,
    pending: super::pending::Pending<Cancel>,
}
thread_local! {static BRIDGE:RefCell<Option<Rc<Bridge>>>=const{RefCell::new(None)};}
fn bridge() -> Result<Rc<Bridge>, JsValue> {
    BRIDGE.with(|slot| {
        if let Some(bridge) = slot.borrow().as_ref() {
            return Ok(bridge.clone());
        }
        let bridge = Rc::new(Bridge { scope: js_sys::global().unchecked_into(), pending: Default::default() });
        let weak = Rc::downgrade(&bridge);
        let listener = Closure::<dyn FnMut(MessageEvent)>::own_aborting(move |event: MessageEvent| {
            let data = event.data();
            if field(&data, "transport").as_string().as_deref() != Some("sync_failed") {
                return;
            }
            if let Some(bridge) = weak.upgrade() {
                let error: JsValue = js_sys::Error::new(&error_message(&field(&data, "error"))).into();
                bridge.pending.fail_all(|cancel| cancel(error.clone()));
            }
        })
        .into_js_value();
        bridge.scope.add_event_listener_with_callback("message", listener.unchecked_ref())?;
        *slot.borrow_mut() = Some(bridge.clone());
        Ok(bridge)
    })
}
pub fn sync_request(key: String, bytes: Vec<u8>, run: impl AsyncFnMut(Vec<u8>) -> Result<Vec<u8>, String> + 'static) -> Result<BytesFuture, JsValue> {
    let bridge = bridge()?;
    let id = bridge.pending.next_id().map_err(|error| JsValue::from_str(&error))?;
    let channel = MessageChannel::new()?;
    // Completion and commands use separate channels so completion can cancel an
    // active operation without waiting for it or running any queued commands.
    let (completed, response) = async_channel::bounded(1);
    let (commands, incoming) = async_channel::unbounded();
    let done = completed.clone();
    let invalid = completed.clone();
    let port = Port::new(
        channel.port1(),
        move |event| match BridgeEvent::decode(&event.data()) {
            Ok(BridgeEvent::Complete(result)) => {
                send_once(&done, result);
                commands.close();
            }
            Ok(BridgeEvent::Storage { id, bytes }) => {
                let _ = commands.try_send((id, bytes));
            }
            Err(error) => {
                send_once(&done, Err(error));
                commands.close();
            }
        },
        move |_| {
            send_once(&invalid, Err("Invalid coordinator message".into()));
        },
    );
    let response_port = port.raw.clone();
    let reply = OwnedReply::new(response.clone(), move || {
        let _ = port.raw.post_message(&object(&[("kind", "cancel".into())]));
    });
    let owner = bridge.clone();
    let registration = OnDrop::new(move || {
        owner.pending.take(id);
    });
    let failed = completed.clone();
    bridge.pending.insert(
        id,
        Box::new(move |error| {
            send_once(&failed, Err(error_message(&error)));
        }),
    );
    let message = object(&[("transport", "sync_request".into()), ("key", key.into()), ("port", channel.port2().into())]);
    let transfer = Array::of1(&channel.port2());
    let bytes = Uint8Array::from(bytes.as_slice());
    transfer.push(&bytes.buffer());
    js_sys::Reflect::set(&message, &"bytes".into(), &bytes)?;
    if let Err(error) = bridge.scope.post_message_with_transfer(&message, &transfer) {
        channel.port2().close();
        send_once(&completed, Err(error_message(&error)));
    }
    Ok(Box::pin(async move {
        // This future owns the port, handler, and registration even before its
        // first poll. Dropping it cancels the active operation and queued work.
        let result = crate::serial::serve_storage(run, incoming, response, |id, result| {
            let (message, transfer) = byte_reply(&[("id", id.into())], result);
            response_port.post_message_with_transferable(&message, &transfer).map_err(|error| error_message(&error))
        })
        .await;
        drop(reply);
        drop(registration);
        result
    }))
}
