#![cfg(target_family = "wasm")]

use app::{
    AppDataLocation, AppStartupState, host::AppWorkerClientMessage, host::AppWorkerStartupMessage, host::BackendWorkerHost, host::WorkerDispatchResult, host::WorkerSubscription, host::decode_worker_message, host::encode_worker_message,
};
use futures_channel::oneshot;
use futures_util::future::{Either, select};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{DedicatedWorkerGlobalScope, MessagePort};

mod transport;

thread_local! {
    static STARTUP_PERMIT: RefCell<Option<client_platform_runtime::executor::StartupPermit>> = const { RefCell::new(None) };
    static BACKEND: RefCell<Option<BackendWorkerHost>> = const { RefCell::new(None) };
    static STARTUP: RefCell<StartupState> = const { RefCell::new(StartupState::Initializing) };
    static PENDING_CONNECTIONS: RefCell<Vec<MessagePort>> = const { RefCell::new(Vec::new()) };
}

enum StartupState {
    Initializing,
    Ready,
    Failed(app::BackendError),
}

fn worker_scope() -> DedicatedWorkerGlobalScope {
    js_sys::global().unchecked_into()
}

#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    STARTUP_PERMIT.with(|permit| *permit.borrow_mut() = Some(app::host::defer_background_work()));
    app::host::web::network_client::install()?;
    transport::install()?;
    wasm_bindgen_futures::spawn_local(async {
        finish_initialize(initialize().await);
    });
    Ok(())
}

async fn initialize() -> Result<(BackendWorkerHost, AppStartupState), app::BackendError> {
    let backend = BackendWorkerHost::initialize(AppDataLocation::PlatformLocal("bokheim".to_owned())).await?;
    let startup = backend.startup_state()?;
    Ok((backend, startup))
}

fn finish_initialize(result: Result<(BackendWorkerHost, AppStartupState), app::BackendError>) {
    match result {
        Ok((backend, _)) => {
            BACKEND.with(|slot| *slot.borrow_mut() = Some(backend));
            STARTUP.with(|state| *state.borrow_mut() = StartupState::Ready);
            connect_pending_ports();
            post_control("backend_ready", None);
            STARTUP_PERMIT.with(|slot| { if let Some(permit) = slot.borrow_mut().take() { permit.release(); } });
        }
        Err(error) => {
            STARTUP.with(|state| *state.borrow_mut() = StartupState::Failed(error.clone()));
            connect_pending_ports();
            post_control("backend_failed", Some(&error.to_string()));
            worker_scope().close();
        }
    }
}

fn connect_pending_ports() {
    let ports = PENDING_CONNECTIONS.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
    for port in ports {
        connect(port);
    }
}

fn post_control(transport: &str, error: Option<&str>) {
    let message = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&message, &JsValue::from_str("transport"), &JsValue::from_str(transport));
    if let Some(error) = error {
        let _ = js_sys::Reflect::set(&message, &JsValue::from_str("error"), &JsValue::from_str(error));
    }
    let _ = worker_scope().post_message(&message);
}

#[wasm_bindgen::prelude::wasm_bindgen]
pub fn connect(port: MessagePort) {
    let initializing = STARTUP.with(|state| matches!(*state.borrow(), StartupState::Initializing));
    if initializing {
        PENDING_CONNECTIONS.with(|pending| pending.borrow_mut().push(port));
        return;
    }

    install_request_handler(port.clone());
    port.start();
    let startup = STARTUP.with(|state| match &*state.borrow() {
        StartupState::Initializing => unreachable!("initializing connections are queued"),
        StartupState::Ready => BACKEND.with(|slot| {
            slot.borrow()
                .as_ref()
                .ok_or_else(|| app::BackendError::message("backend is not initialized"))
                .and_then(BackendWorkerHost::startup_state)
                .map(|startup| AppWorkerStartupMessage::Ready { startup })
                .unwrap_or_else(|error| AppWorkerStartupMessage::Failed { error })
        }),
        StartupState::Failed(error) => AppWorkerStartupMessage::Failed { error: error.clone() },
    });
    let failed = matches!(startup, AppWorkerStartupMessage::Failed { .. });
    post_message(&port, &startup);
    if failed {
        port.close();
    }
}

