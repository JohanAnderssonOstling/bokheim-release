//! Application services wired to the platform's elected browser host.
use super::{Coordinator, ImportQueue};
use client_platform_web::transport::{
    ClientRelay,
    host::{HostedApplication, SharedWorkerHost, WorkerHost},
};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;
use web_sys::{MessageChannel, MessagePort};

struct Application {
    options: super::WorkerOptions,
    coordinator: RefCell<Option<Coordinator>>,
    imports: RefCell<Option<Rc<ImportQueue>>>,
    import_database: RefCell<Option<(ClientRelay, MessagePort)>>,
}
impl HostedApplication for Application {
    fn start(self: Rc<Self>, host: WorkerHost) -> Result<(), JsValue> {
        let factory = host.clone();
        let coordinator = Coordinator::with_clients(move |url, _| factory.endpoint(if url.contains("database") { "database" } else { "cpu" }), || {}, host.clients()?, self.options)?;
        let failed = host.clone();
        let application = Rc::downgrade(&self);
        coordinator.set_host_handlers(
            move |error, recoverable| {
                let failed = failed.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    if recoverable {
                        log::warn!("Restarting browser database after worker failure: {error}");
                        failed.restart();
                    } else {
                        failed.fail(&error);
                    }
                });
            },
            move || {
                if !host.is_current() {
                    return;
                }
                if let Some(application) = application.upgrade() {
                    if let Err(error) = application.start_imports(&host) {
                        host.fail(&super::error_message(&error));
                    }
                }
            },
        );
        *self.coordinator.borrow_mut() = Some(coordinator);
        Ok(())
    }
    fn stop(&self) {
        if let Some(queue) = self.imports.borrow().as_ref() {
            queue.stop();
        }
        if let Some((clients, port)) = self.import_database.borrow_mut().take() {
            clients.disconnect_port(&port);
        }
        if let Some(coordinator) = self.coordinator.borrow_mut().take() {
            coordinator.suspend();
        }
    }
    fn receive(&self, data: JsValue) -> bool {
        match super::field(&data, "transport").as_string().as_deref() {
            Some("import_submit") => {
                let job = super::field(&data, "job");
                if super::field(&job, "id").as_string().is_none() {
                    return false;
                }
                if let Some(queue) = self.imports.borrow().as_ref() {
                    queue.submit(job, super::field(&data, "reply"));
                }
            }
            Some("import_retry") => {
                let id = super::field(&data, "id");
                if id.as_string().is_none() {
                    return false;
                }
                if let Some(queue) = self.imports.borrow().as_ref() {
                    queue.retry(&id);
                }
            }
            _ => return false,
        }
        true
    }
    fn connected(&self, control: &MessagePort) {
        if let Some(queue) = self.imports.borrow().as_ref() {
            queue.snapshot(control);
        }
    }
}
impl Application {
    fn start_imports(&self, host: &WorkerHost) -> Result<(), JsValue> {
        if self.import_database.borrow().is_some() {
            return Ok(());
        }
        let endpoint = host.endpoint("import")?;
        let channel = MessageChannel::new()?;
        let clients = host.clients()?;
        clients.connect(channel.port1())?;
        *self.import_database.borrow_mut() = Some((clients, channel.port1()));
        if let Some(queue) = self.imports.borrow().as_ref() {
            queue.start(endpoint.remote_port().unwrap(), &channel.port2());
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct SharedCoordinator(SharedWorkerHost);
impl SharedCoordinator {
    pub fn new() -> Self {
        Self::with_options(Default::default())
    }
    pub fn with_options(options: super::WorkerOptions) -> Self {
        let application = Rc::new(Application { options, coordinator: RefCell::new(None), imports: RefCell::new(None), import_database: RefCell::new(None) });
        let host = SharedWorkerHost::new(application.clone(), "bokheim-tab-");
        *application.imports.borrow_mut() = Some(ImportQueue::new(host.broadcaster(), host.failure_handler()));
        Self(host)
    }
    pub fn fail(&self, error: &str) {
        self.0.fail(error);
    }
    pub fn connect(&self, port: MessagePort) {
        self.0.connect(port);
    }
}
