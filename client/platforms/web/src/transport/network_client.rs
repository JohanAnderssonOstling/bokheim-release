//! Fetch-compatible client for HTTP requests executed by the coordinator.
use super::{field, object, Port};
use futures_channel::oneshot;
use js_sys::{Array, Function, Promise, Reflect, Uint8Array};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, JsFuture};
use web_sys::{AbortSignal, DedicatedWorkerGlobalScope, MessageChannel, ReadableStreamDefaultController, Request};

thread_local! {
    static ACTIVE: super::pending::Pending<Rc<Pending>> = Default::default();
    static ROUTE: RefCell<Option<Rc<Route>>> = const { RefCell::new(None) };
    static NATIVE_FETCH: RefCell<Option<Function>> = const { RefCell::new(None) };
}
struct Pending {
    id: u64,
    port: RefCell<Option<Port>>,
    signal: AbortSignal,
    abort: Closure<dyn FnMut()>,
    controller: RefCell<Option<ReadableStreamDefaultController>>,
    response: RefCell<Option<oneshot::Sender<Result<JsValue, JsValue>>>>,
    pull: RefCell<Option<oneshot::Sender<()>>>,
}
impl Pending {
    fn send(&self, kind: &str) -> Result<(), JsValue> {
        if let Some(port) = self.port.borrow().as_ref() {
            port.raw.post_message(&object(&[("kind", kind.into())]))?;
        }
        Ok(())
    }
    fn finish_pull(&self) {
        if let Some(done) = self.pull.borrow_mut().take() {
            let _ = done.send(());
        }
    }
    fn close(&self) {
        let Some(_port) = self.port.borrow_mut().take() else { return };
        let _ = self.signal.remove_event_listener_with_callback("abort", self.abort.as_ref().unchecked_ref());
        self.controller.borrow_mut().take();
        self.finish_pull();
        ACTIVE.with(|active| active.take(self.id));
    }
    fn fail(&self, error: JsValue) {
        if self.port.borrow().is_none() {
            return;
        }
        let _ = self.send("cancel");
        if let Some(controller) = self.controller.borrow().as_ref() {
            controller.error_with_e(&error);
        }
        if let Some(response) = self.response.borrow_mut().take() {
            let _ = response.send(Err(error));
        }
        self.close();
    }
    fn receive(self: &Rc<Self>, data: JsValue) -> Result<(), JsValue> {
        if self.port.borrow().is_none() {
            return Ok(());
        }
        use super::control::NetworkEvent;
        match NetworkEvent::decode(&data).map_err(|error| js_sys::TypeError::new(&error))? {
            NetworkEvent::Error(error) => self.fail(js_sys::TypeError::new(&error).into()),
            NetworkEvent::Headers { has_body } => {
                if self.response.borrow().is_none() {
                    return Err(js_sys::TypeError::new("Duplicate network headers").into());
                }
                let body = if has_body { self.stream()? } else { JsValue::NULL };
                let options = object(&[("status", field(&data, "status")), ("statusText", field(&data, "statusText")), ("headers", field(&data, "headers"))]);
                let response = construct("Response", &Array::of2(&body, &options))?;
                for key in ["url", "redirected"] {
                    Reflect::define_property(response.unchecked_ref::<js_sys::Object>(), &key.into(), object(&[("value", field(&data, key))]).unchecked_ref())?;
                }
                if let Some(reply) = self.response.borrow_mut().take() {
                    let _ = reply.send(Ok(response));
                }
                if !has_body {
                    self.close();
                }
            }
            NetworkEvent::Chunk(bytes) => {
                if self.response.borrow().is_some() || self.controller.borrow().is_none() {
                    return Err(js_sys::TypeError::new("Network chunk before headers").into());
                }
                if let Some(controller) = self.controller.borrow().as_ref() {
                    controller.enqueue_with_chunk(&bytes)?;
                }
                self.finish_pull();
            }
            NetworkEvent::Done => {
                if self.response.borrow().is_some() {
                    return Err(js_sys::TypeError::new("Network completion before headers").into());
                }
                if let Some(controller) = self.controller.borrow().as_ref() {
                    controller.close()?;
                }
                self.close();
            }
        }
        Ok(())
    }
    fn stream(self: &Rc<Self>) -> Result<JsValue, JsValue> {
        let weak = Rc::downgrade(self);
        let start = Closure::<dyn FnMut(ReadableStreamDefaultController)>::new(move |controller| {
            if let Some(state) = weak.upgrade() {
                *state.controller.borrow_mut() = Some(controller);
            }
        })
        .into_js_value();
        let weak = Rc::downgrade(self);
        let pull = Closure::<dyn FnMut() -> Promise>::new(move || {
            let Some(state) = weak.upgrade() else {
                return Promise::resolve(&JsValue::UNDEFINED);
            };
            let (done, waiting) = oneshot::channel();
            *state.pull.borrow_mut() = Some(done);
            if let Err(error) = state.send("pull") {
                state.fail(error);
            }
            future_to_promise(async move {
                let _ = waiting.await;
                Ok(JsValue::UNDEFINED)
            })
        })
        .into_js_value();
        let weak = Rc::downgrade(self);
        let cancel = Closure::<dyn FnMut()>::new(move || {
            if let Some(state) = weak.upgrade() {
                let _ = state.send("cancel");
                state.close();
            }
        })
        .into_js_value();
        construct("ReadableStream", &Array::of2(&object(&[("start", start), ("pull", pull), ("cancel", cancel)]), &object(&[("highWaterMark", 0.into())])))
    }
}
fn construct(name: &str, args: &Array) -> Result<JsValue, JsValue> {
    let constructor: Function = Reflect::get(&js_sys::global(), &name.into())?.dyn_into()?;
    Reflect::construct(&constructor, args).map(Into::into)
}
fn check_abort(signal: &AbortSignal) -> Result<(), JsValue> {
    if signal.aborted() {
        Err(signal.reason())
    } else {
        Ok(())
    }
}
async fn fetch(input: JsValue, init: JsValue) -> Result<JsValue, JsValue> {
    let request: Request = construct("Request", &Array::of2(&input, &init))?.unchecked_into();
    let signal = request.signal();
    check_abort(&signal)?;
    // Files/Blobs are structured-cloned to the coordinator without materializing
    // their bytes. Keep the byte fallback for general fetch callers and Firefox.
    let supplied_body = field(&init, "body");
    let body = if request.method() == "GET" || request.method() == "HEAD" {
        JsValue::UNDEFINED
    } else if supplied_body.is_instance_of::<web_sys::Blob>() {
        supplied_body
    } else {
        Uint8Array::new(&JsFuture::from(request.array_buffer()?).await?).into()
    };
    check_abort(&signal)?;
    let channel = MessageChannel::new()?;
    let (reply, waiting) = oneshot::channel();
    let id = ACTIVE.with(|active| active.next_id());
    let state = Rc::new_cyclic(|weak: &std::rc::Weak<Pending>| {
        let receive = weak.clone();
        let error = weak.clone();
        let abort = weak.clone();
        Pending {
            id,
            signal,
            abort: Closure::<dyn FnMut()>::new(move || {
                if let Some(state) = abort.upgrade() {
                    state.fail(state.signal.reason());
                }
            }),
            controller: RefCell::new(None),
            response: RefCell::new(Some(reply)),
            pull: RefCell::new(None),
            port: RefCell::new(Some(Port::new(
                channel.port1(),
                move |event| {
                    if let Some(state) = receive.upgrade() {
                        if let Err(error) = state.receive(event.data()) {
                            state.fail(error);
                        }
                    }
                },
                move |_| {
                    if let Some(state) = error.upgrade() {
                        state.fail(js_sys::Error::new("Invalid network response message").into());
                    }
                },
            ))),
        }
    });
    ACTIVE.with(|active| active.insert(id, state.clone()));
    let sent = (|| {
        state.signal.add_event_listener_with_callback("abort", state.abort.as_ref().unchecked_ref())?;
        let options = js_sys::Object::new();
        for key in ["method", "mode", "credentials", "cache", "redirect", "referrer", "referrerPolicy", "integrity", "keepalive"] {
            Reflect::set(&options, &key.into(), &Reflect::get(&request, &key.into())?)?;
        }
        Reflect::set(&options, &"headers".into(), &Array::from(&request.headers()))?;
        let transfer = Array::of1(&channel.port2());
        if body.is_instance_of::<Uint8Array>() {
            transfer.push(&field(&body, "buffer"));
        }
        let message = object(&[("transport", "network_request".into()), ("port", channel.port2().into()), ("request", object(&[("url", request.url().into()), ("options", options.into()), ("body", body)]))]);
        let route = ROUTE.with(|route| route.borrow().clone());
        if let Some(route) = route {
            route(message, &transfer)?;
        } else {
            let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
            scope.post_message_with_transfer(&message, &transfer)?;
        }
        Ok::<(), JsValue>(())
    })();
    if let Err(error) = sent {
        channel.port2().close();
        state.fail(error);
    }
    waiting.await.map_err(|_| js_sys::Error::new("Network request closed"))?
}
pub fn fail_all(error: JsValue) {
    ACTIVE.with(|active| active.fail_all(|state| state.fail(error.clone())));
}
pub fn install() -> Result<(), JsValue> {
    if NATIVE_FETCH.with(|slot| slot.borrow().is_some()) {
        return Ok(());
    }
    let native = Reflect::get(&js_sys::global(), &"fetch".into())?.dyn_into::<Function>()?;
    let fetch = Closure::<dyn FnMut(JsValue, JsValue) -> Promise>::new(|input, init| future_to_promise(fetch(input, init)));
    if !Reflect::set(&js_sys::global(), &"fetch".into(), fetch.as_ref())? {
        return Err(js_sys::Error::new("Could not install backend fetch bridge").into());
    }
    let _ = fetch.into_js_value();
    NATIVE_FETCH.with(|slot| *slot.borrow_mut() = Some(native));
    Ok(())
}

pub fn native_fetch() -> Result<Function, JsValue> {
    match NATIVE_FETCH.with(|slot| slot.borrow().clone()) {
        Some(fetch) => Ok(fetch),
        None => Reflect::get(&js_sys::global(), &"fetch".into())?.dyn_into(),
    }
}

type Route = dyn Fn(JsValue, &Array) -> Result<(), JsValue>;
/// The coordinator supplies routing directly; database workers use postMessage.
pub fn set_route(route: impl Fn(JsValue, &Array) -> Result<(), JsValue> + 'static) {
    ROUTE.with(|slot| *slot.borrow_mut() = Some(Rc::new(route)));
}
