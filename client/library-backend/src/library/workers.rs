//! Lifetime ownership for this library's background tasks.

use client_platform_runtime::executor::BackendTask;

/// Dropping this value cancels/joins every task held by the platform task
/// wrapper. A session therefore owns all of its workers as one unit.
#[derive(Default)]
pub struct LibraryWorkers {
    account_observer: Option<BackendTask<()>>,
    remote: Option<BackendTask<()>>,
    sync: Option<BackendTask<()>>,
    assets: Option<BackendTask<()>>,
}

impl LibraryWorkers {
    pub fn set_account_observer(&mut self, task: BackendTask<()>) {
        self.account_observer = Some(task);
    }
    pub fn set_remote(&mut self, task: BackendTask<()>) {
        self.remote = Some(task);
    }
    pub fn set_sync(&mut self, task: BackendTask<()>) {
        self.sync = Some(task);
    }
    pub fn set_assets(&mut self, task: BackendTask<()>) {
        self.assets = Some(task);
    }
}
