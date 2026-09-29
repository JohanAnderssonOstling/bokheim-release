//! Mounted-device discovery and filesystem operations.
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const KOBO_MARKER_DIRECTORY: &str = ".kobo";
const KOBO_LIBRARIES_PATH: &str = "Bokheim/Libraries";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KoboDevice {
    root: PathBuf,
    display_name: String,
}

impl KoboDevice {
    /// Inspect an explicitly selected mounted device.
    pub fn from_mount(root: &Path) -> io::Result<Self> {
        if !root.join(KOBO_MARKER_DIRECTORY).is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("Kobo is no longer mounted at {}", root.display())));
        }
        let root = root.canonicalize()?;
        let volume = root.file_name().and_then(|name| name.to_str()).filter(|name| !name.is_empty()).unwrap_or("Kobo eReader");
        Ok(Self { display_name: format!("Kobo eReader ({volume})"), root })
    }

    /// Validate the mount and create its library destination directory.
    pub fn prepare_libraries_dir(&self) -> io::Result<PathBuf> {
        let device = Self::from_mount(&self.root)?;
        let libraries_dir = device.libraries_dir();
        fs::create_dir_all(&libraries_dir)?;
        let libraries_dir = libraries_dir.canonicalize()?;
        if !libraries_dir.starts_with(&device.root) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Kobo libraries directory escapes the mounted device"));
        }
        Ok(libraries_dir)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn libraries_dir(&self) -> PathBuf {
        self.root.join(KOBO_LIBRARIES_PATH)
    }
}

pub fn detect_kobo_devices() -> Vec<KoboDevice> {
    let mut candidates = BTreeSet::new();
    if let Some(path) = std::env::var_os("BOKHEIM_KOBO_MOUNT") {
        candidates.insert(PathBuf::from(path));
    }
    platform_mount_candidates(&mut candidates);

    candidates.into_iter().filter_map(|root| KoboDevice::from_mount(&root).ok()).collect()
}

#[cfg(target_os = "linux")]
fn platform_mount_candidates(candidates: &mut BTreeSet<PathBuf>) {
    let Ok(mounts) = fs::read_to_string("/proc/self/mounts") else {
        return;
    };
    for line in mounts.lines() {
        if let Some(encoded) = line.split_whitespace().nth(1) {
            candidates.insert(PathBuf::from(decode_mount_field(encoded)));
        }
    }
}

#[cfg(target_os = "macos")]
fn platform_mount_candidates(candidates: &mut BTreeSet<PathBuf>) {
    if let Ok(volumes) = fs::read_dir("/Volumes") {
        candidates.extend(volumes.filter_map(Result::ok).map(|entry| entry.path()));
    }
}

#[cfg(target_os = "windows")]
fn platform_mount_candidates(candidates: &mut BTreeSet<PathBuf>) {
    for drive in b'A'..=b'Z' {
        candidates.insert(PathBuf::from(format!("{}:\\\\", drive as char)));
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_mount_candidates(_: &mut BTreeSet<PathBuf>) {}

#[cfg(target_os = "linux")]
fn decode_mount_field(value: &str) -> String {
    value.replace("\\040", " ").replace("\\011", "\t").replace("\\012", "\n").replace("\\134", "\\")
}

/// Copy a file using temporary-file replacement; return whether its contents changed.
pub fn copy_file_if_changed(source: &Path, destination: &Path) -> io::Result<bool> {
    if destination.is_file() && files_equal(source, destination)? {
        return Ok(false);
    }
    let parent = destination.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "transfer destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = destination.file_name().and_then(|name| name.to_str()).unwrap_or("asset");
    let temporary = parent.join(format!(".{file_name}.bokheim-transfer-{}.tmp", Uuid::new_v4()));
    let backup = parent.join(format!(".{file_name}.bokheim-transfer-{}.bak", Uuid::new_v4()));
    let result = (|| {
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        io::copy(&mut input, &mut output)?;
        output.flush()?;
        output.sync_all()?;
        drop(output);

        let had_destination = destination.exists();
        if had_destination {
            fs::rename(destination, &backup)?;
        }
        if let Err(error) = fs::rename(&temporary, destination) {
            if had_destination {
                let _ = fs::rename(&backup, destination);
            }
            return Err(error);
        }
        if had_destination {
            fs::remove_file(&backup)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        if backup.exists() && !destination.exists() {
            let _ = fs::rename(&backup, destination);
        }
    }
    result?;
    Ok(true)
}

fn files_equal(left: &Path, right: &Path) -> io::Result<bool> {
    if fs::metadata(left)?.len() != fs::metadata(right)?.len() {
        return Ok(false);
    }
    let mut left = File::open(left)?;
    let mut right = File::open(right)?;
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    loop {
        let left_read = left.read(&mut left_buffer)?;
        let right_read = right.read(&mut right_buffer)?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}
