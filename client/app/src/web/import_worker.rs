//! Import worker lifecycle. JavaScript supplies file I/O and parser entry points.
use super::{error_message, field, object, Port};
use crate::runtime::{decode_worker_message, encode_worker_message, AppCommand, AppWorkerClientMessage, AppWorkerRequest, AppWorkerStartupMessage, WorkerCommand, WorkerPayload};
use client_platform_web::transport::protocol::ImportCommand;
use js_sys::{Array, Function, Promise, Reflect, Uint8Array};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::MessagePort;

type Reply = futures_channel::oneshot::Sender<Result<Vec<u8>, crate::BackendError>>;
struct State {
    scope: client_platform_web::transport::scope::Scope,
    database: RefCell<Option<Port>>,
    pending: client_platform_runtime::pending::Pending<Reply>,
    jobs: RefCell<VecDeque<JsValue>>,
    active: RefCell<Option<String>>,
    failed: Cell<bool>,
    open: Function,
}
fn transport_error(message: &str) -> crate::BackendError {
    crate::BackendError::transport(message)
}
impl State {
    fn post(&self, message: &JsValue) {
        let _ = self.scope.post_message(message);
    }
    fn fail(&self, message: &str) {
        self.fail_error(transport_error(message));
    }
    fn fail_error(&self, error: crate::BackendError) {
        let error = error.into_transport();
        if self.failed.replace(true) {
            return;
        }
        self.database.borrow_mut().take();
        self.pending.fail_all(|reply| {
            let _ = reply.send(Err(error.clone()));
        });
        self.post(&object(&[("transport", "endpoint_failed".into()), ("error", error.to_string().into())]));
    }
    fn response(&self, data: JsValue) {
        if !data.is_instance_of::<Uint8Array>() {
            if matches!(field(&data, "kind").as_string().as_deref(), Some("failed" | "reconnecting")) {
                self.fail("Database worker disconnected");
            } else {
                self.fail("Invalid import database response");
            }
            return;
        }
        match decode_worker_message::<AppWorkerStartupMessage>(&Uint8Array::new(&data).to_vec()) {
            Ok(AppWorkerStartupMessage::Response { id, result }) => {
                let reply = self.pending.take(id);
                if let Some(reply) = reply {
                    let result = match result {
                        Ok(WorkerPayload(bytes)) => Ok(bytes),
                        Ok(_) => {
                            self.fail("Unexpected import response");
                            Err(transport_error("Unexpected import response"))
                        }
                        Err(error) => Err(error),
                    };
                    let _ = reply.send(result);
                }
            }
            // The database sends startup state and diagnostics to every session.
            // They do not settle an import request.
            Ok(AppWorkerStartupMessage::Ready { .. } | AppWorkerStartupMessage::Diagnostic { .. }) => {}
            Ok(AppWorkerStartupMessage::Failed { error }) => self.fail_error(error),
            Ok(_) => self.fail("Unexpected import database message"),
            Err(error) => self.fail_error(error),
        }
    }
    fn receive(self: &Rc<Self>, data: JsValue) {
        if self.failed.get() {
            return;
        }
        match ImportCommand::decode(&data) {
            Ok(ImportCommand::Initialize(raw)) => {
                let weak = Rc::downgrade(self);
                let failed = weak.clone();
                *self.database.borrow_mut() = Some(Port::listen(
                    raw,
                    move |data| {
                        if let Some(state) = weak.upgrade() {
                            state.response(data);
                        }
                    },
                    move || {
                        if let Some(state) = failed.upgrade() {
                            state.fail("Invalid database transport message");
                        }
                    },
                ));
            }
            Ok(ImportCommand::Submit { id, job }) => {
                let id = Some(id);
                if *self.active.borrow() == id || self.jobs.borrow().iter().any(|job| field(job, "id").as_string() == id) {
                    return;
                }
                self.jobs.borrow_mut().push_back(job);
            }
            Err(error) => {
                self.fail(&error);
                return;
            }
        }
        self.drain();
    }
    fn drain(self: &Rc<Self>) {
        if self.failed.get() || self.database.borrow().is_none() || self.active.borrow().is_some() {
            return;
        }
        let job = self.jobs.borrow_mut().pop_front();
        let Some(job) = job else {
            return;
        };
        let id = field(&job, "id");
        *self.active.borrow_mut() = id.as_string();
        let state = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let library = field(&job, "library").as_string().and_then(|id| id.parse().ok());
            let reporter = state.clone();
            let progress = Rc::new(move |value: JsValue| {
                Reflect::set(&value, &"transport".into(), &"import_progress".into()).unwrap();
                reporter.post(&value);
                Ok(())
            });
            let result = async {
                let library = library.ok_or_else(|| JsValue::from_str("Invalid import library"))?;
                let io = state.open.call1(&JsValue::UNDEFINED, &job)?;
                let io = JsFuture::from(Promise::resolve(&io)).await?;
                crate::library::web_import::run(job, io.unchecked_into(), &Client { state: state.clone(), library }, progress).await
            }
            .await;
            match result {
                Ok(failures) => state.post(&object(&[("transport", "import_result".into()), ("id", id), ("failures", crate::library::web_import::failures_value(&failures).expect("import failures serialize"))])),
                Err(error) if field(&error, "transport").as_bool() == Some(true) => state.fail(&error_message(&error)),
                Err(error) => state.post(&object(&[("transport", "import_result".into()), ("id", id), ("error", error_message(&error).into()), ("storageFull", field(&error, "storageFull"))])),
            }
            state.active.borrow_mut().take();
            state.drain();
        });
    }
}

