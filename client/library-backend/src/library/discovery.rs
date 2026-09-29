//! Filesystem discovery capability owned by a library session.
//!
//! It contains only a root path. Database snapshots, persistence, scheduling,
//! events, and retry decisions belong to `LibrarySession`.

#[derive(Clone)]
pub(crate) struct LibraryScanner {
    root: String,
}

impl LibraryScanner {
    pub(crate) fn from_root(root: &str) -> std::io::Result<Self> {
        Ok(Self { root: root.to_owned() })
    }

    pub(crate) fn enabled(&self) -> bool {
        cfg!(all(feature = "scanner", not(target_arch = "wasm32")))
    }

    pub(crate) fn root(&self) -> &str {
        &self.root
    }

    pub(crate) fn discover(&self, snapshot: &library_database::ScanSnapshot) -> library_database::FilesystemScan {
        #[cfg(all(feature = "scanner", not(target_arch = "wasm32")))]
        {
            filesystem_scanner::filesystem_scan::discover(&self.root, snapshot)
        }
        #[cfg(not(all(feature = "scanner", not(target_arch = "wasm32"))))]
        {
            let _ = snapshot;
            library_database::FilesystemScan { readable: false, ..library_database::FilesystemScan::default() }
        }
    }

    pub(crate) async fn inspect(&self, scan: &library_database::FilesystemScan, snapshot: &library_database::ScanSnapshot, host: &std::sync::Arc<dyn cpu_host::CpuHost>) -> library_database::InspectedFilesystemScan {
        #[cfg(all(feature = "scanner", not(target_arch = "wasm32")))]
        {
            filesystem_scanner::filesystem_scan::inspect(&self.root, scan, snapshot, host).await
        }
        #[cfg(not(all(feature = "scanner", not(target_arch = "wasm32"))))]
        {
            let _ = (scan, snapshot, host);
            library_database::InspectedFilesystemScan { readable: false, ..library_database::InspectedFilesystemScan::default() }
        }
    }
}

pub fn estimate_folders(locators: Vec<String>, cancelled: &std::sync::atomic::AtomicBool) -> Result<(u64, u64, u64), crate::BackendError> {
    #[cfg(all(feature = "scanner", not(target_arch = "wasm32")))]
    {
        filesystem_scanner::estimate_folders_cancellable(locators.into_iter().map(std::path::PathBuf::from).collect(), || cancelled.load(std::sync::atomic::Ordering::Acquire))
            .ok_or_else(|| crate::BackendError::message("Folder estimate cancelled"))
    }
    #[cfg(not(all(feature = "scanner", not(target_arch = "wasm32"))))]
    {
        let _ = (locators, cancelled);
        Err(crate::BackendError::message("Folder size estimation is unavailable on this device"))
    }
}
