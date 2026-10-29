use std::cell::Cell;
use std::rc::Rc;

use js_sys::Array;
use wasm_bindgen::prelude::*;
use web_sys::{MessageEvent, MessagePort};

use super::{ClientRelay, CpuService, SyncService, byte_reply, error_message, field, object};
use client_platform_web::transport::endpoint::WorkerEndpoint;
use client_platform_web::transport::host::WorkerHost;
use lifecycle::{Action, CoordinatorLifecycle};

struct State {
    host: WorkerHost,
    lifecycle: Cell<CoordinatorLifecycle>,
    backend: WorkerEndpoint,
    sync: SyncService,
    cpu: CpuService,
    clients: ClientRelay,
}
impl State {
    fn execute(&self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::FailNetwork { error } => {
                    super::network_client::fail_all(js_sys::Error::new(&error).into());
                }
                Action::FailSync { error } => self.sync.fail(js_sys::Error::new(&error).into()),
                Action::FailCpu { error } => self.cpu.fail(js_sys::Error::new(&error).into()),
                Action::NotifySyncFailure { error } => {
                    let _ = self.backend.post_message(&object(&[("transport", "sync_failed".into()), ("error", error.into())]));
                }
                Action::StopNetwork => {
                    super::network_server::fail_all(js_sys::Error::new("Coordinator stopped").into());
                }
                Action::StopBackend => self.backend.terminate(),
            }
        }
    }
    fn fail(&self, error: String) {
        if !self.lifecycle.get().accepting_work() {
            return;
        }
        let recoverable = !self.lifecycle.get().starting();
        let host = self.host.clone();
        // Host failure stops this coordinator. Defer it until the current
        // callback has returned.
        wasm_bindgen_futures::spawn_local(async move {
            if recoverable {
                log::warn!("Restarting browser database after worker failure: {error}");
                host.restart();
            } else {
                host.fail(&error);
            }
        });
    }

    fn route_network(self: &Rc<Self>, message: JsValue) {
        let Ok(port) = field(&message, "port").dyn_into::<MessagePort>() else {
            return;
        };
        let result = (|| {
            if !self.lifecycle.get().accepting_work() {
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
        if !self.lifecycle.get().accepting_work() {
            if let Ok(port) = field(&message, "port").dyn_into::<MessagePort>() {
                port.close();
            }
            return;
        }
        match field(&message, "transport").as_string().as_deref() {
            Some("endpoint_failed") => self.fail(error_message(&field(&message, "error"))),
            Some("network_request") => self.route_network(message),
            Some("sync_request" | "cpu_request") => {
                use client_platform_web::transport::protocol::WorkerRequest;
                let Ok(port) = field(&message, "port").dyn_into::<MessagePort>() else {
                    return;
                };
                match WorkerRequest::decode(&message) {
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
                let mut lifecycle = self.lifecycle.get();
                lifecycle.ready();
                self.lifecycle.set(lifecycle);
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
    }
}

/// Owns the browser worker topology and executes lifecycle decisions in Rust.
pub struct Coordinator(Rc<State>);
impl Drop for Coordinator {
    fn drop(&mut self) {
        self.0.fail("Coordinator closed".into());
    }
}

impl Coordinator {
    pub(crate) fn suspend(&self) {
        self.0.clients.suspend();
        let mut lifecycle = self.0.lifecycle.get();
        let actions = lifecycle.fail("Database host changed; interrupted writes may have committed".into());
        self.0.lifecycle.set(lifecycle);
        self.0.execute(actions);
    }

    pub(crate) fn with_host(host: WorkerHost, options: super::WorkerOptions) -> Result<Self, JsValue> {
        let backend = host.endpoint("database")?;
        let factory = host.clone();
        let create_cpu = move || factory.endpoint("cpu");
        let clients = host.clients()?;
        let storage = backend.clone();
        let connect_storage = move |port: MessagePort| storage.post_message_with_transfer(&object(&[("transport", "connect".into()), ("port", port.clone().into())]), &Array::of1(&port));
        clients.set_connector(connect_storage);
        let state = Rc::new(State {
            host,
            lifecycle: Cell::new(CoordinatorLifecycle::new()),
            backend,
            sync: SyncService::with_options(options.sync),
            cpu: CpuService::with_options(create_cpu, options.cpu),
            clients,
        });
        let weak = Rc::downgrade(&state);
        let message = Closure::<dyn FnMut(MessageEvent)>::own_aborting(move |event: MessageEvent| {
            if let Some(state) = weak.upgrade() {
                state.receive(event.data());
            }
        })
        .into_js_value();
        state.backend.set_onmessage(Some(message.unchecked_ref()));
        let weak = Rc::downgrade(&state);
        let error = Closure::<dyn FnMut(web_sys::Event)>::own_aborting(move |event: web_sys::Event| {
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

/// Coordinator lifecycle decisions, independent of browser workers and ports.
mod lifecycle {
    #[derive(Debug, PartialEq, Eq)]
    pub enum Action {
        FailNetwork { error: String },
        FailSync { error: String },
        FailCpu { error: String },
        NotifySyncFailure { error: String },
        StopNetwork,
        StopBackend,
    }

    #[derive(Clone, Copy, Default)]
    enum State {
        #[default]
        Starting,
        Ready,
        Failed,
    }

    #[derive(Clone, Copy, Default)]
    pub struct CoordinatorLifecycle {
        state: State,
    }

    impl CoordinatorLifecycle {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn starting(&self) -> bool {
            matches!(self.state, State::Starting)
        }
        pub fn accepting_work(&self) -> bool {
            !matches!(self.state, State::Failed)
        }

        pub fn ready(&mut self) {
            if matches!(self.state, State::Starting) {
                self.state = State::Ready;
            }
        }
        pub fn fail(&mut self, error: String) -> Vec<Action> {
            if matches!(self.state, State::Failed) {
                return vec![];
            }
            self.state = State::Failed;
            let mut result = vec![
                Action::FailNetwork { error: error.clone() },
                Action::FailSync { error: error.clone() },
                Action::NotifySyncFailure { error: error.clone() },
                Action::FailCpu { error: error.clone() },
                Action::StopNetwork,
            ];
            // Release SQLite/OPFS ownership before the host restarts.
            result.push(Action::StopBackend);
            result
        }
    }
}
