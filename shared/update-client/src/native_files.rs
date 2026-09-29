//! Durable file operations used by staging and activation.
use std::{fs::{self, OpenOptions}, path::Path};
#[cfg(unix)]
use std::fs::File;

pub fn sync_file(path: &Path) -> Result<(), String> {
    // Windows FlushFileBuffers requires a handle opened with write access.
    OpenOptions::new().write(true).open(path).and_then(|f| f.sync_all()).map_err(|e| e.to_string())
}

pub fn sync_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    { File::open(path).and_then(|f| f.sync_all()).map_err(|e| e.to_string()) }
    #[cfg(windows)]
    {
        // Windows has no unprivileged equivalent of POSIX directory fsync.
        // File contents are explicitly flushed; replacements below request
        // write-through. Do not attempt FlushFileBuffers on a read-only directory.
        if fs::metadata(path).map_err(|e| e.to_string())?.is_dir() { Ok(()) }
        else { Err("expected an update directory".into()) }
    }
}

pub fn replace(source: &Path, destination: &Path) -> Result<(), String> {
    #[cfg(unix)]
    { fs::rename(source, destination).map_err(|e| e.to_string())?; }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" { fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32; }
        let path = |p: &Path| -> Result<Vec<u16>, String> {
            let mut value: Vec<_> = p.as_os_str().encode_wide().collect();
            if value.contains(&0) { return Err("update path contains a NUL".into()); }
            value.push(0);
            Ok(value)
        };
        let source = path(source)?;
        let destination = path(destination)?;
        // REPLACE_EXISTING | WRITE_THROUGH. Cross-volume copy and reboot-time
        // replacement are deliberately disabled: staging is a sibling file.
        // SAFETY: both UTF-16 buffers are NUL-terminated and live through the call.
        if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 1 | 8) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    sync_dir(destination.parent().ok_or("update destination has no parent")?)
}

pub fn remove(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        // First retire the authoritative name with a write-through rename. An
        // interrupted cleanup can leave a harmless tombstone, not an active journal.
        let retired = path.with_extension("update-retired");
        replace(path, &retired)?;
        let _ = fs::remove_file(retired);
        Ok(())
    }
    #[cfg(unix)]
    {
        fs::remove_file(path).map_err(|e| e.to_string())?;
        sync_dir(path.parent().ok_or("update file has no parent")?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replaces_existing_file_and_preserves_it_on_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let next = dir.path().join("next"); let live = dir.path().join("live");
        fs::write(&live, b"old").unwrap(); fs::write(&next, b"new").unwrap();
        sync_file(&next).unwrap(); replace(&next, &live).unwrap();
        assert_eq!(fs::read(&live).unwrap(), b"new");
        assert!(replace(&next, &live).is_err());
        assert_eq!(fs::read(&live).unwrap(), b"new");
    }

    #[cfg(windows)]
    #[test]
    fn locked_destination_can_be_retried_after_owner_exits() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("Bokheim.exe");
        let next = dir.path().join("candidate.exe");
        fs::write(&live, b"old").unwrap(); fs::write(&next, b"new").unwrap();
        let owner = OpenOptions::new().read(true).share_mode(0).open(&live).unwrap();
        assert!(replace(&next, &live).is_err());
        assert!(next.exists());
        drop(owner);
        assert_eq!(fs::read(&live).unwrap(), b"old");
        replace(&next, &live).unwrap();
        assert_eq!(fs::read(&live).unwrap(), b"new");
    }
}
