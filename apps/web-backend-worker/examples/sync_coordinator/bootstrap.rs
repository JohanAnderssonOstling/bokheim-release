//! The HTML shell can start the same Rust-owned tab transport before the UI
//! WASM arrives. Its client port remains queued until the UI attaches a Session.
use app::host::web::{WebConnection, backend_configuration};
use client_platform_web::transport::{failure, field};
use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, RefUnwindSafe},
    rc::Rc,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;
use web_sys::{Event, MessagePort};

#[wasm_bindgen]
pub struct PreparedBackend {
    connection: WebConnection,
    broker_name: String,
    error: Rc<RefCell<Option<JsValue>>>,
    on_error: Closure<dyn FnMut(Event)>,
}

// The exported getters only clone handles or read a failure snapshot. All
// callbacks that mutate the underlying transport retain abort-on-panic policy.
impl RefUnwindSafe for PreparedBackend {}

impl Drop for PreparedBackend {
    fn drop(&mut self) {
        let _ = self.connection.worker().remove_event_listener_with_callback("error", self.on_error.as_ref().unchecked_ref());
    }
}

#[wasm_bindgen]
impl PreparedBackend {
    #[wasm_bindgen(getter)]
    pub fn worker(&self) -> JsValue {
        self.connection.worker().clone().into()
    }

    #[wasm_bindgen(getter)]
    pub fn port(&self) -> MessagePort {
        self.connection.port()
    }

    #[wasm_bindgen(getter, js_name = brokerName)]
    pub fn broker_name(&self) -> String {
        self.broker_name.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn failure(&self) -> JsValue {
        self.error.borrow().clone().unwrap_or_else(|| self.connection.startup_failure())
    }
}

#[wasm_bindgen(js_name = prepareBackend)]
pub fn prepare_backend(broker_url: String) -> js_sys::Promise {
    // Construction owns its partial connection and drops it on failure; none
    // of that state is exposed to the caller until this promise succeeds.
    future_to_promise(AssertUnwindSafe(async move {
        let config = backend_configuration(&broker_url);
        let broker_name = config.broker_name.clone();
        let connection = WebConnection::connect(config).await.map_err(|error| js_sys::Error::new(&error))?;
        let error = Rc::new(RefCell::new(None));
        let failed = error.clone();
        let on_error = Closure::<dyn FnMut(Event)>::own_aborting(move |event: Event| {
            let message = field(event.as_ref(), "message").as_string().unwrap_or_else(|| "Shared backend worker failed".into());
            *failed.borrow_mut() = Some(failure(&message));
        });
        connection.worker().add_event_listener_with_callback("error", on_error.as_ref().unchecked_ref())?;
        Ok(PreparedBackend { connection, broker_name, error, on_error }.into())
    }))
}
