use super::{error_message, failure, forward, object, Port};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use wasm_bindgen::prelude::*;
use web_sys::{MessageChannel, MessagePort};

type Connector = dyn Fn(MessagePort) -> Result<(), JsValue>;
struct Client {
    tab: Port,
    storage: RefCell<Option<Port>>,
}
impl Drop for Client {
    fn drop(&mut self) {
        if let Some(storage) = self.storage.get_mut().as_ref() {
            let _ = storage.raw.post_message(&"disconnect".into());
        }
    }
}
struct Relay {
    connect_storage: RefCell<Box<Connector>>,
    ready: Cell<bool>,
    next: Cell<u32>,
    clients: RefCell<HashMap<u32, Rc<Client>>>,
}
fn disconnect(relay: &Weak<Relay>, id: u32) {
    if let Some(relay) = relay.upgrade() {
        relay.clients.borrow_mut().remove(&id);
    }
}
impl Relay {
    fn attach(self: &Rc<Self>, id: u32, client: &Rc<Client>) -> Result<(), JsValue> {
        if !self.ready.get() {
            return Ok(());
        }
        let result = (|| {
            let channel = MessageChannel::new()?;
            let weak = Rc::downgrade(client);
            let relay = Rc::downgrade(self);
            let weak_error = weak.clone();
            let error_relay = relay.clone();
            let storage = Port::new(
                channel.port1(),
                move |event| {
                    let Some(client) = weak.upgrade() else { return };
                    if forward(&client.tab.raw, &event.data()).is_err() {
                        disconnect(&relay, id);
                    }
                },
                move |_| {
                    if let Some(client) = weak_error.upgrade() {
                        let _ = client.tab.raw.post_message(&failure("Invalid backend transport message"));
                    }
                    disconnect(&error_relay, id);
                },
            );
            *client.storage.borrow_mut() = Some(storage);
            if let Err(error) = (self.connect_storage.borrow())(channel.port2()) {
                channel.port2().close();
                return Err(error);
            }
            Ok(())
        })();
        if let Err(error) = &result {
            let _ = client.tab.raw.post_message(&failure(&error_message(error)));
            self.clients.borrow_mut().remove(&id);
        }
        result
    }
}
/// Tab ports survive database host changes. Clients restore subscriptions when
/// the replacement sends readiness, but never automatically replay writes.
#[derive(Clone)]
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
pub struct ClientRelay(Rc<Relay>);
#[cfg_attr(feature = "web-runtime-tests", wasm_bindgen)]
impl ClientRelay {
    pub fn connect(&self, port: MessagePort) -> Result<(), JsValue> {
        let id = self.0.next.get();
        self.0.next.set(id + 1);
        let client = Rc::new_cyclic(|weak: &Weak<Client>| {
            let weak = weak.clone();
            let relay = Rc::downgrade(&self.0);
            let weak_error = weak.clone();
            let error_relay = relay.clone();
            let tab = Port::new(
                port,
                move |event| {
                    let Some(client) = weak.upgrade() else { return };
                    let data = event.data();
                    let sent = client.storage.borrow().as_ref().map(|storage| forward(&storage.raw, &data));
                    if data.as_string().as_deref() == Some("disconnect") || matches!(sent, Some(Err(_))) {
                        disconnect(&relay, id);
                    }
                },
                move |_| {
                    if let Some(client) = weak_error.upgrade() {
                        let _ = client.tab.raw.post_message(&failure("Invalid backend transport message"));
                    }
                    disconnect(&error_relay, id);
                },
            );
            Client { tab, storage: RefCell::new(None) }
        });
        self.0.clients.borrow_mut().insert(id, client.clone());
        self.0.attach(id, &client)
    }
    pub fn fail(&self, error: &str) {
        let clients = std::mem::take(&mut *self.0.clients.borrow_mut());
        for client in clients.values() {
            let _ = client.tab.raw.post_message(&failure(error));
        }
    }
    #[cfg(feature = "web-runtime-tests")]
    pub fn client_count(&self) -> usize {
        self.0.clients.borrow().len()
    }
}
impl ClientRelay {
    pub fn disconnect_port(&self, port: &MessagePort) {
        let id = self.0.clients.borrow().iter().find_map(|(id, client)| (client.tab.raw == *port).then_some(*id));
        if let Some(id) = id {
            self.0.clients.borrow_mut().remove(&id);
        }
    }
    pub fn new(connect_storage: impl Fn(MessagePort) -> Result<(), JsValue> + 'static) -> Self {
        Self(Rc::new(Relay { connect_storage: RefCell::new(Box::new(connect_storage)), ready: Cell::new(true), next: Cell::new(0), clients: RefCell::new(HashMap::new()) }))
    }
    pub fn suspend(&self) {
        self.0.ready.set(false);
        for client in self.0.clients.borrow().values() {
            if let Some(storage) = client.storage.borrow_mut().take() {
                let _ = storage.raw.post_message(&"disconnect".into());
            }
            let _ = client.tab.raw.post_message(&object(&[("kind", "reconnecting".into())]));
        }
    }
    pub fn set_connector(&self, connect: impl Fn(MessagePort) -> Result<(), JsValue> + 'static) {
        *self.0.connect_storage.borrow_mut() = Box::new(connect);
    }
    pub fn resume(&self) {
        if self.0.ready.replace(true) {
            return;
        }
        let clients = self.0.clients.borrow().iter().map(|(id, client)| (*id, client.clone())).collect::<Vec<_>>();
        for (id, client) in clients {
            let _ = self.0.attach(id, &client);
        }
    }
}
