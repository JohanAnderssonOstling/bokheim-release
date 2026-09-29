//! Shared coordinator host election. A tab owns workers, never shared storage.
use super::endpoint::WorkerEndpoint;
use super::{error_message, object, ClientRelay, Port};
use js_sys::{Array, Function};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use web_sys::{MessageChannel, MessagePort};

use super::lifetime::observe_tab_lock;
struct Tab {
    control: Port,
    data: RefCell<Option<MessagePort>>,
    watch: RefCell<Option<(Function, JsValue)>>,
}
impl Drop for Tab {
    fn drop(&mut self) {
        if let Some((cancel, _)) = self.watch.get_mut().take() {
            let _ = cancel.call0(&JsValue::UNDEFINED);
        }
    }
}
struct State {
    application: Rc<dyn HostedApplication>,
    lock_prefix: String,
    tabs: RefCell<BTreeMap<u32, Rc<Tab>>>,
    owner: Cell<Option<u32>>,
    next_tab: Cell<u32>,
    next_worker: Cell<u32>,
    generation: Cell<u32>,
    fatal: RefCell<Option<String>>,
    endpoints: RefCell<HashMap<u32, WorkerEndpoint>>,
    clients: ClientRelay,
}
impl State {
    fn endpoint(self: &Rc<Self>, role: &str) -> Result<WorkerEndpoint, JsValue> {
        let owner = self.owner.get().ok_or_else(|| js_sys::Error::new("No worker host"))?;
        let tab = self.tabs.borrow().get(&owner).cloned().ok_or_else(|| js_sys::Error::new("Worker host closed"))?;
        let id = self.next_worker.get();
        self.next_worker.set(id + 1);
        let channel = MessageChannel::new()?;
        let weak = Rc::downgrade(self);
        let endpoint = WorkerEndpoint::remote(channel.port1(), move || {
            let Some(state) = weak.upgrade() else { return };
            state.endpoints.borrow_mut().remove(&id);
            if let Some(tab) = state.tabs.borrow().get(&owner) {
                let _ = tab.control.raw.post_message(&object(&[("transport", "stop_worker".into()), ("id", id.into())]));
            };
        });
        let port = channel.port2();
        tab.control.raw.post_message_with_transferable(&object(&[("transport", "create_worker".into()), ("id", id.into()), ("role", role.into()), ("port", port.clone().into())]), &Array::of1(&port))?;
        self.endpoints.borrow_mut().insert(id, endpoint.clone());
        Ok(endpoint)
    }
    fn elect(self: &Rc<Self>) {
        if self.fatal.borrow().is_some() || self.owner.get().is_some() {
            return;
        }
        let owner = self.tabs.borrow().iter().find_map(|(id, tab)| tab.data.borrow().is_some().then_some(*id));
        let Some(owner) = owner else { return };
        self.owner.set(Some(owner));
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let host = WorkerHost { state: Rc::downgrade(self), generation };
        if let Err(error) = self.application.clone().start(host) {
            self.fail(&error_message(&error));
        }
    }
    fn stop(&self) {
        self.generation.set(self.generation.get() + 1);
        self.application.stop();
        let endpoints = std::mem::take(&mut *self.endpoints.borrow_mut());
        for endpoint in endpoints.into_values() {
            endpoint.terminate();
        }
        self.owner.set(None);
    }
    fn fail(&self, error: &str) {
        if self.fatal.borrow().is_some() {
            return;
        }
        *self.fatal.borrow_mut() = Some(error.to_owned());
        self.stop();
        self.clients.fail(error);
        for tab in self.tabs.borrow().values() {
            let _ = tab.control.raw.post_message(&super::failure(error));
        }
    }
    fn remove(self: &Rc<Self>, id: u32) {
        let tab = self.tabs.borrow_mut().remove(&id);
        if let Some(tab) = tab {
            if let Some(port) = tab.data.borrow().as_ref() {
                self.clients.disconnect_port(port);
            }
        }
        if self.owner.get() == Some(id) {
            self.stop();
            self.elect();
        }
    }
    fn receive(self: &Rc<Self>, id: u32, data: JsValue) {
        if let Some(error) = self.fatal.borrow().as_ref() {
            if let Some(tab) = self.tabs.borrow().get(&id) {
                let _ = tab.control.raw.post_message(&super::failure(error));
            }
            return;
        }
        use super::control::HostEvent;
        match HostEvent::decode(&data) {
            Ok(HostEvent::Closed) => self.remove(id),
            Ok(HostEvent::Application) => {
                if !self.application.receive(data) {
                    self.remove(id);
                }
            }
            Ok(HostEvent::WorkerFailed { id: worker, error }) if self.owner.get() == Some(id) => {
                let endpoint = self.endpoints.borrow().get(&worker).cloned();
                if let Some(endpoint) = endpoint {
                    endpoint.report_failure(&error);
                }
            }
            Ok(HostEvent::Register { lock, port }) => {
                if !lock.starts_with(&self.lock_prefix) {
                    port.close();
                    self.remove(id);
                    return;
                }
                let tab = self.tabs.borrow().get(&id).cloned();
                let Some(tab) = tab else { return };
                if tab.data.borrow().is_some() {
                    return;
                }
                let result = (|| {
                    let weak = Rc::downgrade(self);
                    let released = Closure::<dyn FnMut()>::new(move || {
                        if let Some(state) = weak.upgrade() {
                            state.remove(id);
                        }
                    })
                    .into_js_value();
                    let watch = observe_tab_lock(&lock, released.unchecked_ref())?;
                    *tab.watch.borrow_mut() = Some((watch, released));
                    *tab.data.borrow_mut() = Some(port.clone());
                    self.clients.connect(port)?;
                    Ok::<(), JsValue>(())
                })();
                match result {
                    Ok(()) => {
                        self.application.connected(&tab.control.raw);
                        self.elect();
                    }
                    Err(error) => {
                        let _ = tab.control.raw.post_message(&super::failure(&error_message(&error)));
                        self.remove(id);
                    }
                }
            }
            Err(_) => self.remove(id),
            Ok(HostEvent::WorkerFailed { .. }) => {}
        }
    }
}

