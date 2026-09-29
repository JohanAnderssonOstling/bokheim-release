//! Browser CPU worker execution for opaque application requests.

/// Admission limits and startup deadline for a browser CPU worker.
#[derive(Clone, Copy, Debug)]
pub struct CpuWorkerOptions {
    pub jobs: usize,
    pub bytes: usize,
    pub startup_timeout: std::time::Duration,
}
impl Default for CpuWorkerOptions {
    fn default() -> Self {
        Self { jobs: 32, bytes: 64 * 1024 * 1024, startup_timeout: std::time::Duration::from_secs(5) }
    }
}

use std::cell::RefCell;
use std::rc::Rc;

use futures_channel::oneshot;
use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;
#[cfg(feature = "web-runtime-tests")]
use web_sys::Worker;

use super::{error_message, BytesFuture};
use crate::transport::endpoint::{OwnedEndpoint, WorkerEndpoint};
use crate::transport::protocol::{CpuEvent, CpuRequest};
use client_platform_runtime::coordinator::CpuJobs;

type Reply = oneshot::Sender<Result<Vec<u8>, String>>;
struct Job {
    _bytes: client_platform_runtime::byte_budget::Reservation,
    source: crate::transport::request::Source,
    bytes: Vec<u8>,
    reply: Reply,
}
type Connection = client_platform_runtime::worker_lifecycle::Connection<OwnedEndpoint, oneshot::Sender<()>>;
struct State {
    policy: CpuJobs<Job>,
    next: u32,
    worker: Option<Connection>,
    next_generation: u64,
}
struct Service {
    budget: client_platform_runtime::byte_budget::ByteBudget,
    startup_timeout: std::time::Duration,
    create_worker: Box<dyn Fn() -> Result<WorkerEndpoint, JsValue>>,
    state: RefCell<State>,
}

impl Service {
    fn fail(&self, error: JsValue) {
        let (worker, jobs) = {
            let mut state = self.state.borrow_mut();
            (state.worker.take(), state.policy.fail())
        };
        drop(worker);
        for job in jobs {
            let _ = job.reply.send(Err(error_message(&error)));
        }
    }

    fn start(self: &Rc<Self>) -> Result<(), JsValue> {
        let worker = (self.create_worker)()?;
        let generation = {
            let mut state = self.state.borrow_mut();
            state.next_generation = state.next_generation.checked_add(1).expect("CPU worker generations exhausted");
            state.next_generation
        };
        let service = Rc::downgrade(self);
        let message = move |data: JsValue| {
            let Some(service) = service.upgrade() else {
                return;
            };
            if service.state.borrow().worker.as_ref().is_some_and(|worker| worker.accepts(generation)) {
                service.receive(generation, data);
            }
        };
        let service = Rc::downgrade(self);
        let failed = move |message: String| {
            if let Some(service) = service.upgrade() {
                if service.state.borrow().worker.as_ref().is_some_and(|worker| worker.accepts(generation)) {
                    service.fail(js_sys::Error::new(&message).into());
                }
            }
        };
        let endpoint = OwnedEndpoint::listen(worker, message, failed, "CPU worker stopped", "Invalid CPU worker message");
        let service = Rc::downgrade(self);
        let startup_timeout = self.startup_timeout;
        let (cancel, cancelled) = oneshot::channel();
        self.state.borrow_mut().worker = Some(Connection::new(endpoint, generation, cancel));
        wasm_bindgen_futures::spawn_local(async move {
            if client_platform_runtime::pending::until_cancelled(client_platform_runtime::executor::sleep(startup_timeout), cancelled).await.is_none() {
                return;
            }
            if let Some(service) = service.upgrade() {
                if service.state.borrow().worker.as_ref().is_some_and(|worker| worker.waiting(generation)) {
                    service.fail("CPU worker startup timed out".into());
                }
            }
        });
        Ok(())
    }

