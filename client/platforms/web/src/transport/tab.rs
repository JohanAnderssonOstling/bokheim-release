//! Page-owned workers, ports, Web Locks, and page lifecycle callbacks.
//! Worker roles and application messages are supplied by the caller.
/// Host identity and worker scripts supplied by the application entry point.
pub struct WebConfiguration {
    pub broker_url: String,
    pub broker_name: String,
    pub lifetime_lock: String,
    pub worker_name_prefix: String,
    pub workers: std::collections::BTreeMap<String, String>,
}

use super::{error_message, field, object, Port};
use js_sys::{Array, Function, Promise};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{MessageChannel, MessageEvent, MessagePort, Worker};

use super::lifetime::hold_tab_lock;

/// Application-selected worker script and name. The platform owns its lifetime.
pub struct WorkerSpec {
    pub url: String,
    pub name: String,
}
type WorkerResolver = dyn Fn(&str, u32) -> Result<WorkerSpec, JsValue>;

struct LifetimeLock(Function);
impl LifetimeLock {
    fn release(&self) {
        let _ = self.0.call0(&JsValue::UNDEFINED);
    }
}
impl Drop for LifetimeLock {
    fn drop(&mut self) {
        self.release();
    }
}

struct OwnedWorker {
    worker: Worker,
    _error: JsValue,
}
impl Drop for OwnedWorker {
    fn drop(&mut self) {
        self.worker.set_onerror(None);
        self.worker.terminate();
    }
}
struct State {
    control: MessagePort,
    client_port: MessagePort,
    resolve_worker: Box<WorkerResolver>,
    workers: RefCell<HashMap<u32, OwnedWorker>>,
    release: LifetimeLock,
    closed: Cell<bool>,
    application_message: Box<dyn Fn(JsValue)>,
}
impl State {
    fn shutdown(&self) {
        if self.closed.replace(true) {
            return;
        }
        let _ = self.client_port.post_message(&"disconnect".into());
        self.client_port.close();
        self.workers.borrow_mut().clear();
        let _ = self.control.post_message(&object(&[("transport", "host_closed".into())]));
        self.release.release();
        self.control.close();
    }
    fn receive(self: &Rc<Self>, data: JsValue) {
        use super::control::TabEvent;
        let event = match TabEvent::decode(&data) {
            Ok(event) => event,
            Err(_) => {
                self.shutdown();
                return;
            }
        };
        if matches!(event, TabEvent::Failed) {
            let init = web_sys::MessageEventInit::new();
            init.set_data(&data);
            if let Ok(event) = MessageEvent::new_with_event_init_dict("message", &init) {
                let _ = self.client_port.dispatch_event(&event);
            }
            self.shutdown();
            return;
        }
        match event {
            TabEvent::Stop(id) => {
                self.workers.borrow_mut().remove(&id);
            }
            TabEvent::Create { id, role, port } if !self.closed.get() => {
                let result = (|| {
                    if self.workers.borrow().contains_key(&id) {
                        return Err(js_sys::Error::new("Duplicate worker ID").into());
                    }
                    let spec = (self.resolve_worker)(&role, id)?;
                    let worker = super::endpoint::module_worker(&spec.url, &spec.name)?;
                    let control = self.control.clone();
                    let error = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
                        event.prevent_default();
                        let error = field(event.as_ref(), "message").as_string().unwrap_or_else(|| "Dedicated worker failed".into());
                        let _ = control.post_message(&object(&[("transport", "worker_failed".into()), ("id", id.into()), ("error", error.into())]));
                    })
                    .into_js_value();
                    worker.set_onerror(Some(error.unchecked_ref()));
                    let owned = OwnedWorker { worker, _error: error };
                    owned.worker.post_message_with_transfer(&object(&[("transport", "attach_endpoint".into()), ("port", port.clone().into())]), &Array::of1(&port))?;
                    self.workers.borrow_mut().insert(id, owned);
                    Ok::<(), JsValue>(())
                })();
                if let Err(error) = result {
                    let _ = self.control.post_message(&object(&[("transport", "worker_failed".into()), ("id", id.into()), ("error", error_message(&error).into())]));
                }
            }
            _ => (self.application_message)(data),
        }
    }
}