/// Application services attached to the currently elected browser worker host.
/// Implementations must release their workers and connections in `stop`.
pub trait HostedApplication {
    fn start(self: Rc<Self>, host: WorkerHost) -> Result<(), JsValue>;
    fn stop(&self);
    fn receive(&self, message: JsValue) -> bool;
    fn connected(&self, control: &MessagePort);
}

/// A weak, generation-scoped capability: old worker callbacks cannot affect a new host.
#[derive(Clone)]
pub struct WorkerHost {
    state: std::rc::Weak<State>,
    generation: u32,
}
impl WorkerHost {
    fn state(&self) -> Result<Rc<State>, JsValue> {
        self.state.upgrade().filter(|state| state.generation.get() == self.generation && state.owner.get().is_some()).ok_or_else(|| js_sys::Error::new("Worker host changed").into())
    }
    pub fn endpoint(&self, role: &str) -> Result<WorkerEndpoint, JsValue> {
        self.state()?.endpoint(role)
    }
    pub fn clients(&self) -> Result<ClientRelay, JsValue> {
        Ok(self.state()?.clients.clone())
    }
    pub fn fail(&self, error: &str) {
        if let Ok(state) = self.state() {
            state.fail(error);
        }
    }
    /// Restart the hosted services while retaining tab sessions. Application policy
    /// decides whether the failure is recoverable; stale generations cannot restart.
    pub fn restart(&self) {
        if let Ok(state) = self.state() {
            state.stop();
            state.elect();
        }
    }
    pub fn is_current(&self) -> bool {
        self.state().is_ok()
    }
}

/// Elects a live tab and owns the workers hosted by it.
#[derive(Clone)]
pub struct SharedWorkerHost(Rc<State>);
impl SharedWorkerHost {
    pub fn new(application: Rc<dyn HostedApplication>, lock_prefix: &str) -> Self {
        let clients = ClientRelay::new(|_| Err(js_sys::Error::new("Waiting for a worker host").into()));
        clients.suspend();
        Self(Rc::new(State {
            application,
            lock_prefix: lock_prefix.to_owned(),
            tabs: RefCell::new(BTreeMap::new()),
            owner: Cell::new(None),
            next_tab: Cell::new(1),
            next_worker: Cell::new(1),
            generation: Cell::new(0),
            fatal: RefCell::new(None),
            endpoints: RefCell::new(HashMap::new()),
            clients,
        }))
    }
    /// Weak callbacks avoid retaining the host through application service closures.
    pub fn broadcaster(&self) -> impl Fn(JsValue) + 'static {
        let weak = Rc::downgrade(&self.0);
        move |message| {
            if let Some(state) = weak.upgrade() {
                for tab in state.tabs.borrow().values() {
                    let _ = tab.control.raw.post_message(&message);
                }
            }
        }
    }
    pub fn failure_handler(&self) -> impl Fn(String) + 'static {
        let weak = Rc::downgrade(&self.0);
        move |error| {
            let weak = weak.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Some(state) = weak.upgrade() {
                    state.fail(&error);
                }
            });
        }
    }
    pub fn fail(&self, error: &str) {
        self.0.fail(error);
    }
    pub fn connect(&self, port: MessagePort) {
        if let Some(error) = self.0.fatal.borrow().as_ref() {
            let _ = port.post_message(&super::failure(error));
            port.close();
            return;
        }
        let id = self.0.next_tab.get();
        self.0.next_tab.set(id + 1);
        let weak = Rc::downgrade(&self.0);
        let error = weak.clone();
        let control = Port::new(
            port,
            move |event| {
                if let Some(state) = weak.upgrade() {
                    state.receive(id, event.data());
                }
            },
            move |_| {
                if let Some(state) = error.upgrade() {
                    state.remove(id);
                }
            },
        );
        self.0.tabs.borrow_mut().insert(id, Rc::new(Tab { control, data: RefCell::new(None), watch: RefCell::new(None) }));
    }
}
