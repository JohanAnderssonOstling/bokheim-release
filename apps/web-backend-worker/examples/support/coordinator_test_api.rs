#![cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub async fn synchronize(bytes: Vec<u8>, storage: js_sys::Function) -> Result<Vec<u8>, JsValue> {
    app::host::sync_contract::run(&bytes, std::rc::Rc::new(move |bytes| js_bytes(storage.call1(&JsValue::NULL, &js_sys::Uint8Array::from(bytes.as_slice()))))).await.map_err(|error| JsValue::from_str(&error))
}

#[wasm_bindgen(js_name = backgroundTiming)]
pub fn background_timing(now: f64, retry: f64, debounce: f64) -> BackgroundTiming {
    BackgroundTiming(app::host::testing::BackgroundTiming::new(now, retry, debounce))
}

// Only the test adapter needs a JS class; scheduling remains platform-neutral.
#[wasm_bindgen]
pub struct BackgroundTiming(app::host::testing::BackgroundTiming);
#[wasm_bindgen]
impl BackgroundTiming {
    pub fn wake(&mut self, now: f64) {
        self.0.wake(now);
    }
    pub fn refresh(&mut self, now: f64) {
        self.0.refresh(now);
    }
    pub fn deadline(&self) -> f64 {
        self.0.deadline()
    }
    pub fn start(&mut self, now: f64) -> Option<bool> {
        self.0.start(now)
    }
    pub fn finish(&mut self, now: f64, ok: bool) {
        self.0.finish(now, ok);
    }
}

#[wasm_bindgen(js_name = clientRelay)]
pub fn client_relay(connect_storage: js_sys::Function) -> app::host::web::ClientRelay {
    app::host::web::ClientRelay::new(move |port| connect_storage.call1(&JsValue::NULL, &port).map(|_| ()))
}

#[wasm_bindgen(js_name = cpuService)]
pub fn cpu_service(create_worker: js_sys::Function, limit: usize) -> app::host::web::CpuService {
    app::host::web::CpuService::new(move || create_worker.call0(&JsValue::NULL).map(|worker| worker.unchecked_into()), limit)
}

#[wasm_bindgen(js_name = transferService)]
pub fn transfer_service(limit: usize) -> app::host::web::TransferService {
    app::host::web::TransferService::new(limit)
}

#[wasm_bindgen(js_name = syncService)]
pub fn sync_service(run: js_sys::Function, limit: usize) -> app::host::web::SyncService {
    app::host::web::SyncService::with_runner(
        move |bytes, storage| {
            let storage = Closure::<dyn Fn(Vec<u8>) -> js_sys::Promise>::new(move |bytes| bytes_promise(storage(bytes)));
            let response = js_bytes(run.call2(&JsValue::NULL, &js_sys::Uint8Array::from(bytes.as_slice()), storage.as_ref()));
            Box::pin(async move {
                // Keep the test callback alive exactly as long as its sync pass.
                let _storage = storage;
                response.await
            })
        },
        limit,
    )
}

#[wasm_bindgen(js_name = backgroundService)]
pub fn background_service(port: web_sys::MessagePort, retry: u32, debounce: u32) -> app::host::web::BackgroundService {
    app::host::web::BackgroundService::new(port, retry, debounce)
}

#[wasm_bindgen(js_name = startCoordinator)]
pub fn start_coordinator(create_worker: Option<js_sys::Function>, close: Option<js_sys::Function>) -> Result<super::Coordinator, JsValue> {
    super::Coordinator::with_factory(
        move |url, name| {
            let options = web_sys::WorkerOptions::new();
            options.set_type(web_sys::WorkerType::Module);
            options.set_name(name);
            match &create_worker {
                Some(factory) => factory.call2(&JsValue::NULL, &url.into(), &options).map(|worker| worker.unchecked_into()),
                None => web_sys::Worker::new_with_options(url, &options),
            }
        },
        move || {
            if let Some(close) = &close {
                let _ = close.call0(&JsValue::NULL);
            }
        },
    )
}