pub fn start(open: Function, discover: Function) -> Result<(), JsValue> {
    let scope = client_platform_web::transport::scope::Scope::current();
    let state = Rc::new(State { scope: scope.clone(), database: RefCell::new(None), pending: Default::default(), jobs: RefCell::new(VecDeque::new()), active: RefCell::new(None), failed: Cell::new(false), open });
    let receiver = state.clone();
    scope.receive(move |data| receiver.receive(data));
    wasm_bindgen_futures::spawn_local(async move {
        let result = match discover.call0(&JsValue::UNDEFINED) {
            Ok(value) => JsFuture::from(Promise::resolve(&value)).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(entries) => {
                for entry in Array::from(&entries).iter() {
                    match recover(&Uint8Array::new(&field(&entry, "manifest")).to_vec(), &Uint8Array::new(&field(&entry, "journal")).to_vec()) {
                        Ok(job) if !job.is_undefined() => state.post(&object(&[("transport", "import_recovered".into()), ("job", job)])),
                        Ok(_) => {}
                        Err(error) => web_sys::console::warn_2(&"Could not recover import".into(), &error),
                    }
                }
            }
            Err(error) => state.fail(&error_message(&error)),
        }
    });
    Ok(())
}

/// Commands and their associated replies stay in Rust until the binary port boundary.
pub(crate) struct Client {
    state: Rc<State>,
    library: crate::LibraryId,
}
impl Client {
    pub(crate) async fn request<T: crate::executor::BackendOutput + serde::de::DeserializeOwned>(&self, command: crate::library::LibraryCommand) -> Result<T, crate::BackendError> {
        let state = &self.state;
        if state.failed.get() || state.database.borrow().is_none() {
            return Err(transport_error("Database worker disconnected"));
        }
        let id = state.pending.next_id();
        let bytes = encode_worker_message(&AppWorkerClientMessage::Request(AppWorkerRequest { id, command: WorkerCommand::Library { library_id: self.library, command: command } }))?;
        let bytes = Uint8Array::from(bytes.as_slice());
        let (reply, result) = futures_channel::oneshot::channel();
        let _pending = state.pending.track(id, reply);
        let sent = state.database.borrow().as_ref().unwrap().raw.post_message_with_transferable(&bytes, &Array::of1(&bytes.buffer()));
        if let Err(error) = sent {
            state.fail(&error_message(&error));
        }
        let bytes = result.await.unwrap_or_else(|_| Err(transport_error("Import request disconnected")))?;
        decode_worker_message(&bytes).map_err(|error| {
            let error = error.into_transport();
            state.fail_error(error.clone());
            error
        })
    }
}

