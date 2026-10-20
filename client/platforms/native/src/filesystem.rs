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
/// Rename a file or directory without replacing an existing destination.
pub fn rename_noclobber(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;
        let source = CString::new(source.as_os_str().as_bytes())?;
        let destination = CString::new(destination.as_os_str().as_bytes())?;
        // Both C strings remain alive for the syscall, which only reads them.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let result = unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, source.as_ptr(), libc::AT_FDCWD, destination.as_ptr(), libc::RENAME_NOREPLACE) };
        #[cfg(target_os = "macos")]
        let result = unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
        if result == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    }
    #[cfg(windows)]
    {
        use std::io::{Error, ErrorKind};
        use std::os::windows::ffi::OsStrExt as _;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32;
        }
        let wide = |path: &Path| -> std::io::Result<Vec<u16>> {
            let mut path: Vec<_> = path.as_os_str().encode_wide().collect();
            if path.contains(&0) {
                return Err(Error::new(ErrorKind::InvalidInput, "path contains a NUL character"));
            }
            path.push(0);
            Ok(path)
        };
        let source = wide(source)?;
        let destination = wide(destination)?;
        // With no replacement flag, Windows rejects an existing destination
        // atomically. Both terminated strings remain alive for the call.
        if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0) } != 0 { Ok(()) } else { Err(Error::last_os_error()) }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", windows)))]
    {
        let _ = (source, destination);
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "exclusive rename is unavailable on this platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn rename_noclobber_moves_files_and_directories_to_absent_destinations() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("source-file");
        let moved_file = root.path().join("moved-file");
        fs::write(&file, b"book").unwrap();
        rename_noclobber(&file, &moved_file).unwrap();
        assert!(!file.exists());
        assert_eq!(fs::read(moved_file).unwrap(), b"book");
        let directory = root.path().join("source-directory");
        let moved_directory = root.path().join("moved-directory");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("book"), b"nested book").unwrap();
        rename_noclobber(&directory, &moved_directory).unwrap();
        assert!(!directory.exists());
        assert_eq!(fs::read(moved_directory.join("book")).unwrap(), b"nested book");
    }

    #[test]
    fn rename_noclobber_keeps_both_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        std::fs::write(&source, b"new book").unwrap();
        std::fs::write(&destination, b"existing book").unwrap();
        assert!(rename_noclobber(&source, &destination).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"new book");
        assert_eq!(std::fs::read(&destination).unwrap(), b"existing book");
    }

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
