/// Admission limits for concurrent application sync requests.
#[derive(Clone, Copy, Debug)]
pub struct SyncOptions {
    pub jobs: usize,
    pub bytes: usize,
}
impl Default for SyncOptions {
    fn default() -> Self {
        Self { jobs: 256, bytes: 16 * 1024 * 1024 }
    }
}

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use futures_channel::oneshot;
use js_sys::{Array, Uint8Array};
use wasm_bindgen::prelude::*;
use web_sys::MessagePort;

use super::{byte_reply, error_message, object, BytesFuture, BytesOperation, Port};
use client_platform_runtime::coordinator::SyncJobs;
use client_platform_runtime::pending::{take_matching, OnDrop};
use client_platform_web::transport::protocol::SyncEvent;

type Reply = oneshot::Sender<Result<Vec<u8>, String>>;
type Pending = Rc<RefCell<Option<(u32, Reply)>>>;
struct Request {
    _bytes: client_platform_runtime::byte_budget::Reservation,
    port: MessagePort,
    bytes: Vec<u8>,
}
struct Job {
    port: Port,
    pending: Pending,
    next: Cell<u32>,
    abort: Rc<RefCell<Option<oneshot::Sender<String>>>>,
}
impl Job {
    fn reject_storage(&self, error: &str) {
        if let Some((_, reply)) = self.pending.borrow_mut().take() {
            let _ = reply.send(Err(error.to_owned()));
        }
    }
    fn storage(&self, bytes: Vec<u8>) -> BytesFuture {
        // SyncEngine and CoordinatorStore await each command before issuing another.
        if self.pending.borrow().is_some() {
            return Box::pin(async { Err("Sync storage command already active".into()) });
        }
        let Some(id) = self.next.get().checked_add(1) else {
            return Box::pin(async { Err("Sync request IDs exhausted".into()) });
        };
        self.next.set(id);
        let (reply, response) = oneshot::channel();
        *self.pending.borrow_mut() = Some((id, reply));
        let bytes = Uint8Array::from(bytes.as_slice());
        let transfer = Array::of1(&bytes.buffer());
        let message = object(&[("kind", "storage".into()), ("id", id.into()), ("bytes", bytes.into())]);
        if let Err(error) = self.port.raw.post_message_with_transferable(&message, &transfer) {
            if let Some((_, reply)) = self.pending.borrow_mut().take() {
                let _ = reply.send(Err(error_message(&error)));
            }
        }
        let pending = self.pending.clone();
        let cleanup = OnDrop::new(move || {
            drop(take_matching(&pending, id));
        });
        Box::pin(async move {
            let _cleanup = cleanup;
            response.await.unwrap_or_else(|_| Err("Sync storage connection closed".into()))
        })
    }
}
struct State {
    policy: SyncJobs<Request>,
    jobs: HashMap<String, Rc<Job>>,
}
type Runner = Box<dyn Fn(Vec<u8>, BytesOperation) -> BytesFuture>;
struct Service {
    budget: client_platform_runtime::byte_budget::ByteBudget,
    run: Runner,
    state: RefCell<State>,
}
impl Service {
    fn reply(request: Request, result: &JsValue) {
        let _ = request.port.post_message(result);
        request.port.close();
    }
    fn execute(self: &Rc<Self>, key: String) {
        let (port, bytes) = {
            let mut state = self.state.borrow_mut();
            let Some(request) = state.policy.active_mut(&key) else {
                return;
            };
            (request.port.clone(), std::mem::take(&mut request.bytes))
        };
        let pending: Pending = Rc::new(RefCell::new(None));
        let replies = pending.clone();
        let (abort, aborted) = oneshot::channel();
        let abort = Rc::new(RefCell::new(Some(abort)));
        let cancellation = abort.clone();
        let message = move |data: JsValue| match SyncEvent::decode(&data) {
            Ok(SyncEvent::Cancel) => {
                if let Some(abort) = cancellation.borrow_mut().take() {
                    let _ = abort.send("Sync request cancelled".to_owned());
                }
            }
            Ok(SyncEvent::Reply { id, result }) => {
                let reply = take_matching(&replies, id);
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
            Err(error) => {
                if let Some(abort) = cancellation.borrow_mut().take() {
                    let _ = abort.send(error.clone());
                }
                if let Some((_, reply)) = replies.borrow_mut().take() {
                    let _ = reply.send(Err(error));
                }
            }
        };
        let replies = pending.clone();
        let malformed = move || {
            if let Some((_, reply)) = replies.borrow_mut().take() {
                let _ = reply.send(Err("Invalid sync storage response".into()));
            }
        };
        let job = Rc::new(Job { port: Port::listen(port, message, malformed), pending, next: Cell::new(0), abort });
        self.state.borrow_mut().jobs.insert(key.clone(), job.clone());
        let storage = job.clone();
        let running = (self.run)(bytes, Rc::new(move |bytes| storage.storage(bytes)));
        let service = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = tokio::select! {
                biased;
                error = aborted => Err(error.unwrap_or_else(|_| "Sync pass ended".into())),
                result = running => result,
            };
            let kind = if result.is_ok() { "done" } else { "error" };
            let (result, _) = byte_reply(&[("kind", kind.into())], result);
            service.state.borrow_mut().jobs.remove(&key);
            job.reject_storage("Sync pass ended");
            let completed = service.state.borrow_mut().policy.complete(&key);
            for request in completed {
                Self::reply(request, &result);
            }
            service.execute(key);
        });
    }
}

