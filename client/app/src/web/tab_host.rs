//! Application worker roles and typed import state over browser transport.
use super::{error_message, field, object};
pub use client_platform_web::transport::tab::WebConfiguration;
use client_platform_web::transport::tab::{TabConnection, WorkerSpec};
use js_sys::Array;
use std::collections::BTreeMap;
use wasm_bindgen::prelude::*;
use web_sys::MessagePort;

#[wasm_bindgen(module = "/src/web/import_client.js")]
extern "C" {
    #[wasm_bindgen(catch, js_name = submitDirectoryImport)]
    async fn submit_directory_import(control: &MessagePort, manifest: &str, files: &Array) -> Result<JsValue, JsValue>;
}

use crate::ImportProgress;

fn import_progress(data: &JsValue) -> Result<ImportProgress, String> {
    let json = js_sys::JSON::stringify(data).map_err(|error| error_message(&error))?;
    let wire: import_pipeline::import_progress_wire::WireImportProgress = serde_json::from_str(&String::from(json)).map_err(|error| error.to_string())?;
    wire.try_into().map_err(str::to_owned)
}

/// Keeps the browser transport alive and exposes application import status.
pub struct WebConnection {
    pub(crate) worker: web_sys::SharedWorker,
    transport: TabConnection,
    imports: tokio::sync::watch::Sender<BTreeMap<String, ImportProgress>>,
}
impl WebConnection {
    pub async fn connect(config: WebConfiguration) -> Result<Self, String> {
        let imports = tokio::sync::watch::channel(BTreeMap::new()).0;
        let progress = imports.clone();
        let transport = TabConnection::connect(
            &config.broker_url,
            &config.broker_name,
            &config.lifetime_lock,
            move |role, id| {
                let url = config.workers.get(role).ok_or_else(|| js_sys::Error::new("Unknown worker role"))?.clone();
                Ok(WorkerSpec { url, name: format!("{}-{id}", config.worker_name_prefix) })
            },
            move |data| {
                if field(&data, "transport").as_string().as_deref() == Some("import_progress") {
                    match import_progress(&data) {
                        Ok(item) => {
                            progress.send_modify(|imports| {
                                imports.insert(item.id.clone(), item);
                            });
                        }
                        Err(error) => log::warn!("Invalid import progress: {error}"),
                    }
                }
            },
            client_platform_web::transport::tab::reload_page,
        )
        .await?;
        Ok(Self { worker: transport.worker().clone(), transport, imports })
    }
    pub(crate) async fn import_directory(&self, manifest: &str, files: Array) -> Result<Vec<crate::ImportFailure>, String> {
        let result = submit_directory_import(&self.transport.control(), manifest, &files).await.map_err(|error| error_message(&error))?;
        serde_json::from_str(&result.as_string().ok_or("Invalid import result")?).map_err(|error| error.to_string())
    }
    pub(crate) fn import_updates(&self) -> tokio::sync::watch::Receiver<std::collections::BTreeMap<String, ImportProgress>> {
        self.imports.subscribe()
    }
    pub(crate) fn import_failed(&self, id: &str, name: &str, error: &str) {
        log::warn!("Import {id} failed: {error}");
        // Coordinator-owned jobs can resume. Pre-submission failures need new capabilities.
        let state = if self.imports.borrow().contains_key(id) {
            crate::ImportState::Paused { reason: crate::ImportFailure::operation(crate::ImportFailureKind::Interrupted, &[], error) }
        } else {
            crate::ImportState::Rejected { reason: crate::ImportFailure::operation(crate::ImportFailureKind::Submission, &[], error) }
        };
        let progress = ImportProgress { id: id.to_owned(), name: name.to_owned(), state, failures: Vec::new() };
        self.imports.send_modify(|imports| {
            imports.insert(id.to_owned(), progress);
        });
    }
    pub(crate) fn retry_import(&self, id: &str) {
        let _ = self.transport.control().post_message(&object(&[("transport", "import_retry".into()), ("id", id.into())]));
    }
    pub fn port(&self) -> MessagePort {
        self.transport.port()
    }
}

impl WebConnection {
    pub(crate) async fn import_selected_directory(&self, library_id: crate::LibraryId, parent_id: crate::DirId, directory: crate::DirectoryImport, create_root: bool) -> Result<Vec<crate::ImportFailure>, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let name = directory.name.clone();
        let result = async {
            let (manifest, files) = crate::library::web_import::submission(&id, library_id, parent_id, directory, create_root)?;
            self.import_directory(&manifest, files).await
        }
        .await;
        if let Err(error) = &result {
            self.import_failed(&id, &name, error);
        }
        result
    }
}
