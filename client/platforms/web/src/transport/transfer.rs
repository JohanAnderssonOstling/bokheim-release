//! Browser adapters for shared transfer ownership and reply delivery.
use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use web_sys::MessagePort;

use super::{error_message, object, Port};
use crate::transport::protocol::TransferEvent;
use client_platform_runtime::coordinator::{Transfer, TransferJobs};

struct State {
    policy: TransferJobs<Port>,
    next: u32,
}
struct Service(RefCell<State>);
impl Service {
    fn reply(&self, job: Option<Transfer<Port>>, result: JsValue) {
        let Some(job) = job else {
            return;
        };
        if job.owner_subscribed {
            let _ = job.owner.1.raw.post_message(&result);
        }
        for port in job.subscribers.into_values() {
            let _ = port.raw.post_message(&result);
        }
    }
    fn finish(&self, key: &str, id: u32, connection_failed: bool, result: JsValue) {
        let job = self.0.borrow_mut().policy.take_finished(key, id, connection_failed);
        self.reply(job, result);
    }
    fn progress(&self, key: &str, id: u32, fraction: f64) {
        let message = object(&[("kind", "progress".into()), ("fraction", fraction.into())]);
        self.0.borrow_mut().policy.report_progress(key, id, fraction, |port| {
            let _ = port.raw.post_message(&message);
        });
    }
}

/// Shared transfer ports, completion delivery and callback cleanup in Rust.
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
pub struct TransferService(Rc<Service>);
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
impl TransferService {
    #[cfg_attr(feature = "web-runtime-tests", wasm_bindgen(constructor))]
    pub fn new(limit: usize) -> Self {
        Self(Rc::new(Service(RefCell::new(State { policy: TransferJobs::new(limit), next: 0 }))))
    }

    pub fn request(&self, key: String, port: MessagePort) {
        let id = {
            let mut state = self.0 .0.borrow_mut();
            let Some(next) = state.next.checked_add(1) else {
                let _ = port.post_message(&object(&[("kind", "error".into()), ("error", "Transfer request IDs exhausted".into())]));
                port.close();
                return;
            };
            state.next = next;
            state.next
        };
        let service = Rc::downgrade(&self.0);
        let message_key = key.clone();
        let message = move |data: JsValue| {
            let Some(service) = service.upgrade() else {
                return;
            };
            match TransferEvent::decode(&data) {
                Ok(TransferEvent::Cancel) => {
                    let cancelled = service.0.borrow_mut().policy.cancel(&message_key, id);
                    drop(cancelled);
                }
                Ok(TransferEvent::Complete) => service.finish(&message_key, id, false, data),
                Ok(TransferEvent::Progress(fraction)) => service.progress(&message_key, id, fraction),
                Err(error) => service.finish(&message_key, id, true, object(&[("kind", "error".into()), ("error", error.into())])),
            }
        };
        let service = Rc::downgrade(&self.0);
        let error_key = key.clone();
        let error = move || {
            if let Some(service) = service.upgrade() {
                service.finish(&error_key, id, true, object(&[("kind", "error".into()), ("error", "Transfer executor connection failed".into())]));
            }
        };
        let handle = Port::listen(port.clone(), message, error);
        let requested = self.0 .0.borrow_mut().policy.request(key.clone(), id, handle);
        let owner = match requested {
            Ok(owner) => owner,
            Err((error, port)) => {
                let _ = port.raw.post_message(&object(&[("kind", "error".into()), ("error", error.into())]));
                return;
            }
        };
        if owner {
            if let Err(error) = port.post_message(&object(&[("kind", "run".into())])) {
                self.0.finish(&key, id, true, object(&[("kind", "error".into()), ("error", error_message(&error).into())]));
            }
        } else if let Some(fraction) = self.0 .0.borrow().policy.latest_progress(&key) {
            let _ = port.post_message(&object(&[("kind", "progress".into()), ("fraction", fraction.into())]));
        }
    }

    pub fn fail(&self, error: JsValue) {
        let error = error_message(&error);
        let jobs = self.0 .0.borrow_mut().policy.drain(error.clone());
        let result = object(&[("kind", "error".into()), ("error", error.into())]);
        for job in jobs {
            self.0.reply(Some(job), result.clone());
        }
    }
    #[cfg_attr(feature = "web-runtime-tests", wasm_bindgen(getter))]
    #[cfg(feature = "web-runtime-tests")]
    pub fn count(&self) -> usize {
        self.0 .0.borrow().policy.count()
    }
    #[cfg(feature = "web-runtime-tests")]
    pub fn job_count(&self) -> usize {
        self.0 .0.borrow().policy.job_count()
    }
}
