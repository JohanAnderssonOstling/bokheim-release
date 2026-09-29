//! A scheduler connection is independent of which browser context owns a worker.
use std::cell::Cell;
use std::rc::Rc;

use js_sys::{Array, Function};
use wasm_bindgen::JsValue;
use web_sys::{MessagePort, Worker};

#[derive(Clone)]
pub enum WorkerEndpoint {
    Local(Worker),
    Remote(Rc<RemoteWorker>),
}

pub struct RemoteWorker {
    port: MessagePort,
    stopped: Cell<bool>,
    stop: Box<dyn Fn()>,
}

impl From<Worker> for WorkerEndpoint {
    fn from(worker: Worker) -> Self {
        Self::Local(worker)
    }
}

impl WorkerEndpoint {
    pub fn remote_port(&self) -> Option<MessagePort> {
        match self {
            Self::Remote(remote) => Some(remote.port.clone()),
            Self::Local(_) => None,
        }
    }
    pub fn report_failure(&self, error: &str) {
        let callback = match self {
            Self::Local(worker) => worker.onmessage(),
            Self::Remote(remote) => remote.port.onmessage(),
        };
        if let Some(callback) = callback {
            let options = web_sys::MessageEventInit::new();
            options.set_data(&super::object(&[("transport", "endpoint_failed".into()), ("error", error.into())]));
            if let Ok(event) = web_sys::MessageEvent::new_with_event_init_dict("message", &options) {
                let _ = callback.call1(&JsValue::UNDEFINED, &event);
            }
        }
    }
    pub fn remote(port: MessagePort, stop: impl Fn() + 'static) -> Self {
        Self::Remote(Rc::new(RemoteWorker { port, stopped: Cell::new(false), stop: Box::new(stop) }))
    }

    pub fn post_message(&self, message: &JsValue) -> Result<(), JsValue> {
        self.post_message_with_transfer(message, &Array::new())
    }

    pub fn post_message_with_transfer(&self, message: &JsValue, transfer: &Array) -> Result<(), JsValue> {
        match self {
            Self::Local(worker) => worker.post_message_with_transfer(message, transfer),
            Self::Remote(remote) if !remote.stopped.get() => remote.port.post_message_with_transferable(message, transfer),
            Self::Remote(_) => Err(js_sys::Error::new("Worker connection closed").into()),
        }
    }

    pub fn set_onmessage(&self, callback: Option<&Function>) {
        match self {
            Self::Local(worker) => worker.set_onmessage(callback),
            Self::Remote(remote) => {
                remote.port.set_onmessage(callback);
                if callback.is_some() {
                    remote.port.start();
                }
            }
        }
    }

    pub fn set_onmessageerror(&self, callback: Option<&Function>) {
        match self {
            Self::Local(worker) => worker.set_onmessageerror(callback),
            Self::Remote(remote) => remote.port.set_onmessageerror(callback),
        }
    }

    pub fn set_onerror(&self, callback: Option<&Function>) {
        // Remote runtime errors arrive as endpoint_failed control messages from
        // the owning tab. MessagePort itself has no worker error event.
        if let Self::Local(worker) = self {
            worker.set_onerror(callback);
        }
    }

    pub fn terminate(&self) {
        match self {
            Self::Local(worker) => worker.terminate(),
            Self::Remote(remote) if !remote.stopped.replace(true) => {
                remote.port.set_onmessage(None);
                remote.port.set_onmessageerror(None);
                remote.port.close();
                (remote.stop)();
            }
            Self::Remote(_) => {}
        }
    }
}

/// Creates a module worker; callers choose its script and role.
pub fn module_worker(url: &str, name: &str) -> Result<Worker, JsValue> {
    let options = web_sys::WorkerOptions::new();
    options.set_type(web_sys::WorkerType::Module);
    options.set_name(name);
    Worker::new_with_options(url, &options)
}

pub fn shared_module_worker(url: &str, name: &str) -> Result<web_sys::SharedWorker, JsValue> {
    let options = web_sys::WorkerOptions::new();
    options.set_type(web_sys::WorkerType::Module);
    options.set_name(name);
    web_sys::SharedWorker::new_with_worker_options(url, &options)
}

/// Owns worker callbacks and disconnects them before terminating the endpoint.
pub struct OwnedEndpoint {
    endpoint: WorkerEndpoint,
    _message: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MessageEvent)>,
    _error: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>,
    _malformed: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MessageEvent)>,
}
impl OwnedEndpoint {
    /// Own event conversion and browser errors while callers choose failure policy.
    pub fn listen(endpoint: WorkerEndpoint, mut message: impl FnMut(JsValue) + 'static, failed: impl Fn(String) + 'static, stopped: &'static str, malformed: &'static str) -> Self {
        let failed = Rc::new(failed);
        let invalid = failed.clone();
        Self::new(
            endpoint,
            move |event| message(event.data()),
            move |event| {
                event.prevent_default();
                let error = super::field(event.as_ref(), "message").as_string().filter(|value| !value.is_empty()).unwrap_or_else(|| stopped.into());
                failed(error);
            },
            move |_| invalid(malformed.into()),
        )
    }

    fn new(endpoint: WorkerEndpoint, message: impl FnMut(web_sys::MessageEvent) + 'static, error: impl FnMut(web_sys::Event) + 'static, malformed: impl FnMut(web_sys::MessageEvent) + 'static) -> Self {
        use wasm_bindgen::{closure::Closure, JsCast};
        let message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(message);
        let error = Closure::<dyn FnMut(web_sys::Event)>::new(error);
        let malformed = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(malformed);
        endpoint.set_onmessage(Some(message.as_ref().unchecked_ref()));
        endpoint.set_onerror(Some(error.as_ref().unchecked_ref()));
        endpoint.set_onmessageerror(Some(malformed.as_ref().unchecked_ref()));
        Self { endpoint, _message: message, _error: error, _malformed: malformed }
    }
    pub fn endpoint(&self) -> &WorkerEndpoint {
        &self.endpoint
    }
}
impl Drop for OwnedEndpoint {
    fn drop(&mut self) {
        self.endpoint.set_onmessage(None);
        self.endpoint.set_onerror(None);
        self.endpoint.set_onmessageerror(None);
        self.endpoint.terminate();
    }
}
