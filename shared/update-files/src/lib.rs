//! Durable update file operations shared by native activation and Kobo installs.
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn parent_directory(path: &Path) -> io::Result<&Path> {
    path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("update path has no parent directory: {}", path.display())))
}

pub fn sync_file(path: &Path) -> io::Result<()> {
    // FlushFileBuffers on Windows requires a writable handle.
    OpenOptions::new().write(true).open(path)?.sync_all()
}

pub fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(windows)]
    {
        // No unprivileged directory-fsync equivalent exists on Windows.
        if fs::metadata(path)?.is_dir() { Ok(()) } else { Err(io::Error::new(io::ErrorKind::InvalidInput, "expected an update directory")) }
    }
}

pub fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        fs::rename(source, destination)?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
        }
        let wide = |path: &Path| -> io::Result<Vec<u16>> {
            let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
            if value.contains(&0) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "update path contains a NUL"));
            }
            value.push(0);
            Ok(value)
        };
        let source = wide(source)?;
        let destination = wide(destination)?;
        // REPLACE_EXISTING | WRITE_THROUGH; never copy across volumes or defer
        // replacement until reboot. Both buffers remain NUL-terminated and live.
        if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 1 | 8) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    sync_dir(parent_directory(destination)?)
}

pub fn remove(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let retired = path.with_extension("update-retired");
        replace(path, &retired)?;
        let _ = fs::remove_file(retired);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::remove_file(path)?;
        sync_dir(parent_directory(path)?)
    }
}

pub fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    ignore_missing(fs::remove_file(path))
}
pub fn remove_dir_if_exists(path: &Path) -> io::Result<()> {
    ignore_missing(fs::remove_dir_all(path))
}

fn ignore_missing(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

pub fn existing_regular_file(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidData, format!("installation file would replace a directory or special file: {}", path.display()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn read_bounded(input: impl Read, maximum: usize) -> io::Result<Vec<u8>> {
    let limit = u64::try_from(maximum).ok().and_then(|maximum| maximum.checked_add(1)).ok_or_else(|| io::Error::other("update metadata size overflow"))?;
    let mut bytes = Vec::new();
    input.take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "update metadata exceeds size limit"));
    }
    Ok(bytes)
}

/// Inspect each chunk before writing, then report (chunk bytes, total bytes).
/// Artifact verification uses the inspection hook to enforce its signed limit.
pub fn copy_observed(mut input: impl Read, output: &mut impl Write, mut inspect: impl FnMut(&[u8], u64) -> io::Result<()>, mut progress: impl FnMut(u64, u64)) -> io::Result<u64> {
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total.checked_add(count as u64).ok_or_else(|| io::Error::other("update size overflow"))?;
        inspect(&buffer[..count], total)?;
        output.write_all(&buffer[..count])?;
        progress(count as u64, total);
    }
    Ok(total)
}

/// Stage and flush a complete file, recheck the destination, then replace it.
/// Callers choose the temporary name because Kobo firmware recovery knows its
/// suffix, while a USB installation needs a unique name for each write.
pub fn write_atomic(path: &Path, temporary: &Path, input: impl Read, permissions: Option<Permissions>, mut on_bytes: impl FnMut(u64)) -> io::Result<u64> {
    with_atomic_file(path, temporary, permissions, |output| copy_observed(input, output, |_, _| Ok(()), |bytes, _| on_bytes(bytes)))
}

pub fn with_atomic_file<T>(path: &Path, temporary: &Path, permissions: Option<Permissions>, write: impl FnOnce(&mut File) -> io::Result<T>) -> io::Result<T> {
    if path == temporary {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "update temporary path equals destination"));
    }
    existing_regular_file(path)?;
    fs::create_dir_all(parent_directory(path)?)?;
    remove_file_if_exists(temporary)?;
    let result = (|| {
        let mut output = OpenOptions::new().create_new(true).write(true).open(temporary)?;
        let result = write(&mut output)?;
        if let Some(permissions) = permissions {
            fs::set_permissions(temporary, permissions)?;
        }
        output.sync_all()?;
        drop(output);
        existing_regular_file(path)?;
        replace(temporary, path)?;
        Ok(result)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub fn copy_atomic(source: &Path, destination: &Path, temporary: &Path) -> io::Result<()> {
    let input = File::open(source)?;
    let permissions = input.metadata()?.permissions();
    write_atomic(destination, temporary, input, Some(permissions), |_| {}).map(|_| ())
}

pub fn probe_write(directory: &Path) -> io::Result<()> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let path = directory.join(format!(".bokheim-write-check-{}-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
    let file = OpenOptions::new().write(true).create_new(true).open(&path)?;
    drop(file);
    fs::remove_file(path)
}

pub fn same_volume(left: &Path, right: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(fs::metadata(left)?.dev() == fs::metadata(right)?.dev())
    }
    #[cfg(windows)]
    {
        // Callers reject reparse points before comparing canonical prefixes.
        let prefix = |path: &Path| -> io::Result<String> { Ok(path.canonicalize()?.components().next().ok_or_else(|| io::Error::other("missing volume prefix"))?.as_os_str().to_string_lossy().to_ascii_lowercase()) };
        Ok(prefix(left)? == prefix(right)?)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (left, right);
        Err(io::Error::new(io::ErrorKind::Unsupported, "update volume comparison is unsupported"))
    }
}