fn install_request_handler(port: MessagePort) -> oneshot::Receiver<()> {
    let (disconnect, disconnected) = oneshot::channel();
    let mut disconnect = Some(disconnect);
    let alive = Rc::new(Cell::new(true));
    let cancellations = Rc::new(RefCell::new(HashMap::<u64, oneshot::Sender<()>>::new()));
    let registration_port = port.clone();
    let handler_port = port.clone();
    let response_port = port;
    let on_message = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
        if event.data().as_string().as_deref() == Some("disconnect") {
            alive.set(false);
            if let Some(disconnect) = disconnect.take() {
                let _ = disconnect.send(());
            }
            for (_, cancellation) in cancellations.borrow_mut().drain() {
                let _ = cancellation.send(());
            }
            handler_port.set_onmessage(None);
            handler_port.close();
            return;
        }
        let request = match decode_worker_message::<AppWorkerClientMessage>(&js_sys::Uint8Array::new(&event.data()).to_vec()) {
            Ok(AppWorkerClientMessage::Request(request)) => Ok(request),
            Ok(AppWorkerClientMessage::CancelSubscription { id }) => {
                if let Some(cancellation) = cancellations.borrow_mut().remove(&id) {
                    let _ = cancellation.send(());
                }
                return;
            }
            Err(error) => Err(error),
        };
        // Playback subscriptions own file generations even during preparation.
        // Register cancellation before awaiting metadata or OPFS so an abandoned
        // open cannot leave a file lease behind.
        let mut playback_cancel = request.as_ref().ok().filter(|request| request.is_source_subscription()).map(|request| {
            let (cancel, cancelled) = oneshot::channel();
            cancellations.borrow_mut().insert(request.id, cancel);
            Box::pin(cancelled)
        });
        let backend = BACKEND.with(|slot| slot.borrow().clone());
        let port = response_port.clone();
        let alive = alive.clone();
        let cancellations = cancellations.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let (id, dispatch) = match (request, backend) {
                (Ok(request), Some(backend)) => {
                    let id = request.id;
                    if request.is_resolve_book() {
                        let _ = post_message(&port, &AppWorkerStartupMessage::Diagnostic { message: format!("resolve_book phase=request_received request_id={id}") });
                    }
                    web_sys::console::info_1(&JsValue::from_str(&format!("backend phase=request_received id={id}")));
                    let dispatch = if let Some(cancelled) = playback_cancel.take() {
                        match select(Box::pin(backend.dispatch(request)), cancelled).await {
                            Either::Left((result, pending)) => {
                                playback_cancel = Some(pending);
                                result
                            }
                            Either::Right(_) => return,
                        }
                    } else {
                        backend.dispatch(request).await
                    };
                    (id, dispatch)
                }
                (Ok(request), None) => (request.id, Err(app::BackendError::message("backend is not initialized"))),
                (Err(error), _) => (0, Err(error)),
            };
            let (result, subscription) = match dispatch {
                Ok(WorkerDispatchResult::Response(value)) => (Ok(value), None),
                Ok(WorkerDispatchResult::Subscription { initial, events }) => (Ok(initial), Some(events)),
                Err(error) => (Err(error), None),
            };
            if !alive.get() {
                return;
            }
            web_sys::console::info_1(&JsValue::from_str(&format!("backend phase=response_post id={id} ok={}", result.is_ok())));
            post_message(&port, &AppWorkerStartupMessage::Response { id, result });
            if let Some(events) = subscription {
                let cancelled = playback_cancel.unwrap_or_else(|| {
                    let (cancel, cancelled) = oneshot::channel();
                    cancellations.borrow_mut().insert(id, cancel);
                    Box::pin(cancelled)
                });
                forward_subscription(events, id, port, alive, cancelled).await;
            }
            cancellations.borrow_mut().remove(&id);
        });
    });
    // Let JS own the callback lifetime. Clearing onmessage on disconnect can
    // then release the captured ports and cancellation map.
    let on_message = on_message.into_js_value();
    registration_port.set_onmessage(Some(on_message.unchecked_ref()));
    disconnected
}

async fn forward_subscription(events: WorkerSubscription, id: u64, port: MessagePort, alive: Rc<Cell<bool>>, mut cancelled: std::pin::Pin<Box<oneshot::Receiver<()>>>) {
    loop {
        match select(Box::pin(events.next_value()), cancelled).await {
            Either::Left((Some(payload), pending_cancel)) => {
                cancelled = pending_cancel;
                if !alive.get() || !post_message(&port, &AppWorkerStartupMessage::Event { id, payload }) {
                    break;
                }
            }
            Either::Left((None, _)) | Either::Right(_) => break,
        }
    }
}

fn post_message(port: &MessagePort, message: &AppWorkerStartupMessage) -> bool {
    let bytes = encode_worker_message(message).expect("backend worker message must serialize");
    let message = js_sys::Uint8Array::from(bytes.as_slice());
    let transferable = js_sys::Array::new();
    transferable.push(&message.buffer());
    port.post_message_with_transferable(message.as_ref(), transferable.as_ref()).is_ok()
}
