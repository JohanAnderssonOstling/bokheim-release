//! Mounted-device discovery and filesystem operations.
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const KOBO_MARKER_DIRECTORY: &str = ".kobo";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KoboDevice {
    root: PathBuf,
    display_name: String,
}

impl KoboDevice {
    pub fn profile(&self) -> Result<&'static kobo_profile::Profile, io::Error> {
        kobo_profile::Profile::detect("", &fs::read_to_string(self.root.join(".kobo/version"))?)
            .map_err(|error| io::Error::new(io::ErrorKind::Unsupported, error))
    }

    /// Inspect an explicitly selected mounted device.
    pub fn from_mount(root: &Path) -> io::Result<Self> {
        if !root.join(KOBO_MARKER_DIRECTORY).is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("Kobo is no longer mounted at {}", root.display())));
        }
        let root = root.canonicalize()?;
        let volume = root.file_name().and_then(|name| name.to_str()).filter(|name| !name.is_empty()).unwrap_or("Kobo eReader");
        Ok(Self { display_name: format!("Kobo eReader ({volume})"), root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
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
