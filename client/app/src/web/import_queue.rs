//! Worker connections and replies for the library-owned import queue.
use super::{field, object, Port};
use crate::library::web_import::queue::ImportJobs;
use js_sys::Array;
#[cfg(feature = "web-runtime-tests")]
use js_sys::{Object, Reflect};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::MessagePort;

pub struct ImportQueue {
    jobs: RefCell<ImportJobs<MessagePort>>,
    port: RefCell<Option<Port>>,
    broadcast: Box<dyn Fn(JsValue)>,
    failed: Box<dyn Fn(String)>,
}
impl ImportQueue {
    pub fn new(broadcast: impl Fn(JsValue) + 'static, failed: impl Fn(String) + 'static) -> Rc<Self> {
        Rc::new(Self { jobs: RefCell::new(ImportJobs::default()), port: RefCell::new(None), broadcast: Box::new(broadcast), failed: Box::new(failed) })
    }
    fn send(&self, message: &JsValue, transfer: &Array) -> bool {
        let port = self.port.borrow().as_ref().map(|port| port.raw.clone());
        let Some(port) = port else { return false };
        match port.post_message_with_transferable(message, transfer) {
            Ok(()) => true,
            Err(error) => {
                (self.failed)(super::error_message(&error));
                false
            }
        }
    }
    pub fn stop(&self) {
        self.port.borrow_mut().take();
    }
    pub fn submit(&self, job: JsValue, reply: JsValue) {
        let status = self.jobs.borrow_mut().submit(job.clone(), reply.dyn_into().ok());
        let Some(status) = status else { return };
        (self.broadcast)(status);
        self.send(&object(&[("transport", "import_submit".into()), ("job", job)]), &Array::new());
    }
    pub fn snapshot(&self, port: &MessagePort) {
        for status in self.jobs.borrow().statuses() {
            let _ = port.post_message(&status);
        }
    }
    pub fn retry(&self, id: &JsValue) {
        let Some(id) = id.as_string() else { return };
        let next = self.jobs.borrow_mut().retry(&id);
        let Some(next) = next else { return };
        (self.broadcast)(next.0);
        self.send(&object(&[("transport", "import_submit".into()), ("job", next.1)]), &Array::new());
    }
    pub fn start(self: &Rc<Self>, raw: MessagePort, database: &MessagePort) {
        self.stop();
        let weak = Rc::downgrade(self);
        let weak_error = weak.clone();
        *self.port.borrow_mut() = Some(Port::new(
            raw.clone(),
            move |event| {
                if let Some(queue) = weak.upgrade() {
                    queue.receive(event.data());
                }
            },
            move |_| {
                if let Some(queue) = weak_error.upgrade() {
                    (queue.failed)("Import worker connection failed".into());
                }
            },
        ));
        if !self.send(&object(&[("transport", "initialize_import".into()), ("database", database.clone().into())]), &Array::of1(database)) {
            return;
        }
        let jobs = self.jobs.borrow().pending();
        for job in jobs {
            if !self.send(&object(&[("transport", "import_submit".into()), ("job", job)]), &Array::new()) {
                break;
            }
        }
    }
    fn receive(&self, data: JsValue) {
        match field(&data, "transport").as_string().as_deref() {
            Some("endpoint_failed") => (self.failed)(field(&data, "error").as_string().unwrap_or_else(|| "Import worker failed".into())),
            Some("import_recovered") => self.submit(field(&data, "job"), JsValue::NULL),
            Some("import_progress") => {
                let status = self.jobs.borrow_mut().progress(&data);
                if let Some(status) = status {
                    (self.broadcast)(status);
                }
            }
            Some("import_result") => {
                let result = self.jobs.borrow_mut().finish(&data);
                if let Some((status, reply)) = result {
                    if let Some(reply) = reply {
                        let _ = reply.post_message(&data);
                        reply.close();
                    }
                    (self.broadcast)(status);
                }
            }
            _ => {}
        }
    }
}