#[cfg(feature = "web-runtime-tests")]
pub async fn contract() -> Result<(), JsValue> {
    let requests = client_platform_runtime::pending::Pending::default();
    let first = requests.next_id();
    requests.insert(first, 1);
    requests.fail_all(|value| {
        assert_eq!(value, 1);
        assert!(requests.take(first).is_none(), "callbacks run after removal");
        let second = requests.next_id();
        requests.insert(second, 2);
    });
    assert_eq!(requests.take(requests.last_id()), Some(2), "reentrant registration survives the old disconnect");
    assert!(requests.is_empty());
    use crate::runtime::{encode_reply, encode_worker_message, AppWorkerStartupMessage};
    let library = crate::LibraryId::from_u128(123);
    let (state, remote) = fixture();
    let startup = crate::runtime::AppStartupState { libraries: vec![], browsing_preferences: Default::default(), account_status: Default::default(), reader_preferences: Default::default() };
    for message in [AppWorkerStartupMessage::Ready { startup }, AppWorkerStartupMessage::Diagnostic { message: "database ready".into() }] {
        state.response(Uint8Array::from(encode_worker_message(&message).unwrap().as_slice()).into());
        assert!(!state.failed.get(), "database session startup must preserve the import endpoint");
        assert!(state.pending.is_empty());
    }
    let client = Client { state: state.clone(), library };
    let hash = crate::ContentHash::new(&"a".repeat(64));
    let command = crate::runtime::library_requests::ImportStagedBook { parent_id: crate::ROOT_DIR_ID, file_name: "book.epub".into(), physical: format!("__libraries/{library}/imports/{library}/0"), length: 1, hash };
    let reply = async {
        crate::executor::sleep(std::time::Duration::ZERO).await;
        let bytes = encode_worker_message(&AppWorkerStartupMessage::Response { id: state.pending.last_id() as u64, result: Ok(encode_reply(&hash).unwrap()) }).unwrap();
        remote.post_message(&Uint8Array::from(bytes.as_slice())).unwrap();
    };
    let (result, ()) = futures_util::future::join(client.request::<crate::ContentHash>(command), reply).await;
    assert_eq!(result.map_err(|error| JsValue::from_str(&error.to_string()))?, hash, "the real reply bridge must preserve the hash");
    let reconnect = async {
        crate::executor::sleep(std::time::Duration::ZERO).await;
        state.response(object(&[("kind", "reconnecting".into())]));
    };
    let (result, ()) = futures_util::future::join(client.request::<()>(crate::runtime::library_requests::FinishDirectoryImport { activity: uuid::Uuid::nil() }), reconnect).await;
    assert!(result.unwrap_err().is_transport());
    assert!(state.pending.is_empty());
    assert!(state.database.borrow().is_none());
    remote.close();
    let (state, remote) = fixture();
    let throw = Function::new_no_args("throw new Error('send failed')");
    Reflect::set(&state.database.borrow().as_ref().unwrap().raw, &"postMessage".into(), &throw)?;
    let result = Client { state: state.clone(), library }.request::<()>(crate::runtime::library_requests::FinishDirectoryImport { activity: uuid::Uuid::nil() }).await;
    assert!(result.unwrap_err().is_transport());
    assert!(state.pending.is_empty());
    remote.close();
    let (state, remote) = fixture();
    let client = Client { state: state.clone(), library };
    let wrong_reply = async {
        crate::executor::sleep(std::time::Duration::ZERO).await;
        let bytes = encode_worker_message(&AppWorkerStartupMessage::Response { id: state.pending.last_id(), result: Ok(encode_reply(&hash).unwrap()) }).unwrap();
        remote.post_message(&Uint8Array::from(bytes.as_slice())).unwrap();
    };
    let (result, ()) = futures_util::future::join(client.request::<()>(crate::runtime::library_requests::FinishDirectoryImport { activity: uuid::Uuid::nil() }), wrong_reply).await;
    assert!(result.unwrap_err().is_transport(), "a reply of the wrong type must fail the endpoint");
    assert!(state.failed.get());
    assert!(state.pending.is_empty());
    remote.close();
    Ok(())
}

