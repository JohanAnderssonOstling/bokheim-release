use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::Array;
use wasm_bindgen::prelude::*;
#[cfg(feature = "web-runtime-tests")]
use web_sys::Worker;
use web_sys::{MessageEvent, MessagePort};

use super::{BackgroundService, ClientRelay, CpuService, SyncService, TransferService, byte_reply, error_message, failure, field, object};
use app::host::coordinator_lifecycle::{Action, CoordinatorLifecycle};
use client_platform_web::transport::endpoint::WorkerEndpoint;

type WorkerFactory = Rc<dyn Fn(&str, &str) -> Result<WorkerEndpoint, JsValue>>;

struct State {
    failure_handler: RefCell<Option<Rc<dyn Fn(String, bool)>>>,
    ready_handler: RefCell<Option<Rc<dyn Fn()>>>,
    lifecycle: RefCell<CoordinatorLifecycle>,
    backend: WorkerEndpoint,
    close: Box<dyn Fn()>,
    sync: SyncService,
    transfers: TransferService,
    cpu: CpuService,
    clients: ClientRelay,
    background: RefCell<Vec<BackgroundService>>,
    ports: RefCell<HashMap<u32, MessagePort>>,
    next: Cell<u32>,
}
impl State {
    fn execute(&self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Connect { id } => {
                    if let Some(port) = self.ports.borrow_mut().remove(&id) {
                        let _ = self.clients.connect(port);
                    }
                }
                Action::Reject { id, error } => {
                    if let Some(port) = self.ports.borrow_mut().remove(&id) {
                        let _ = port.post_message(&failure(&error));
                        port.close();
                    }
                }
                Action::StopBackground => {
                    self.background.borrow_mut().clear();
                }
                Action::FailClients { error } => self.clients.fail(&error),
                Action::FailNetwork { error } => {
                    super::network_client::fail_all(js_sys::Error::new(&error).into());
                }
                Action::FailSync { error } => self.sync.fail(js_sys::Error::new(&error).into()),
                Action::FailTransfers { error } => self.transfers.fail(js_sys::Error::new(&error).into()),
                Action::FailCpu { error } => self.cpu.fail(js_sys::Error::new(&error).into()),
                Action::NotifySyncFailure { error } => {
                    let _ = self.backend.post_message(&object(&[("transport", "sync_failed".into()), ("error", error.into())]));
                }
                Action::StopNetwork => {
                    super::network_server::fail_all(js_sys::Error::new("Coordinator stopped").into());
                }
                Action::StopBackend => self.backend.terminate(),
                Action::Close => (self.close)(),
            }
        }
    }
    fn fail(&self, error: String) {
        if !self.lifecycle.borrow().accepting_work() {
            return;
        }
        if let Some(handler) = self.failure_handler.borrow().clone() {
            handler(error, !self.lifecycle.borrow().starting());
            return;
        }
        let actions = self.lifecycle.borrow_mut().fail(error);
        self.execute(actions);
    }
    fn route_network(self: &Rc<Self>, message: JsValue) {
        let Ok(port) = field(&message, "port").dyn_into::<MessagePort>() else {
            return;
        };
        let result = (|| {
            if !self.lifecycle.borrow().accepting_work() {
                return Err(js_sys::Error::new("Coordinator stopped").into());
            }
            let request = field(&message, "request");
            super::network_server::serve(port.clone(), request, Some(super::network_client::native_fetch()?))
        })();
        if let Err(error) = result {
            let _ = port.post_message(&object(&[("kind", "error".into()), ("error", error_message(&error).into())]));
            port.close();
        }
    }
    fn receive(self: &Rc<Self>, message: JsValue) {
        if !self.lifecycle.borrow().accepting_work() {
            if let Ok(port) = field(&message, "port").dyn_into::<MessagePort>() {
                port.close();
            }
            return;
        }
        match field(&message, "transport").as_string().as_deref() {
            Some("endpoint_failed") => self.fail(error_message(&field(&message, "error"))),
            Some("background_start") => {
                self.background.borrow_mut().retain(|service| !service.stopped());
                let parsed = (|| {
                    let options = field(&message, "options");
                    let retry = client_platform_web::transport::control::milliseconds(field(&options, "retry"))?;
                    let debounce = client_platform_web::transport::control::milliseconds(field(&options, "debounce"))?;
                    let port = field(&message, "port").dyn_into::<MessagePort>().map_err(|_| "Missing background port".to_owned())?;
                    Ok::<_, String>((port, retry, debounce))
                })();
                match parsed {
                    Ok((port, retry, debounce)) => self.background.borrow_mut().push(BackgroundService::new(port, retry, debounce)),
                    Err(_) => {
                        if let Ok(port) = field(&message, "port").dyn_into::<MessagePort>() {
                            let _ = port.post_message(&object(&[("kind", "stopped".into())]));
                            port.close();
                        }
                    }
                }
            }
            Some("network_request") => self.route_network(message),
            Some("transfer_request" | "sync_request" | "cpu_request") => {
                use client_platform_web::transport::protocol::WorkerRequest;
                let Ok(port) = field(&message, "port").dyn_into::<MessagePort>() else {
                    return;
                };
                match WorkerRequest::decode(&message) {
                    Ok(WorkerRequest::Transfer { key }) => self.transfers.request(key, port),
                    Ok(WorkerRequest::Sync { key, bytes }) => self.sync.request_bytes(key, bytes, port),
                    Ok(WorkerRequest::Cpu { bytes, source }) => {
                        let response = self.cpu.request_source(bytes, source);
                        client_platform_web::transport::request::serve(port, response);
                    }
                    Err(error) => {
                        let (message, _) = byte_reply(&[("kind", "error".into())], Err(error));
                        let _ = port.post_message(&message);
                        port.close();
                    }
                }
            }
            Some("backend_ready") => {
                self.clients.resume();
                if let Some(handler) = self.ready_handler.borrow().clone() {
                    handler();
                }
                let actions = self.lifecycle.borrow_mut().ready();
                self.execute(actions);
            }
            Some("backend_failed") => self.fail(field(&message, "error").as_string().unwrap_or_else(|| "The browser database could not be opened.".into())),
            _ => {}
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.backend.set_onmessage(None);
        self.backend.set_onerror(None);
        self.backend.terminate();
        for (_, port) in self.ports.get_mut().drain() {
            port.close();
        }
    }
}

