use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use futures_channel::oneshot;
use wasm_bindgen::prelude::*;
use web_sys::{MessageEvent, MessagePort};

use super::{object, Port};
use crate::sync::SyncRequest;
use client_platform_web::transport::control::BackgroundEvent;

type Reply = oneshot::Sender<bool>;
struct State {
    port: RefCell<Option<Port>>,
    pending: RefCell<Option<(u32, Reply)>>,
    next: Cell<u32>,
    signals: async_channel::Sender<SyncRequest>,
}
impl State {
    fn stop(&self) {
        let Some(port) = self.port.borrow_mut().take() else { return };
        let _ = self.signals.try_send(SyncRequest::Stop);
        if let Some((_, reply)) = self.pending.borrow_mut().take() {
            let _ = reply.send(false);
        }
        let _ = port.raw.post_message(&object(&[("kind", "stopped".into())]));
    }
    fn receive(&self, data: JsValue) {
        match BackgroundEvent::decode(&data) {
            Ok(BackgroundEvent::Stop) | Err(_) => self.stop(),
            Ok(BackgroundEvent::Wake) => {
                let _ = self.signals.try_send(SyncRequest::Wake(Vec::new()));
            }
            Ok(BackgroundEvent::WakeAfter(delay)) => {
                let _ = self.signals.try_send(SyncRequest::WakeAfter(Duration::from_millis(delay.into())));
            }
            Ok(BackgroundEvent::Refresh) => {
                let _ = self.signals.try_send(SyncRequest::RefreshRemote);
            }
            Ok(BackgroundEvent::Result { id, success }) => {
                if let Some(reply) = client_platform_runtime::pending::take_matching(&self.pending, id) {
                    let _ = reply.send(success);
                }
            }
        }
    }

    async fn request(&self, refresh_remote: bool) -> Result<(), ()> {
        if self.port.borrow().is_none() || self.pending.borrow().is_some() {
            return Err(());
        }
        let id = self.next.get().checked_add(1).ok_or(())?;
        self.next.set(id);
        let (reply, response) = oneshot::channel();
        *self.pending.borrow_mut() = Some((id, reply));
        let result = self.port.borrow().as_ref().ok_or(())?.raw.post_message(&object(&[("kind", "cycle".into()), ("id", id.into()), ("refresh_remote", refresh_remote.into())]));
        if result.is_err() {
            self.pending.borrow_mut().take();
            return Err(());
        }
        if response.await.unwrap_or(false) {
            Ok(())
        } else {
            Err(())
        }
    }
}

/// Shared browser timers request whole cycles from the database worker.
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
pub struct BackgroundService(Rc<State>);
impl Drop for BackgroundService {
    fn drop(&mut self) {
        self.0.stop();
    }
}
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
impl BackgroundService {
    #[cfg_attr(feature = "web-runtime-tests", wasm_bindgen(constructor))]
    pub fn new(port: MessagePort, retry: u32, debounce: u32) -> Self {
        let (signals, requests) = async_channel::unbounded();
        let state = Rc::new_cyclic(|weak: &std::rc::Weak<State>| {
            let message = weak.clone();
            let error = weak.clone();
            let port = Port::new(
                port,
                move |event: MessageEvent| {
                    if let Some(state) = message.upgrade() {
                        state.receive(event.data());
                    }
                },
                move |_| {
                    if let Some(state) = error.upgrade() {
                        state.stop();
                    }
                },
            );
            State { port: RefCell::new(Some(port)), pending: RefCell::new(None), next: Cell::new(0), signals }
        });
        let run = state.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let cycles = run.clone();
            crate::sync::run_timed(
                move |input| {
                    let state = cycles.clone();
                    async move { state.request(input.remote).await }
                },
                requests,
                Duration::from_millis(retry.into()),
                Duration::from_millis(debounce.into()),
            )
            .await;
            run.stop();
        });
        Self(state)
    }
    #[cfg(feature = "web-runtime-tests")]
    pub fn stop(&self) {
        self.0.stop();
    }
    #[cfg_attr(feature = "web-runtime-tests", wasm_bindgen(getter))]
    pub fn stopped(&self) -> bool {
        self.0.port.borrow().is_none()
    }
}
