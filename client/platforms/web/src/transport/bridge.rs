//! Database-worker endpoints for coordinator messages. Owns cancellation and ports.
use super::protocol::BridgeEvent;
use super::{byte_reply, error_message, field, object, BytesFuture, BytesOperation, Port};
use futures_channel::oneshot;
#[cfg(feature = "web-runtime-tests")]
use js_sys::Promise;
use js_sys::{Array, Uint8Array};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
#[cfg(feature = "web-runtime-tests")]
use wasm_bindgen_futures::future_to_promise;
use web_sys::{DedicatedWorkerGlobalScope, MessageChannel, MessageEvent};

type Cancel = Box<dyn FnOnce(JsValue)>;
struct Bridge {
    scope: DedicatedWorkerGlobalScope,
    pending: super::pending::Pending<Cancel>,
}
thread_local! {static BRIDGE:RefCell<Option<Rc<Bridge>>>=const{RefCell::new(None)};}
thread_local! {static ACTIVE_TRANSFERS:RefCell<HashMap<String, std::rc::Weak<Request>>>=RefCell::new(HashMap::new());}
fn bridge() -> Result<Rc<Bridge>, JsValue> {
    BRIDGE.with(|slot| {
        if let Some(bridge) = slot.borrow().as_ref() {
            return Ok(bridge.clone());
        }
        let bridge = Rc::new(Bridge { scope: js_sys::global().unchecked_into(), pending: Default::default() });
        let weak = Rc::downgrade(&bridge);
        let listener = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
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
type Reply = oneshot::Sender<Result<Vec<u8>, String>>;
enum Runner {
    Storage { bytes: Vec<u8>, run: BytesOperation },
    Transfer(RefCell<Option<Box<dyn FnOnce() -> BytesFuture>>>),
}
struct Request {
    bridge: Rc<Bridge>,
    id: u64,
    key: String,
    port: RefCell<Option<Port>>,
    reply: RefCell<Option<Reply>>,
    cancelled: async_channel::Receiver<()>,
    runner: Runner,
    progress: Option<Box<dyn Fn(f64)>>,
}
impl Request {
    fn forget_progress(&self) {
        ACTIVE_TRANSFERS.with(|active| {
            let mut active = active.borrow_mut();
            if active.get(&self.key).is_some_and(|owner| std::ptr::eq(owner.as_ptr(), self)) {
                active.remove(&self.key);
            }
        });
    }
    fn finish(&self, result: Result<Vec<u8>, String>) {
        self.forget_progress();
        self.cancelled.close();
        self.port.borrow_mut().take();
        self.bridge.pending.take(self.id);
        if let Some(reply) = self.reply.borrow_mut().take() {
            let _ = reply.send(result);
        }
    }
    fn receive(self: &Rc<Self>, data: JsValue) {
        match BridgeEvent::decode(&data) {
            Ok(BridgeEvent::Complete(result)) => self.finish(result),
            Ok(BridgeEvent::Storage { id, bytes }) if matches!(self.runner, Runner::Storage { .. }) => self.execute(Some((id, bytes))),
            Ok(BridgeEvent::Run) if matches!(self.runner, Runner::Transfer(_)) => self.execute(None),
            Ok(BridgeEvent::Progress(fraction)) if matches!(self.runner, Runner::Transfer(_)) => {
                if let Some(progress) = &self.progress {
                    progress(fraction);
                }
            }
            Ok(_) => self.finish(Err("Worker requested an unavailable capability".into())),
            Err(error) => self.finish(Err(error)),
        }
    }
    fn execute(self: &Rc<Self>, storage: Option<(u32, Vec<u8>)>) {
        if self.cancelled.is_closed() {
            return;
        }
        let (id, run) = match (&self.runner, storage) {
            (Runner::Storage { run, .. }, Some((id, bytes))) => (Some(id), run(bytes)),
            (Runner::Transfer(run), None) => {
                let Some(run) = run.borrow_mut().take() else {
                    self.finish(Err("Transfer capability already consumed".into()));
                    return;
                };
                ACTIVE_TRANSFERS.with(|active| {
                    active.borrow_mut().insert(self.key.clone(), Rc::downgrade(self));
                });
                (None, run())
            }
            _ => {
                self.finish(Err("Worker requested an unavailable capability".into()));
                return;
            }
        };
        let state = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = tokio::select! {
                biased;
                _ = state.cancelled.recv() => return,
                result = run => result,
            };
            let header = if let Some(id) = id { ("id", id.into()) } else { ("kind", if result.is_ok() { "result" } else { "error" }.into()) };
            let (message, transfer) = byte_reply(&[header], result);
            let result = state.port.borrow().as_ref().map(|port| port.raw.post_message_with_transferable(&message, &transfer));
            if let Some(Err(error)) = result {
                state.finish(Err(error_message(&error)));
            }
        });
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        self.forget_progress();
        if matches!(self.runner, Runner::Storage { .. }) {
            if let Some(port) = self.port.get_mut().as_ref() {
                let _ = port.raw.post_message(&object(&[("kind", "cancel".into())]));
            }
        }
        self.bridge.pending.take(self.id);
    }
}
fn request(key: String, mut runner: Runner, progress: Option<Box<dyn Fn(f64)>>) -> Result<BytesFuture, JsValue> {
    let bridge = bridge()?;
    let id = bridge.pending.next_id();
    let channel = MessageChannel::new()?;
    let (reply, response) = oneshot::channel();
    // Dropping the returned future closes this sender and cancels active work.
    // finish() closes the receiver to cancel work when a coordinator reply arrives.
    let (cancel, cancelled) = async_channel::unbounded();
    let transport = match &runner {
        Runner::Storage { .. } => "sync_request",
        Runner::Transfer(_) => "transfer_request",
    };
    let message = object(&[("transport", transport.into()), ("key", key.clone().into()), ("port", channel.port2().into())]);
    let transfer = Array::of1(&channel.port2());
    if let Runner::Storage { bytes, .. } = &mut runner {
        // The queued runner retains no input buffer after dispatch.
        let bytes = Uint8Array::from(std::mem::take(bytes).as_slice());
        transfer.push(&bytes.buffer());
        js_sys::Reflect::set(&message, &"bytes".into(), &bytes)?;
    }
    let state = Rc::new_cyclic(|weak: &std::rc::Weak<Request>| {
        let message = weak.clone();
        let error = weak.clone();
        Request {
            bridge: bridge.clone(),
            id,
            key: key.clone(),
            port: RefCell::new(Some(Port::new(
                channel.port1(),
                move |event| {
                    if let Some(state) = message.upgrade() {
                        state.receive(event.data());
                    }
                },
                move |_| {
                    if let Some(state) = error.upgrade() {
                        state.finish(Err("Invalid coordinator message".into()));
                    }
                },
            ))),
            reply: RefCell::new(Some(reply)),
            cancelled,
            runner,
            progress,
        }
    });
    let weak = Rc::downgrade(&state);
    bridge.pending.insert(
        id,
        Box::new(move |error| {
            if let Some(state) = weak.upgrade() {
                state.finish(Err(error_message(&error)));
            }
        }),
    );
    if let Err(error) = bridge.scope.post_message_with_transfer(&message, &transfer) {
        channel.port2().close();
        state.finish(Err(error_message(&error)));
    }
    Ok(Box::pin(async move {
        let _state = state;
        let _cancel = cancel;
        response.await.unwrap_or_else(|_| Err("Coordinator request closed".into()))
    }))
}
pub fn sync_request(key: String, bytes: Vec<u8>, storage: BytesOperation) -> Result<BytesFuture, JsValue> {
    request(key, Runner::Storage { bytes, run: storage }, None)
}
pub fn transfer_request(key: String, run: impl FnOnce() -> BytesFuture + 'static) -> Result<BytesFuture, JsValue> {
    request(key, Runner::Transfer(RefCell::new(Some(Box::new(run)))), None)
}
pub fn transfer_request_with_progress(key: String, run: impl FnOnce() -> BytesFuture + 'static, progress: impl Fn(f64) + 'static) -> Result<BytesFuture, JsValue> {
    request(key, Runner::Transfer(RefCell::new(Some(Box::new(run)))), Some(Box::new(progress)))
}
pub fn report_transfer_progress(key: &str, fraction: f64) {
    if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        return;
    }
    ACTIVE_TRANSFERS.with(|active| {
        let owner = active.borrow().get(key).and_then(std::rc::Weak::upgrade);
        if let Some(owner) = owner {
            if let Some(port) = owner.port.borrow().as_ref() {
                let _ = port.raw.post_message(&object(&[("kind", "progress".into()), ("fraction", fraction.into())]));
            }
        }
    });
}