    fn receive(&self, generation: u64, message: JsValue) {
        let event = match CpuEvent::decode(&message) {
            Ok(event) => event,
            Err(error) => {
                self.fail(error.into());
                return;
            }
        };
        match event {
            CpuEvent::Failed(error) => {
                self.fail(error.into());
                return;
            }
            CpuEvent::Ready => {
                let mut state = self.state.borrow_mut();
                if let Some(worker) = state.worker.as_mut() {
                    worker.ready(generation);
                }
                state.policy.ready();
            }
            CpuEvent::Reply { id, result } => {
                let completed = self.state.borrow_mut().policy.complete(id);
                match completed {
                    Ok(job) => {
                        let _ = job.reply.send(result);
                    }
                    Err(error) => {
                        self.fail(error.into());
                        return;
                    }
                }
            }
        }
        self.pump();
    }

    fn pump(&self) {
        let work = {
            let mut state = self.state.borrow_mut();
            let next = state.policy.next().map(|(id, job)| (id, std::mem::take(&mut job.bytes), std::mem::take(&mut job.source)));
            next.map(|(id, bytes, source)| (id, bytes, source, state.worker.as_ref().expect("ready worker").endpoint.endpoint().clone()))
        };
        if let Some((id, bytes, mut source, worker)) = work {
            let (message, transfer) = CpuRequest { id, bytes, source: source.value() }.encode();
            if let Err(error) = worker.post_message_with_transfer(&message, &transfer) {
                self.fail(error);
            } else {
                source.transferred();
            }
        }
    }
}

/// CPU scheduling, pending replies and worker callbacks owned by Rust.
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
pub struct CpuService(Rc<Service>);
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
impl CpuService {
    #[cfg(feature = "web-runtime-tests")]
    pub fn request(&self, bytes: Uint8Array, source: JsValue) -> js_sys::Promise {
        let response = self.request_source(bytes.to_vec(), source);
        wasm_bindgen_futures::future_to_promise(async move { response.await.map(|bytes| Uint8Array::from(bytes.as_slice()).into()).map_err(|error| js_sys::Error::new(&error).into()) })
    }

    pub fn fail(&self, error: JsValue) {
        self.0.fail(error);
    }
}

impl CpuService {
    pub fn request_source(&self, bytes: Vec<u8>, source: JsValue) -> BytesFuture {
        let source = crate::transport::request::Source::new(source);
        let Some(reservation) = self.0.budget.reserve(bytes.len()) else {
            let error = if bytes.len() > self.0.budget.limit() { "CPU request exceeds configured input byte limit" } else { "CPU input queue byte limit exceeded; retry after queued work completes" };
            return Box::pin(async move { Err(error.into()) });
        };
        let (sender, receiver) = oneshot::channel();
        let (id, start) = {
            let mut state = self.0.state.borrow_mut();
            let id = state.next;
            let Some(next) = state.next.checked_add(1) else {
                return Box::pin(async { Err("CPU request IDs exhausted".into()) });
            };
            state.next = next;
            match state.policy.request(id, Job { _bytes: reservation, bytes, source, reply: sender }) {
                Ok(()) => (id, state.worker.is_none()),
                Err((error, _)) => return Box::pin(async move { Err(error) }),
            }
        };
        if start {
            if let Err(error) = self.0.start() {
                self.0.fail(error);
            }
        }
        self.0.pump();
        let service = Rc::downgrade(&self.0);
        let cleanup = client_platform_runtime::pending::OnDrop::new(move || {
            if let Some(service) = service.upgrade() {
                let cancelled = service.state.borrow_mut().policy.cancel_pending(id);
                drop(cancelled);
            }
        });
        Box::pin(async move {
            let _cleanup = cleanup;
            receiver.await.unwrap_or_else(|_| Err("CPU service closed".into()))
        })
    }

    #[cfg(feature = "web-runtime-tests")]
    pub fn new(create_worker: impl Fn() -> Result<Worker, JsValue> + 'static, limit: usize) -> Self {
        Self::with_options(move || create_worker().map(Into::into), CpuWorkerOptions { jobs: limit, ..Default::default() })
    }

    pub fn with_options(create_worker: impl Fn() -> Result<WorkerEndpoint, JsValue> + 'static, options: CpuWorkerOptions) -> Self {
        Self(Rc::new(Service {
            budget: client_platform_runtime::byte_budget::ByteBudget::new(options.bytes),
            startup_timeout: options.startup_timeout,
            create_worker: Box::new(create_worker),
            state: RefCell::new(State { policy: CpuJobs::new(options.jobs), next: 1, worker: None, next_generation: 0 }),
        }))
    }
}