#[cfg(feature = "web-runtime-tests")]
pub fn contract() -> Result<(), JsValue> {
    use web_sys::MessageChannel;
    let queue = ImportQueue::new(|_| {}, |error| panic!("{error}"));
    let source = Object::new();
    let files = Array::new();
    files.push(&object(&[("file", source.clone().into())]));
    let job = object(&[("id", "job".into()), ("name", "Books".into()), ("files", files.into())]);
    let reply = MessageChannel::new()?;
    queue.submit(job.clone(), reply.port1().into());
    let first = MessageChannel::new()?;
    queue.start(first.port1(), &MessageChannel::new()?.port2());
    let stale = first.port1().onmessage().unwrap();
    queue.receive(object(&[("transport", "import_progress".into()), ("id", "job".into()), ("phase", "copying".into())]));
    queue.stop();
    let second = MessageChannel::new()?;
    queue.start(second.port1(), &MessageChannel::new()?.port2());
    let options = web_sys::MessageEventInit::new();
    options.set_data(&object(&[("transport", "import_result".into()), ("id", "job".into()), ("error", "stale worker".into())]));
    assert!(first.port1().onmessage().is_none());
    assert!(first.port1().onmessageerror().is_none());
    assert!(stale.call1(&JsValue::NULL, &web_sys::MessageEvent::new_with_event_init_dict("message", &options)?.into()).is_err(), "closed port callbacks must be released");
    assert_eq!(field(&queue.jobs.borrow().status("job"), "phase").as_string().as_deref(), Some("copying"));
    queue.receive(object(&[("transport", "import_recovered".into()), ("job", job.clone())]));
    assert_eq!(queue.jobs.borrow().statuses().len(), 1);
    queue.receive(object(&[("transport", "import_result".into()), ("id", "job".into()), ("error", "quota".into())]));
    assert_eq!(Array::from(&field(&job, "files")).length(), 1);
    queue.retry(&"job".into());
    assert_eq!(field(&queue.jobs.borrow().status("job"), "phase").as_string().as_deref(), Some("queued"));
    assert_eq!(field(&Array::from(&field(&job, "files")).get(0), "file"), JsValue::from(source));
    queue.receive(object(&[("transport", "import_result".into()), ("id", "job".into()), ("failures", Array::new().into())]));
    assert_eq!(Array::from(&field(&job, "files")).length(), 0);
    assert!(!queue.jobs.borrow().has_reply("job"));
    queue.retry(&"job".into());
    assert_eq!(field(&queue.jobs.borrow().status("job"), "phase").as_string().as_deref(), Some("complete"));
    queue.stop();
    first.port2().close();
    second.port2().close();
    reply.port2().close();
    for failed_transport in ["initialize_import", "import_submit"] {
        let owner = Rc::new(RefCell::new(std::rc::Weak::<ImportQueue>::new()));
        let stopped = owner.clone();
        let failures = Rc::new(std::cell::Cell::new(0));
        let observed = failures.clone();
        let queue = ImportQueue::new(
            |_| {},
            move |error| {
                assert!(error.contains("send failed"));
                observed.set(observed.get() + 1);
                let queue = stopped.borrow().upgrade().unwrap();
                queue.stop();
                queue.submit(object(&[("id", "reentrant".into()), ("files", Array::new().into())]), JsValue::NULL);
            },
        );
        *owner.borrow_mut() = Rc::downgrade(&queue);
        for id in ["first", "second"] {
            queue.submit(object(&[("id", id.into()), ("files", Array::new().into())]), JsValue::NULL);
        }
        let worker = MessageChannel::new()?;
        let database = MessageChannel::new()?;
        let send = js_sys::Function::new_with_args("message", &format!("if (message.transport === '{failed_transport}') throw Error('send failed');"));
        Reflect::set(&worker.port1(), &"postMessage".into(), &send)?;
        queue.start(worker.port1(), &database.port2());
        assert_eq!(failures.get(), 1);
        assert!(queue.port.borrow().is_none());
        assert_eq!(queue.jobs.borrow().statuses().len(), 3, "recovery can update jobs after a failed send");
        worker.port2().close();
        database.port1().close();
        database.port2().close();
    }
    Ok(())
}