struct Background {
    bridge: Rc<Bridge>,
    id: u64,
    port: RefCell<Option<Port>>,
    active: Cell<u32>,
    commands: async_channel::Receiver<bool>,
}
impl Background {
    fn stop(&self) {
        let Some(port) = self.port.borrow_mut().take() else { return };
        self.bridge.pending.take(self.id);
        self.commands.close();
        let _ = port.raw.post_message(&object(&[("kind", "stop".into())]));
    }
}
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
pub struct BackgroundHost(Rc<Background>);
impl Drop for BackgroundHost {
    fn drop(&mut self) {
        self.0.stop();
    }
}
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
impl BackgroundHost {
    #[cfg_attr(feature = "web-runtime-tests", wasm_bindgen(constructor))]
    pub fn new(retry: f64, debounce: f64) -> Result<Self, JsValue> {
        let bridge = bridge()?;
        let id = bridge.pending.next_id();
        let channel = MessageChannel::new()?;
        let (send, commands) = async_channel::bounded(1);
        let state = Rc::new_cyclic(|weak: &std::rc::Weak<Background>| {
            let message = weak.clone();
            let error = weak.clone();
            let port = Port::new(
                channel.port1(),
                move |event| {
                    let Some(state) = message.upgrade() else {
                        return;
                    };
                    let data = event.data();
                    use super::control::BackgroundReply;
                    match BackgroundReply::decode(&data) {
                        Ok(BackgroundReply::Stopped) | Err(_) => state.stop(),
                        Ok(BackgroundReply::Cycle { id, refresh }) => {
                            state.active.set(id);
                            if send.try_send(refresh).is_err() {
                                state.stop();
                            }
                        }
                    }
                },
                move |_| {
                    if let Some(state) = error.upgrade() {
                        state.stop();
                    }
                },
            );
            Background { bridge: bridge.clone(), id, port: RefCell::new(Some(port)), active: Cell::new(0), commands }
        });
        let weak = Rc::downgrade(&state);
        bridge.pending.insert(
            id,
            Box::new(move |_| {
                if let Some(state) = weak.upgrade() {
                    state.stop();
                }
            }),
        );
        let options = object(&[("retry", retry.into()), ("debounce", debounce.into())]);
        if let Err(error) = bridge.scope.post_message_with_transfer(&object(&[("transport", "background_start".into()), ("port", channel.port2().into()), ("options", options)]), &Array::of1(&channel.port2())) {
            channel.port2().close();
            state.stop();
            return Err(error);
        }
        Ok(Self(state))
    }
    pub fn notify(&self, kind: &str) {
        if let Some(port) = self.0.port.borrow().as_ref() {
            let _ = port.raw.post_message(&object(&[("kind", kind.into())]));
        }
    }
    pub fn wake_after(&self, delay: f64) {
        if let Some(port) = self.0.port.borrow().as_ref() {
            let _ = port.raw.post_message(&object(&[("kind", "wake_after".into()), ("delay", delay.into())]));
        }
    }
    pub fn finish(&self, success: bool) -> Result<(), JsValue> {
        if let Some(port) = self.0.port.borrow().as_ref() {
            port.raw.post_message(&object(&[("kind", "cycle_result".into()), ("id", self.0.active.get().into()), ("success", success.into())]))?;
        }
        Ok(())
    }
    #[cfg(feature = "web-runtime-tests")]
    pub fn stop(&self) {
        self.0.stop();
    }
    #[cfg(feature = "web-runtime-tests")]
    pub fn next(&self) -> Promise {
        let state = self.0.clone();
        future_to_promise(async move { next_background_cycle(&state).await.map(JsValue::from).map_err(|error| js_sys::Error::new(&error).into()) })
    }
}
impl BackgroundHost {
    pub async fn next_cycle(&self) -> Result<bool, String> {
        next_background_cycle(&self.0).await
    }
}