/// Owns the browser worker topology and executes lifecycle decisions in Rust.
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
pub struct Coordinator(Rc<State>);
impl Drop for Coordinator {
    fn drop(&mut self) {
        self.0.fail("Coordinator closed".into());
    }
}
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
impl Coordinator {
    pub fn connect(&self, port: MessagePort) {
        let id = self.0.next.get() + 1;
        self.0.next.set(id);
        self.0.ports.borrow_mut().insert(id, port);
        let actions = self.0.lifecycle.borrow_mut().connect(id);
        self.0.execute(actions);
    }
}

impl Coordinator {
    pub(crate) fn set_host_handlers(&self, failed: impl Fn(String, bool) + 'static, ready: impl Fn() + 'static) {
        *self.0.failure_handler.borrow_mut() = Some(Rc::new(failed));
        *self.0.ready_handler.borrow_mut() = Some(Rc::new(ready));
    }
    #[cfg(feature = "web-runtime-tests")]
    pub fn with_factory(create_worker: impl Fn(&str, &str) -> Result<Worker, JsValue> + 'static, close: impl Fn() + 'static) -> Result<Self, JsValue> {
        Self::create(move |url, name| create_worker(url, name).map(Into::into), close)
    }
    #[cfg(feature = "web-runtime-tests")]
    pub(crate) fn create(create_worker: impl Fn(&str, &str) -> Result<WorkerEndpoint, JsValue> + 'static, close: impl Fn() + 'static) -> Result<Self, JsValue> {
        let clients = ClientRelay::new(|_| Err(js_sys::Error::new("Backend is starting").into()));
        Self::with_clients(create_worker, close, clients, Default::default())
    }

    pub(crate) fn suspend(&self) {
        self.0.clients.suspend();
        let actions = self.0.lifecycle.borrow_mut().fail("Database host changed; interrupted writes may have committed".into());
        self.0.execute(actions.into_iter().filter(|action| !matches!(action, Action::FailClients { .. } | Action::Close)).collect());
    }

    pub(crate) fn with_clients(create_worker: impl Fn(&str, &str) -> Result<WorkerEndpoint, JsValue> + 'static, close: impl Fn() + 'static, clients: ClientRelay, options: super::WorkerOptions) -> Result<Self, JsValue> {
        let create_worker: WorkerFactory = Rc::new(create_worker);
        let backend = create_worker("./web_backend_database_worker_loader.js", "bokheim-database-binary-v1")?;
        let factory = create_worker.clone();
        let create_cpu = move || factory("./cpu_worker_loader.js", "bokheim-cpu-v1");
        let storage = backend.clone();
        let connect_storage = move |port: MessagePort| storage.post_message_with_transfer(&object(&[("transport", "connect".into()), ("port", port.clone().into())]), &Array::of1(&port));
        clients.set_connector(connect_storage);
        let state = Rc::new(State {
            failure_handler: RefCell::new(None),
            ready_handler: RefCell::new(None),
            lifecycle: RefCell::new(CoordinatorLifecycle::new()),
            backend,
            close: Box::new(close),
            sync: SyncService::with_options(options.sync),
            transfers: TransferService::new(256),
            cpu: CpuService::with_options(create_cpu, options.cpu),
            clients,
            background: RefCell::new(Vec::new()),
            ports: RefCell::new(HashMap::new()),
            next: Cell::new(0),
        });
        let weak = Rc::downgrade(&state);
        let message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if let Some(state) = weak.upgrade() {
                state.receive(event.data());
            }
        })
        .into_js_value();
        state.backend.set_onmessage(Some(message.unchecked_ref()));
        let weak = Rc::downgrade(&state);
        let error = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            event.prevent_default();
            if let Some(state) = weak.upgrade() {
                state.fail(field(event.as_ref(), "message").as_string().unwrap_or_else(|| "The browser database worker stopped unexpectedly.".into()));
            }
        })
        .into_js_value();
        state.backend.set_onerror(Some(error.unchecked_ref()));
        let weak = Rc::downgrade(&state);
        super::network_client::set_route(move |message, _| {
            let state = weak.upgrade().ok_or_else(|| js_sys::Error::new("Coordinator stopped"))?;
            state.route_network(message);
            Ok(())
        });
        // Startup includes transactional schema/taxonomy migrations. Their
        // duration depends on the library size; a wall-clock deadline would
        // repeatedly abort healthy work. Worker errors still call State::fail.
        Ok(Self(state))
    }
}