/// Sync passes receive their storage endpoint directly as a Rust operation.
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
pub struct SyncService(Rc<Service>);
impl SyncService {
    pub fn request_bytes(&self, key: String, bytes: Vec<u8>, port: MessagePort) {
        self.enqueue(key, bytes.len(), || bytes, port);
    }
    fn enqueue(&self, key: String, size: usize, bytes: impl FnOnce() -> Vec<u8>, port: MessagePort) {
        let Some(reservation) = self.0.budget.reserve(size) else {
            let error = if size > self.0.budget.limit() { "Sync request exceeds configured input byte limit" } else { "Sync input queue byte limit exceeded; retry after queued work completes" };
            let _ = port.post_message(&object(&[("kind", "error".into()), ("error", error.into())]));
            port.close();
            return;
        };
        let start = self.0.state.borrow_mut().policy.request(key.clone(), Request { _bytes: reservation, port, bytes: bytes() });
        match start {
            Err((error, request)) => Service::reply(request, &object(&[("kind", "error".into()), ("error", error.into())])),
            Ok(true) => self.0.execute(key),
            Ok(false) => {}
        }
    }
    pub fn new(limit: usize) -> Self {
        Self::with_options(SyncOptions { jobs: limit, ..Default::default() })
    }
    pub fn with_runner(run: impl Fn(Vec<u8>, BytesOperation) -> BytesFuture + 'static, limit: usize) -> Self {
        Self::build(run, SyncOptions { jobs: limit, ..Default::default() })
    }
    pub fn with_options(options: SyncOptions) -> Self {
        Self::build(|bytes, storage| Box::pin(async move { crate::sync::run(&bytes, storage).await }), options)
    }
    fn build(run: impl Fn(Vec<u8>, BytesOperation) -> BytesFuture + 'static, options: SyncOptions) -> Self {
        Self(Rc::new(Service { budget: client_platform_runtime::byte_budget::ByteBudget::new(options.bytes), run: Box::new(run), state: RefCell::new(State { policy: SyncJobs::new(options.jobs), jobs: HashMap::new() }) }))
    }
}
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
impl SyncService {
    pub fn request(&self, key: String, bytes: Uint8Array, port: MessagePort) {
        self.enqueue(key, bytes.length() as usize, || bytes.to_vec(), port);
    }
    pub fn fail(&self, error: JsValue) {
        let error = error_message(&error);
        let (jobs, requests) = {
            let mut state = self.0.state.borrow_mut();
            (state.jobs.values().cloned().collect::<Vec<_>>(), state.policy.fail(error.clone()))
        };
        for job in jobs {
            if let Some(abort) = job.abort.borrow_mut().take() {
                let _ = abort.send(error.clone());
            }
            job.reject_storage(&error);
        }
        let result = object(&[("kind", "error".into()), ("error", error.into())]);
        for request in requests {
            Service::reply(request, &result);
        }
    }
    #[cfg_attr(feature = "web-runtime-tests", wasm_bindgen(getter))]
    #[cfg(feature = "web-runtime-tests")]
    pub fn count(&self) -> usize {
        self.0.state.borrow().policy.count()
    }
}

#[cfg(feature = "web-runtime-tests")]
pub async fn cancellation_contract() -> Result<bool, JsValue> {
    let ran = Rc::new(Cell::new(false));
    let observed = ran.clone();
    let service = SyncService::with_runner(
        move |_, _| {
            let ran = observed.clone();
            Box::pin(async move {
                ran.set(true);
                Ok(Vec::new())
            })
        },
        2,
    );
    let channel = web_sys::MessageChannel::new()?;
    let followup = web_sys::MessageChannel::new()?;
    service.request("library".into(), Uint8Array::new_with_length(0), channel.port1());
    service.request("library".into(), Uint8Array::new_with_length(0), followup.port1());
    let job = Rc::downgrade(service.0.state.borrow().jobs.get("library").unwrap());
    service.fail("coordinator stopped".into());
    crate::executor::sleep(std::time::Duration::ZERO).await;
    channel.port2().close();
    followup.port2().close();
    Ok(!ran.get() && service.count() == 0 && service.0.state.borrow().jobs.is_empty() && job.upgrade().is_none())
}