async fn next_background_cycle(state: &Background) -> Result<bool, String> {
    if state.port.borrow().is_none() {
        return Err("Background scheduler stopped".into());
    }
    let result = state.commands.recv().await;
    if state.port.borrow().is_none() {
        return Err("Background scheduler stopped".into());
    }
    result.map_err(|_| "Background scheduler stopped".to_owned())
}

#[cfg(feature = "web-runtime-tests")]
pub async fn cancellation_contract() -> bool {
    for (cancelled_before_dispatch, coordinator_reply) in [(false, false), (true, false), (false, true), (true, true)] {
        let ran = Rc::new(Cell::new(false));
        let created = Rc::new(Cell::new(false));
        let resource = Rc::new(());
        let captured = resource.clone();
        let ran_work = ran.clone();
        let created_work = created.clone();
        let bridge = Rc::new(Bridge { scope: js_sys::global().unchecked_into(), pending: Default::default() });
        let (reply, _response) = oneshot::channel();
        let (cancel, cancelled) = async_channel::unbounded();
        let mut cancel = Some(cancel);
        let state = Rc::new(Request {
            id: bridge.pending.next_id(),
            bridge,
            key: "test-transfer".into(),
            port: RefCell::new(None),
            reply: RefCell::new(Some(reply)),
            cancelled,
            progress: None,
            runner: Runner::Transfer(RefCell::new(Some(Box::new(move || {
                created_work.set(true);
                Box::pin(async move {
                    drop(captured);
                    ran_work.set(true);
                    Ok(Vec::new())
                })
            })))),
        });
        if cancelled_before_dispatch {
            if coordinator_reply {
                state.finish(Err("cancelled".into()));
            } else {
                cancel.take();
            }
        }
        state.execute(None);
        if !cancelled_before_dispatch {
            if coordinator_reply {
                state.finish(Err("cancelled".into()));
            } else {
                cancel.take();
            }
        }
        drop(state);
        client_platform_runtime::executor::sleep(std::time::Duration::ZERO).await;
        let expected_creation = !cancelled_before_dispatch;
        if ran.get() || created.get() != expected_creation || Rc::strong_count(&resource) != 1 {
            return false;
        }
    }
    true
}
