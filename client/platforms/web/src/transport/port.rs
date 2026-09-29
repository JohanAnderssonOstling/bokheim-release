use wasm_bindgen::{closure::Closure, JsCast};
use web_sys::{MessageEvent, MessagePort};

/// Owns browser callbacks and closes the port before releasing their closures.
pub struct Port {
    pub raw: MessagePort,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _error: Closure<dyn FnMut(MessageEvent)>,
}
impl Port {
    /// Application adapters receive payloads; the platform owns browser events.
    pub fn listen(raw: MessagePort, mut message: impl FnMut(wasm_bindgen::JsValue) + 'static, mut malformed: impl FnMut() + 'static) -> Self {
        Self::new(raw, move |event| message(event.data()), move |_| malformed())
    }

    pub fn new(raw: MessagePort, message: impl FnMut(web_sys::MessageEvent) + 'static, error: impl FnMut(web_sys::MessageEvent) + 'static) -> Self {
        let message = Closure::<dyn FnMut(MessageEvent)>::new(message);
        let error = Closure::<dyn FnMut(MessageEvent)>::new(error);
        raw.set_onmessage(Some(message.as_ref().unchecked_ref()));
        raw.set_onmessageerror(Some(error.as_ref().unchecked_ref()));
        raw.start();
        Self { raw, _message: message, _error: error }
    }
}
impl Drop for Port {
    fn drop(&mut self) {
        self.raw.set_onmessage(None);
        self.raw.set_onmessageerror(None);
        self.raw.close();
    }
}

#[cfg(feature = "web-runtime-tests")]
pub async fn lifecycle_contract() -> Result<bool, wasm_bindgen::JsValue> {
    use std::{cell::RefCell, rc::Rc};
    let resource = Rc::new(());
    let channel = web_sys::MessageChannel::new()?;
    let message_resource = resource.clone();
    let error_resource = resource.clone();
    let port = Port::listen(
        channel.port1(),
        move |_| {
            let _ = &message_resource;
        },
        move || {
            let _ = &error_resource;
        },
    );
    if Rc::strong_count(&resource) != 3 {
        return Ok(false);
    }
    drop(port);
    if Rc::strong_count(&resource) != 1 || channel.port1().onmessage().is_some() || channel.port1().onmessageerror().is_some() {
        return Ok(false);
    }
    channel.port2().close();

    // Releasing the owner during its own callback must be safe too.
    let channel = web_sys::MessageChannel::new()?;
    let owner = Rc::new(RefCell::new(None));
    let weak = Rc::downgrade(&owner);
    let captured = resource.clone();
    let (reply, response) = futures_channel::oneshot::channel();
    let mut reply = Some(reply);
    *owner.borrow_mut() = Some(Port::listen(
        channel.port1(),
        move |data| {
            let _ = &captured;
            if let Some(owner) = weak.upgrade() {
                owner.borrow_mut().take();
            }
            if let Some(reply) = reply.take() {
                let _ = reply.send(data.as_string().as_deref() == Some("payload"));
            }
        },
        || {},
    ));
    channel.port2().post_message(&"payload".into())?;
    let delivered = response.await.unwrap_or(false);
    channel.port2().close();
    Ok(delivered && owner.borrow().is_none() && Rc::strong_count(&resource) == 1)
}
