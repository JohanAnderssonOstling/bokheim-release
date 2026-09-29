//! Filesystem durability mechanisms independent of book layout.
use std::path::Path;
pub fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
pub fn persist_noclobber(temporary: tempfile::TempPath, destination: &Path) -> Result<(), tempfile::PathPersistError> {
    temporary.persist_noclobber(destination)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persist_keeps_existing_destination_and_recoverable_temporary() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("existing");
        std::fs::write(&target, b"original").unwrap();
        let temporary = tempfile::NamedTempFile::new_in(root.path()).unwrap().into_temp_path();
        std::fs::write(&temporary, b"replacement").unwrap();
        let error = persist_noclobber(temporary, &target).unwrap_err();
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert_eq!(std::fs::read(&error.path).unwrap(), b"replacement");
        let next = root.path().join("new");
        persist_noclobber(error.path, &next).unwrap();
        sync_directory(root.path()).unwrap();
        assert_eq!(std::fs::read(next).unwrap(), b"replacement");
    }
}

/// An owned source opened by the backend thread when its import reaches the file.
pub struct ImportSource(Box<dyn FnOnce() -> ImportReaderFuture + Send>, Option<u64>);
pub type ImportReader = Box<dyn std::io::Read + Send>;
pub type ImportReaderFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<ImportReader, String>> + Send>>;
impl ImportSource {
    /// The opener must not depend on the UI thread.
    pub fn new(open: Box<dyn FnOnce() -> ImportReaderFuture + Send>) -> Self {
        Self(open, None)
    }
    pub fn with_size_bytes(mut self, size: Option<u64>) -> Self {
        self.1 = size;
        self
    }
    pub fn size_bytes(&self) -> Option<u64> {
        self.1
    }
    pub async fn open(self) -> Result<ImportReader, String> {
        (self.0)().await
    }
}