#[cfg(feature = "web-runtime-tests")]
fn fixture() -> (Rc<State>, MessagePort) {
    let channel = web_sys::MessageChannel::new().unwrap();
    let state = Rc::new(State {
        scope: client_platform_web::transport::scope::Scope::current(),
        database: RefCell::new(None),
        pending: Default::default(),
        jobs: RefCell::new(VecDeque::new()),
        active: RefCell::new(None),
        failed: Cell::new(false),
        open: Function::new_no_args("return Promise.resolve([])"),
    });
    state.receive(object(&[("transport", "initialize_import".into()), ("database", channel.port1().into())]));
    (state, channel.port2())
}

// Test-only database peer: workflow tests still traverse the production binary
// request/reply port while injecting storage outcomes from JavaScript.
#[cfg(feature = "web-runtime-tests")]
pub async fn workflow_contract(job: JsValue, io: JsValue, respond: Function, progress: Function) -> Result<JsValue, JsValue> {
    super::requests::wire_contract();
    use crate::runtime::LibraryCommand;
    let (state, remote) = fixture();
    let failed = state.clone();
    let raw = remote.clone();
    let peer = Port::new(
        remote,
        move |event| {
            let AppWorkerClientMessage::Request(request) = decode_worker_message(&Uint8Array::new(&event.data()).to_vec()).unwrap() else { panic!("expected request") };
            let WorkerCommand::Library { command, .. } = request.command else { panic!("expected library command") };
            let respond = respond.clone();
            let raw = raw.clone();
            let failed = failed.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = async {
                    let value = respond.call1(&JsValue::UNDEFINED, &JsValue::from_str(&serde_json::to_string(&command).unwrap()))?;
                    let value = JsFuture::from(Promise::resolve(&value)).await?;
                    let json = value.as_string().unwrap_or_else(|| "null".into());
                    let payload = match command {
                        LibraryCommand::PrepareStagedDirectoryImport { .. } => encode_worker_message(&serde_json::from_str::<crate::library::PreparedDirectoryImport>(&json).unwrap()),
                        LibraryCommand::ImportStagedBook { .. } => encode_worker_message(&serde_json::from_str::<crate::ContentHash>(&json).unwrap()),
                        _ => encode_worker_message(&()),
                    }
                    .unwrap();
                    Ok(WorkerPayload(payload))
                }
                .await;
                let result = match result {
                    Ok(reply) => Ok(reply),
                    Err(error) if field(&error, "transport").as_bool() == Some(true) => {
                        failed.fail(&error_message(&error));
                        return;
                    }
                    Err(error) => Err(crate::BackendError::message(error_message(&error))),
                };
                let bytes = encode_worker_message(&AppWorkerStartupMessage::Response { id: request.id, result }).unwrap();
                raw.post_message(&Uint8Array::from(bytes.as_slice())).unwrap();
            });
        },
        |_| panic!("invalid test peer message"),
    );
    let client = Client { state, library: crate::LibraryId::from_u128(123) };
    let result = crate::library::web_import::run(job, io.unchecked_into(), &client, Rc::new(move |value| progress.call1(&JsValue::UNDEFINED, &value).map(|_| ()))).await.and_then(|values| crate::library::web_import::failures_value(&values));
    drop(peer);
    result
}

pub fn recover(manifest: &[u8], journal: &[u8]) -> Result<JsValue, JsValue> {
    crate::library::web_import::recover(manifest, journal)
}

impl crate::library::web_import::ImportClient for Client {
    async fn request<T: crate::executor::BackendOutput + serde::de::DeserializeOwned>(&self, command: crate::library::LibraryCommand) -> Result<T, crate::BackendError> {
        self.request(command).await
    }
}