/// Owns the page's workers, lifetime lock and coordinator connection.
/// Keeping this alive is required while using its returned message port.
pub struct TabConnection {
    worker: web_sys::SharedWorker,
    state: Rc<State>,
    _control: Port,
    pagehide: JsValue,
    pageshow: JsValue,
}
impl Drop for TabConnection {
    fn drop(&mut self) {
        self.state.shutdown();
        if let Some(window) = web_sys::window() {
            let _ = window.remove_event_listener_with_callback("pagehide", self.pagehide.unchecked_ref());
            let _ = window.remove_event_listener_with_callback("pageshow", self.pageshow.unchecked_ref());
        }
    }
}
impl TabConnection {
    pub async fn connect(
        broker_url: &str, worker_name: &str, lock_name: &str, resolve_worker: impl Fn(&str, u32) -> Result<WorkerSpec, JsValue> + 'static, application_message: impl Fn(JsValue) + 'static, on_restore: impl Fn() + 'static,
    ) -> Result<Self, String> {
        let worker = super::endpoint::shared_module_worker(broker_url, worker_name).map_err(|error| format!("failed to create shared backend worker: {error:?}"))?;
        let control = worker.port();
        let lock = lock_name;
        let acquired = hold_tab_lock(&lock).map_err(|error| error_message(&error))?;
        // Install the release guard before awaiting acquisition, so dropping
        // the connection future cannot leave a lifetime lock behind.
        let release = LifetimeLock(field(&acquired, "release").dyn_into().map_err(|_| "Invalid tab lifetime lock".to_owned())?);
        let ready: Promise = field(&acquired, "ready").dyn_into().map_err(|_| "Invalid tab lifetime promise".to_owned())?;
        JsFuture::from(ready).await.map_err(|error| error_message(&error))?;
        let channel = MessageChannel::new().map_err(|error| error_message(&error))?;
        let state = Rc::new(State {
            client_port: channel.port1(),
            control: control.clone(),
            resolve_worker: Box::new(resolve_worker),
            workers: RefCell::new(HashMap::new()),
            release,
            closed: Cell::new(false),
            application_message: Box::new(application_message),
        });
        let weak = Rc::downgrade(&state);
        let error = weak.clone();
        let callback = Port::new(
            control.clone(),
            move |event: MessageEvent| {
                if let Some(state) = weak.upgrade() {
                    state.receive(event.data());
                }
            },
            move |_| {
                if let Some(state) = error.upgrade() {
                    state.shutdown();
                }
            },
        );
        let weak = Rc::downgrade(&state);
        let pagehide = Closure::<dyn FnMut()>::new(move || {
            if let Some(state) = weak.upgrade() {
                state.shutdown();
            }
        })
        .into_js_value();
        let pageshow = Closure::<dyn FnMut(web_sys::PageTransitionEvent)>::new(move |event: web_sys::PageTransitionEvent| {
            if event.persisted() {
                on_restore();
            }
        })
        .into_js_value();
        let connection = Self { worker, state, _control: callback, pagehide, pageshow };
        if let Some(window) = web_sys::window() {
            window.add_event_listener_with_callback("pagehide", connection.pagehide.unchecked_ref()).map_err(|error| error_message(&error))?;
            window.add_event_listener_with_callback("pageshow", connection.pageshow.unchecked_ref()).map_err(|error| error_message(&error))?;
        }

        let port = channel.port2();
        control.post_message_with_transferable(&object(&[("transport", "register_host".into()), ("lock", lock.into()), ("port", port.clone().into())]), &Array::of1(&port)).map_err(|error| error_message(&error))?;
        Ok(connection)
    }
    pub fn worker(&self) -> &web_sys::SharedWorker {
        &self.worker
    }
    pub fn control(&self) -> MessagePort {
        self.state.control.clone()
    }
    pub fn port(&self) -> MessagePort {
        self.state.client_port.clone()
    }
}

/// Available as an opt-in page-restore policy for hosts using immutable bundles.
pub fn reload_page() {
    if let Some(window) = web_sys::window() {
        let _ = window.location().reload();
    }
}