#[wasm_bindgen(js_name = syncRequest)]
pub fn sync_request(key: String, bytes: js_sys::Uint8Array, storage: js_sys::Function) -> Result<js_sys::Promise, JsValue> {
    app::host::web::bridge::sync_request(key, bytes.to_vec(), std::rc::Rc::new(move |bytes| js_bytes(storage.call1(&JsValue::NULL, &js_sys::Uint8Array::from(bytes.as_slice()))))).map(bytes_promise)
}
#[wasm_bindgen(js_name = transferRequest)]
pub fn transfer_request(key: String, run: js_sys::Function) -> Result<js_sys::Promise, JsValue> {
    app::host::web::bridge::transfer_request(key, move || js_bytes(run.call0(&JsValue::NULL))).map(bytes_promise)
}
#[wasm_bindgen(js_name = backgroundHost)]
pub fn background_host(retry: f64, debounce: f64) -> Result<app::host::web::bridge::BackgroundHost, JsValue> {
    app::host::web::bridge::BackgroundHost::new(retry, debounce)
}

#[wasm_bindgen(js_name = serveNetwork)]
pub fn serve_network(port: web_sys::MessagePort, input: JsValue, fetch: Option<js_sys::Function>) -> Result<(), JsValue> {
    app::host::web::network_server::serve(port, input, fetch)
}
#[wasm_bindgen(js_name = installFetchBridge)]
pub fn install_fetch_bridge() -> Result<(), JsValue> {
    app::host::web::network_client::install()
}

#[wasm_bindgen(js_name = cpuRequest)]
pub fn cpu_request(bytes: Vec<u8>) -> Result<js_sys::Promise, JsValue> {
    let response = app::host::web::cpu_request(bytes).map_err(|error| js_sys::Error::new(&error))?;
    Ok(wasm_bindgen_futures::future_to_promise(async move { response.await.map(|bytes| js_sys::Uint8Array::from(bytes.as_slice()).into()).map_err(|error| js_sys::Error::new(&error).into()) }))
}

fn js_bytes(value: Result<JsValue, JsValue>) -> app::host::web::BytesFuture {
    Box::pin(async move {
        let value = value.map_err(js_error)?;
        let value = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&value)).await.map_err(js_error)?;
        Ok(js_sys::Uint8Array::new(&value).to_vec())
    })
}
fn js_error(error: JsValue) -> String {
    error.as_string().or_else(|| js_sys::Reflect::get(&error, &"message".into()).ok()?.as_string()).unwrap_or_else(|| format!("{error:?}"))
}

fn bytes_promise(response: app::host::web::BytesFuture) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move { response.await.map(|bytes| js_sys::Uint8Array::from(bytes.as_slice()).into()).map_err(|error| js_sys::Error::new(&error).into()) })
}

#[wasm_bindgen(js_name = setNetworkWorker)]
pub fn set_network_worker(worker: web_sys::Worker) {
    app::host::web::network_client::set_route(move |message, transfer| {
        js_sys::Reflect::set(&message, &"transport".into(), &"fetch".into())?;
        worker.post_message_with_transfer(&message, transfer)
    });
}

#[wasm_bindgen]
pub fn import_queue_contract() -> Result<(), JsValue> {
    app::host::web::import_queue_contract()
}

#[wasm_bindgen(js_name = failFetchBridge)]
pub fn fail_fetch_bridge(error: JsValue) {
    app::host::web::network_client::fail_all(error);
}

#[wasm_bindgen]
pub async fn notification_socket_contract(url: String, token: String) -> Result<u32, JsValue> {
    app::host::testing::notification_socket_contract(&url, &token).await.map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen]
pub fn notification_fixture(kind: u32) -> Vec<u8> {
    use sync_common::api::notifications::NotificationMessage;
    let message = match kind {
        0 => NotificationMessage::Hello { heartbeat_seconds: 20, timeout_seconds: 60 },
        1 => NotificationMessage::LibrariesChanged,
        2 => NotificationMessage::LibraryChanged { library_id: sync_common::LibraryId::from_u128(1) },
        _ => NotificationMessage::Reconcile,
    };
    sync_common::transport::encode(&message).unwrap()
}

#[wasm_bindgen(js_name = executorTimerContract)]
pub async fn executor_timer_contract() -> bool {
    app::host::testing::timer_lifecycle_contract().await
}

#[wasm_bindgen(js_name = portLifecycleContract)]
pub async fn port_lifecycle_contract() -> Result<bool, JsValue> {
    app::host::web::port_lifecycle_contract().await
}

#[wasm_bindgen(js_name = cancelledRequestContract)]
pub async fn cancelled_request_contract() -> bool {
    app::host::web::bridge::cancellation_contract().await
}

#[wasm_bindgen(js_name = cancelledSyncContract)]
pub async fn cancelled_sync_contract() -> Result<bool, JsValue> {
    app::host::web::sync_cancellation_contract().await
}

#[wasm_bindgen]
pub fn library_subscription_contract() {
    app::host::web::subscription_contract();
}
