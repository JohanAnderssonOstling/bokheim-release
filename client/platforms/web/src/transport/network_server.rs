//! Fetch and body reads run asynchronously in the coordinator; CPU and SQLite
//! remain in dedicated workers so they cannot block stream delivery.
use super::{error_message, field, object, Port};
use js_sys::{Array, Function, Object, Promise, Reflect};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortController, DedicatedWorkerGlobalScope, MessageEvent, MessagePort, ReadableStreamDefaultReader, Response};
thread_local! {static REQUESTS: super::pending::Pending<Rc<Request>> = Default::default();}
struct Request {
    id: u64,
    port: RefCell<Option<Port>>,
    controller: AbortController,
    reader: RefCell<Option<ReadableStreamDefaultReader>>,
    reading: Cell<bool>,
}
impl Request {
    fn close(&self) {
        let Some(_port) = self.port.borrow_mut().take() else { return };
        self.controller.abort();
        if let Some(reader) = self.reader.borrow_mut().take() {
            wasm_bindgen_futures::spawn_local(async move {
                let _ = JsFuture::from(reader.cancel()).await;
            });
        }
        REQUESTS.with(|requests| requests.take(self.id));
    }
    fn send(&self, message: &JsValue, transfer: &Array) -> Result<(), JsValue> {
        if let Some(port) = self.port.borrow().as_ref() {
            port.raw.post_message_with_transferable(message, transfer)
        } else {
            Ok(())
        }
    }
    fn fail(&self, error: JsValue) {
        if self.port.borrow().is_none() {
            return;
        }
        let _ = self.send(&object(&[("kind", "error".into()), ("error", error_message(&error).into())]), &Array::new());
        self.close();
    }
    fn receive(self: &Rc<Self>, data: JsValue) {
        use super::control::NetworkCommand;
        match NetworkCommand::decode(&data) {
            Ok(NetworkCommand::Cancel) => self.close(),
            Ok(NetworkCommand::Pull) if self.port.borrow().is_some() && !self.reading.get() => {
                let Some(reader) = self.reader.borrow().clone() else {
                    return;
                };
                self.reading.set(true);
                let state = self.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let result = JsFuture::from(reader.read()).await;
                    if state.port.borrow().is_none() {
                        return;
                    }
                    match result {
                        Ok(result) if field(&result, "done").as_bool() == Some(true) => {
                            let _ = state.send(&object(&[("kind", "done".into())]), &Array::new());
                            state.close();
                        }
                        Ok(result) => {
                            let bytes = field(&result, "value");
                            let transfer = Array::of1(&field(&bytes, "buffer"));
                            if let Err(error) = state.send(&object(&[("kind", "chunk".into()), ("bytes", bytes)]), &transfer) {
                                state.fail(error);
                            }
                        }
                        Err(error) => state.fail(error),
                    }
                    state.reading.set(false);
                });
            }
            Ok(NetworkCommand::Pull) => {}
            Err(error) => self.fail(js_sys::TypeError::new(&error).into()),
        }
    }
}

pub fn serve(port: MessagePort, input: JsValue, fetch: Option<Function>) -> Result<(), JsValue> {
    let request = super::control::NetworkRequest::decode(&input).map_err(|error| js_sys::TypeError::new(&error))?;
    let id = REQUESTS.with(|requests| requests.next_id());
    let controller = AbortController::new()?;
    let state = Rc::new_cyclic(|weak: &std::rc::Weak<Request>| {
        let message = weak.clone();
        let error = weak.clone();
        Request {
            id,
            port: RefCell::new(Some(Port::new(
                port,
                move |event| {
                    if let Some(state) = message.upgrade() {
                        state.receive(event.data());
                    }
                },
                move |_| {
                    if let Some(state) = error.upgrade() {
                        state.fail(js_sys::Error::new("Invalid network request message").into());
                    }
                },
            ))),
            controller,
            reader: RefCell::new(None),
            reading: Cell::new(false),
        }
    });
    REQUESTS.with(|requests| requests.insert(id, state.clone()));
    let started = (|| {
        let options = Object::assign(&Object::new(), &request.options);
        Reflect::set(&options, &"body".into(), &request.body)?;
        Reflect::set(&options, &"signal".into(), &state.controller.signal())?;
        let global = js_sys::global();
        let fetch = match fetch {
            Some(fetch) => fetch,
            None => Reflect::get(&global, &"fetch".into())?.dyn_into::<Function>()?,
        };
        fetch.call2(&global, &request.url.into(), &options)
    })();
    wasm_bindgen_futures::spawn_local(async move {
        let result = async {
            let response: Response = JsFuture::from(Promise::resolve(&started?)).await?.unchecked_into();
            if state.port.borrow().is_none() {
                return Ok(());
            }
            if response.status() == 0 {
                return Err(js_sys::Error::new("Opaque HTTP responses are not supported by the backend").into());
            }
            // Some browsers expose an empty stream even for null-body statuses.
            // Response construction rejects a stream for these statuses.
            let bodyless = matches!(response.status(), 204 | 205 | 304) || field(&field(&input, "options"), "method").as_string().as_deref() == Some("HEAD");
            let reader = if bodyless { None } else { response.body().map(|body| body.get_reader().unchecked_into::<ReadableStreamDefaultReader>()) };
            let has_body = reader.is_some();
            *state.reader.borrow_mut() = reader;
            state.send(
                &object(&[
                    ("kind", "headers".into()),
                    ("status", response.status().into()),
                    ("statusText", response.status_text().into()),
                    ("headers", Array::from(&response.headers()).into()),
                    ("url", response.url().into()),
                    ("redirected", response.redirected().into()),
                    ("hasBody", has_body.into()),
                ]),
                &Array::new(),
            )?;
            if !has_body {
                state.close();
            }
            Ok::<(), JsValue>(())
        }
        .await;
        if let Err(error) = result {
            state.fail(error);
        }
    });
    Ok(())
}

pub fn fail_all(error: JsValue) {
    REQUESTS.with(|requests| requests.fail_all(|request| request.fail(error.clone())));
}

pub fn start() -> Result<(), JsValue> {
    let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let listener = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let data = event.data();
        if field(&data, "transport").as_string().as_deref() != Some("fetch") {
            return;
        }
        let port: MessagePort = field(&data, "port").unchecked_into();
        if let Err(error) = serve(port.clone(), field(&data, "request"), None) {
            let _ = port.post_message(&object(&[("kind", "error".into()), ("error", error_message(&error).into())]));
            port.close();
        }
    })
    .into_js_value();
    scope.set_onmessage(Some(listener.unchecked_ref()));
    Ok(())
}
