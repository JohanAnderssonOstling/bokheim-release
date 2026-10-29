//! Application worker roles over browser transport.

pub use client_platform_web::transport::tab::WebConfiguration;
use client_platform_web::transport::tab::{TabConnection, WorkerSpec};
use client_platform_web::transport::{error_message, field};
use js_sys::{Function, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{MessagePort, SharedWorker};

/// Application deployment identity and worker script URLs, shared by early
/// startup and the UI's fallback connection path.
pub fn backend_configuration(broker_url: &str) -> WebConfiguration {
    let base = broker_url.rsplit_once('/').map(|(base, _)| format!("{base}/")).unwrap_or_default();
    WebConfiguration {
        broker_url: broker_url.to_owned(),
        broker_name: "bokheim-backend-hosted-v3".into(),
        lifetime_lock: format!("bokheim-tab-{}", uuid::Uuid::new_v4()),
        worker_name_prefix: "bokheim-tab-worker".into(),
        workers: [("database", "web_backend_database_worker_loader.js?endpoint"), ("cpu", "cpu_worker_loader.js?endpoint")]
            .into_iter()
            .map(|(role, script)| (role.into(), format!("{base}{script}")))
            .collect(),
    }
}

enum ConnectionOwner {
    Local(TabConnection),
    Prepared { owner: PreparedConnection, port: MessagePort },
}

// The smaller bootstrap WASM owns the existing TabConnection. Retain its JS
// wrapper until the UI session closes, then release its workers and Web Lock.
struct PreparedConnection {
    handle: JsValue,
}
impl Drop for PreparedConnection {
    fn drop(&mut self) {
        if let Ok(free) = field(&self.handle, "free").dyn_into::<Function>() {
            let _ = free.call0(&self.handle);
        }
    }
}

/// Keeps the browser transport alive.
pub struct WebConnection {
    pub(crate) worker: SharedWorker,
    transport: ConnectionOwner,
}
impl WebConnection {
    /// Consume the connection started by the HTML shell, or connect normally
    /// for callers whose shell does not use the early startup module.
    pub async fn connect_prestarted(config: WebConfiguration) -> Result<Self, String> {
        let global = js_sys::global();
        let startup = field(&global, "__BOKHEIM_BACKEND_STARTUP__");
        if field(&startup, "brokerUrl").as_string().as_deref() != Some(config.broker_url.as_str()) {
            return Self::connect(config).await;
        }
        let take: Function = field(&startup, "take").dyn_into().map_err(|_| "Invalid backend startup handle")?;
        let promise: Promise = take.call0(&startup).map_err(|error| error_message(&error))?.dyn_into().map_err(|_| "Invalid backend startup promise")?;
        let _ = Reflect::delete_property(&global, &"__BOKHEIM_BACKEND_STARTUP__".into());
        let handle = JsFuture::from(promise).await.map_err(|error| error_message(&error))?;
        let prepared = PreparedConnection { handle };
        // Keep the owner guarded while validating getters, so malformed or
        // mismatched startup results cannot leave a storage owner alive.
        if field(&prepared.handle, "brokerName").as_string().as_deref() != Some(config.broker_name.as_str()) {
            return Err("Backend startup belongs to a different coordinator".into());
        }
        let worker = field(&prepared.handle, "worker").dyn_into().map_err(|_| "Invalid prepared backend worker")?;
        let port = field(&prepared.handle, "port").dyn_into().map_err(|_| "Invalid prepared backend port")?;
        Ok(Self { worker, transport: ConnectionOwner::Prepared { owner: prepared, port } })
    }

    pub async fn connect(config: WebConfiguration) -> Result<Self, String> {
        let transport = TabConnection::connect(
            &config.broker_url,
            &config.broker_name,
            &config.lifetime_lock,
            move |role, id| {
                let url = config.workers.get(role).ok_or_else(|| js_sys::Error::new("Unknown worker role"))?.clone();
                Ok(WorkerSpec { url, name: format!("{}-{id}", config.worker_name_prefix) })
            },
            |_| {},
            client_platform_web::transport::tab::reload_page,
        )
        .await?;
        Ok(Self { worker: transport.worker().clone(), transport: ConnectionOwner::Local(transport) })
    }
    pub fn worker(&self) -> &SharedWorker {
        &self.worker
    }
    pub fn startup_failure(&self) -> JsValue {
        match &self.transport {
            ConnectionOwner::Local(transport) => transport.failure(),
            ConnectionOwner::Prepared { owner, .. } => field(&owner.handle, "failure"),
        }
    }
    pub fn port(&self) -> MessagePort {
        match &self.transport {
            ConnectionOwner::Local(transport) => transport.port(),
            ConnectionOwner::Prepared { port, .. } => port.clone(),
        }
    }
}
