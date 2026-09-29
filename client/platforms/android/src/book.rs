//! Book handling for Android private and shared storage.
use std::path::{Path, PathBuf};
static LEASE_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn configure_lease_root(app_data: &Path) -> std::io::Result<()> {
    let root = app_data.join("book-leases");
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    if LEASE_ROOT.get_or_init(|| root.clone()) != &root {
        return Err(std::io::Error::other("Android book lock directory is already configured"));
    }
    Ok(())
}

pub fn lease_root() -> std::io::Result<&'static Path> {
    LEASE_ROOT.get().map(PathBuf::as_path).ok_or_else(|| std::io::Error::other("Android book lock directory is not configured"))
}

pub fn persist_book_noclobber(temporary: tempfile::TempPath, destination: &Path) -> Result<(), tempfile::PathPersistError> {
    match temporary.persist_noclobber(destination) {
        Ok(()) => Ok(()),
        Err(error) => {
            // Android shared-storage FUSE can reject RENAME_NOREPLACE. tempfile
            // then tries a hard link, which is also unavailable to Android apps.
            if matches!(error.error.raw_os_error(), Some(1 | 13 | 22 | 38 | 95)) {
                return copy_after_persist_failure(error, destination);
            }
            Err(error)
        }
    }
}

fn copy_after_persist_failure(mut failure: tempfile::PathPersistError, destination: &Path) -> Result<(), tempfile::PathPersistError> {
    let result = std::fs::File::open(&failure.path).and_then(|mut source| copy_book_noclobber(&mut source, destination));
    match result {
        Ok(()) => Ok(()), // Dropping the original TempPath removes the staging file.
        Err(error) => {
            failure.error = error;
            Err(failure)
        }
    }
}

fn copy_book_noclobber(source: &mut impl std::io::Read, destination: &Path) -> std::io::Result<()> {
    use std::io::Write as _;
    struct PendingCopy<'a> {
        path: &'a Path,
        file: Option<std::fs::File>,
        complete: bool,
    }
    impl Drop for PendingCopy<'_> {
        fn drop(&mut self) {
            drop(self.file.take());
            if !self.complete {
                let _ = std::fs::remove_file(self.path);
            }
        }
    }
    // Do not replace a file that appeared after the caller checked the name.
    // Unlike the rename path, this compatibility fallback is not crash-atomic:
    // the destination is visible during copying. Ordinary errors clean it up.
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(destination)?;
    let mut pending = PendingCopy { path: destination, file: Some(file), complete: false };
    let output = pending.file.as_mut().unwrap();
    // Keep this fallback to ordinary reads/writes, without filesystem-specific
    // copy acceleration that the shared-storage provider may also reject.
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = match source.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
    }
    output.sync_all()?;
    pending.complete = true;
    Ok(())
}

#[cfg(test)]
mod android_book_tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    fn denied_rename(directory: &Path) -> tempfile::PathPersistError {
        let mut source = tempfile::NamedTempFile::new_in(directory).unwrap();
        source.write_all(b"verified book").unwrap();
        tempfile::PathPersistError { error: std::io::Error::from_raw_os_error(13), path: source.into_temp_path() }
    }

    #[test]
    fn lease_directory_is_private_and_cannot_be_reconfigured() {
        assert!(lease_root().is_err());
        let profile = tempfile::tempdir().unwrap();
        configure_lease_root(profile.path()).unwrap();
        let expected = profile.path().join("book-leases").canonicalize().unwrap();
        assert_eq!(lease_root().unwrap(), expected);
        configure_lease_root(&profile.path().join(".")).unwrap();
        let other = tempfile::tempdir().unwrap();
        assert!(configure_lease_root(other.path()).is_err());
        assert_eq!(lease_root().unwrap(), expected);
    }

    #[test]
    fn android_book_falls_back_after_denied_hard_link() {
        let directory = tempfile::tempdir().unwrap();
        let failure = denied_rename(directory.path());
        let staged = failure.path.to_path_buf();
        let destination = directory.path().join("book.epub");
        copy_after_persist_failure(failure, &destination).unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), b"verified book");
        assert!(!staged.exists());
    }

    #[test]
    fn android_book_fallback_preserves_a_concurrent_destination() {
        let directory = tempfile::tempdir().unwrap();
        let failure = denied_rename(directory.path());
        let destination = directory.path().join("book.epub");
        std::fs::write(&destination, b"existing user book").unwrap();
        let failure = copy_after_persist_failure(failure, &destination).unwrap_err();
        assert_eq!(failure.error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(destination).unwrap(), b"existing user book");
        assert_eq!(std::fs::read(&failure.path).unwrap(), b"verified book");
    }

    #[test]
    fn android_book_fallback_removes_an_incomplete_copy() {
        struct FailedRead;
        impl std::io::Read for FailedRead {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from_raw_os_error(13))
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("book.epub");
        let mut source = std::io::Cursor::new(b"partial book").chain(FailedRead);
        let error = copy_book_noclobber(&mut source, &destination).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(13));
        assert!(!destination.exists());
    }
}
