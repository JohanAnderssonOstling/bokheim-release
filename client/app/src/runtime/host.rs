//! Restricted entry point for a platform that hosts the browser worker.
use super::{AppStartupState, AppWorkerRequest, WorkerDispatchResult};
use crate::app::{AppBackend, AppDataLocation};

#[derive(Clone)]
pub struct BackendWorkerHost(AppBackend);

impl BackendWorkerHost {
    /// Browser startup always uses the deployed bundle and its bundled taxonomy.
    /// Finish migrations before opening sessions; failures propagate to the page.
    pub async fn initialize(location: AppDataLocation) -> Result<Self, crate::BackendError> {
        crate::initialize_web_storage().await?;
        let context = crate::app::BackendContext::initialize(location).map_err(crate::BackendError::message)?;
        let entries = context.registry.entries().map_err(crate::BackendError::operation)?;
        for entry in &entries {
            let locator = context.registry.library_database_locator(entry.library_id(), entry.storage_locator());
            library_backend::updates::initialize_database(&locator)?;
        }
        let backend = AppBackend::new(context)?;
        for entry in entries { backend.library_directory().get(entry.library_id())?; }
        Ok(Self(backend))
    }

    pub fn startup_state(&self) -> Result<AppStartupState, crate::BackendError> {
        super::app_startup_state(&self.0)
    }

    pub async fn dispatch(&self, request: AppWorkerRequest) -> Result<WorkerDispatchResult, crate::BackendError> {
        super::dispatch_worker_request(self.0.clone(), request).await
    }
}
