use flate2::read::GzDecoder;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const KOBO_MARKER_DIRECTORY: &str = ".kobo";
const REQUIRED_INSTALLED_FILES: &[&str] = &[".adds/bokheim/desktop-gpui-kobo", ".adds/bokheim/fbink", ".adds/bokheim/run.sh", ".adds/bokheim/run-monitored.sh", ".adds/nm/desktop-gpui-kobo"];
const PROTECTED_APP_DATA_DIRECTORIES: &[&str] = &[".adds/bokheim/data", ".adds/bokheim/libraries", ".adds/bokheim/remote-book-cache", ".adds/bokheim/taxonomy"];
const PROTECTED_APP_DATA_FILES: &[&str] =
    &[".adds/bokheim/browsing_preferences.json", ".adds/bokheim/detached-libraries.json", ".adds/bokheim/libraries.csv", ".adds/bokheim/reader_preferences.json", ".adds/bokheim/session.json", ".adds/bokheim/transfer_preferences.json"];
const MAX_UNPACKED_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KoboDevice {
    root: PathBuf,
    display_name: String,
}

impl KoboDevice {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn nickel_menu_detected(&self) -> bool {
        let nickel_menu = self.root.join(".adds/nm");
        nickel_menu.join("doc").exists() || nickel_menu.join("config").exists()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InstallSummary {
    pub directories: usize,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InstallProgress {
    pub copied_bytes: u64,
    pub total_bytes: u64,
}

pub fn detect_kobo_devices() -> Vec<KoboDevice> {
    let mut candidates = BTreeSet::new();
    if let Some(path) = std::env::var_os("BOKHEIM_KOBO_MOUNT") {
        candidates.insert(PathBuf::from(path));
    }
    platform_mount_candidates(&mut candidates);

    candidates
        .into_iter()
        .filter_map(|root| {
            if !root.join(KOBO_MARKER_DIRECTORY).is_dir() {
                return None;
            }
            let root = root.canonicalize().ok()?;
            let volume = root.file_name().and_then(OsStr::to_str).filter(|name| !name.is_empty()).unwrap_or("Kobo eReader");
            Some(KoboDevice { display_name: format!("Kobo eReader ({volume}) — {}", root.display()), root })
        })
        .collect()
}

#[cfg(target_os = "windows")]
fn platform_mount_candidates(candidates: &mut BTreeSet<PathBuf>) {
    let drive_mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    for offset in 0..26 {
        if drive_mask & (1 << offset) != 0 {
            candidates.insert(PathBuf::from(format!("{}:\\", (b'A' + offset) as char)));
        }
    }
}

#[cfg(target_os = "linux")]
fn platform_mount_candidates(candidates: &mut BTreeSet<PathBuf>) {
    let Ok(mounts) = fs::read_to_string("/proc/self/mounts") else {
        return;
    };
    for line in mounts.lines() {
        if let Some(encoded) = line.split_whitespace().nth(1) {
            candidates.insert(PathBuf::from(decode_linux_mount_field(encoded)));
        }
    }
}

#[cfg(target_os = "macos")]
fn platform_mount_candidates(candidates: &mut BTreeSet<PathBuf>) {
    if let Ok(volumes) = fs::read_dir("/Volumes") {
        candidates.extend(volumes.filter_map(Result::ok).map(|entry| entry.path()));
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn platform_mount_candidates(_: &mut BTreeSet<PathBuf>) {}

#[cfg(target_os = "linux")]
fn decode_linux_mount_field(value: &str) -> String {
    value.replace("\\040", " ").replace("\\011", "\t").replace("\\012", "\n").replace("\\134", "\\")
}

pub fn install_archive(device_root: &Path, archive: &[u8]) -> io::Result<InstallSummary> {
    install_archive_with_progress(device_root, archive, |_| {})
}

pub fn install_archive_with_progress(device_root: &Path, archive: &[u8], mut on_progress: impl FnMut(InstallProgress)) -> io::Result<InstallSummary> {
    let total_bytes = validate_archive_size(archive)?;
    let device_root = validate_device_root(device_root)?;
    let mut summary = InstallSummary::default();
    let mut copied_bytes = 0_u64;
    on_progress(InstallProgress { copied_bytes, total_bytes });
    let decoder = GzDecoder::new(archive);
    let mut package = tar::Archive::new(decoder);

    for entry in package.entries()? {
        let mut entry = entry?;
        let relative = sanitize_package_path(&entry.path()?)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let destination = device_root.join(&relative);
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            fs::create_dir_all(&destination)?;
            summary.directories += 1;
        } else if entry_type.is_file() {
            summary.bytes += install_file_atomically(&mut entry, &destination, &mut |bytes| {
                copied_bytes = copied_bytes.saturating_add(bytes);
                on_progress(InstallProgress { copied_bytes, total_bytes });
            })?;
            summary.files += 1;
        } else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("the Kobo package contains an unsupported entry: {}", relative.display())));
        }
    }

    for required in REQUIRED_INSTALLED_FILES {
        if !device_root.join(required).is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("installation is incomplete: {required} is missing")));
        }
    }
    Ok(summary)
}

pub fn validate_archive(archive: &[u8]) -> io::Result<()> {
    validate_archive_size(archive).map(|_| ())
}

fn validate_archive_size(archive: &[u8]) -> io::Result<u64> {
    let decoder = GzDecoder::new(archive);
    let mut package = tar::Archive::new(decoder);
    let mut required = REQUIRED_INSTALLED_FILES.iter().copied().collect::<BTreeSet<_>>();
    let mut unpacked_bytes = 0_u64;

    for entry in package.entries()? {
        let entry = entry?;
        let relative = sanitize_package_path(&entry.path()?)?;
        let entry_type = entry.header().entry_type();
        if !entry_type.is_dir() && !entry_type.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("the Kobo package contains an unsupported entry: {}", relative.display())));
        }
        if entry_type.is_file() {
            unpacked_bytes = unpacked_bytes.checked_add(entry.header().size()?).filter(|bytes| *bytes <= MAX_UNPACKED_PACKAGE_BYTES).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the Kobo package is unexpectedly large"))?;
            required.remove(package_entry_name(&relative).as_str());
        }
    }

    if let Some(path) = required.first() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("the Kobo package is incomplete: {path} is missing")));
    }
    Ok(unpacked_bytes)
}

fn validate_device_root(device_root: &Path) -> io::Result<PathBuf> {
    if !device_root.join(KOBO_MARKER_DIRECTORY).is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is not a mounted Kobo device", device_root.display())));
    }
    device_root.canonicalize()
}

/// Renders a sanitised package path the way the archive spells it. The
/// sanitised path is rebuilt component by component, so on Windows it comes
/// back separated by backslashes and never matches the slash-separated names
/// the package is required to contain.
fn package_entry_name(relative: &Path) -> String {
    relative.components().filter_map(|component| component.as_os_str().to_str()).collect::<Vec<_>>().join("/")
}

fn sanitize_package_path(path: &Path) -> io::Result<PathBuf> {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => clean.push(part),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unsafe path in Kobo package: {}", path.display())));
            }
        }
    }
    let allowed = clean.starts_with(".adds") || clean.starts_with("Bokheim");
    if !clean.as_os_str().is_empty() && !allowed {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected path in Kobo package: {}", path.display())));
    }
    let overwrites_library = clean.starts_with("Bokheim/Libraries") && clean != Path::new("Bokheim/Libraries");
    let overwrites_app_data = PROTECTED_APP_DATA_FILES.iter().any(|protected| clean == Path::new(protected)) || PROTECTED_APP_DATA_DIRECTORIES.iter().any(|protected| clean == Path::new(protected) || clean.starts_with(protected));
    if overwrites_library || overwrites_app_data {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("Kobo package attempts to overwrite user data: {}", path.display())));
    }
    Ok(clean)
}

fn install_file_atomically<R: Read>(source: &mut R, destination: &Path, on_bytes: &mut impl FnMut(u64)) -> io::Result<u64> {
    let parent = destination.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "package file has no parent"))?;
    fs::create_dir_all(parent)?;
    let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = destination.file_name().and_then(OsStr::to_str).unwrap_or("asset");
    let temporary = parent.join(format!(".{name}.bokheim-install-{}-{sequence}.tmp", std::process::id()));
    let backup = parent.join(format!(".{name}.bokheim-install-{}-{sequence}.bak", std::process::id()));

    let result = (|| {
        let mut output = OpenOptions::new().create_new(true).write(true).open(&temporary)?;
        let mut bytes = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count])?;
            bytes += count as u64;
            on_bytes(count as u64);
        }
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
        Ok(bytes)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        if backup.exists() && !destination.exists() {
            let _ = fs::rename(&backup, destination);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Cursor;

    fn test_archive(extra_path: Option<&str>) -> Vec<u8> {
        let mut compressed = Vec::new();
        {
            let encoder = GzEncoder::new(&mut compressed, Compression::fast());
            let mut archive = tar::Builder::new(encoder);
            for path in REQUIRED_INSTALLED_FILES {
                let bytes = format!("contents for {path}");
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                archive.append_data(&mut header, path, Cursor::new(bytes)).unwrap();
            }
            if let Some(path) = extra_path {
                let mut header = tar::Header::new_gnu();
                header.set_size(1);
                header.set_mode(0o644);
                header.set_cksum();
                archive.append_data(&mut header, path, Cursor::new([1])).unwrap();
            }
            archive.into_inner().unwrap().finish().unwrap();
        }
        compressed
    }

    #[test]
    fn installs_only_expected_package_roots() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join(".kobo")).unwrap();
        let summary = install_archive(directory.path(), &test_archive(None)).unwrap();
        assert_eq!(summary.files, REQUIRED_INSTALLED_FILES.len());
        assert!(directory.path().join(".adds/bokheim/desktop-gpui-kobo").is_file());
    }

    #[test]
    fn reports_byte_accurate_installation_progress() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join(".kobo")).unwrap();
        let mut updates = Vec::new();
        let summary = install_archive_with_progress(directory.path(), &test_archive(None), |progress| updates.push(progress)).unwrap();
        let last = updates.last().copied().unwrap();
        assert_eq!(last.copied_bytes, summary.bytes);
        assert_eq!(last.total_bytes, summary.bytes);
        assert!(updates.windows(2).all(|pair| pair[0].copied_bytes <= pair[1].copied_bytes));
    }

    #[test]
    fn refuses_non_kobo_destination() {
        let directory = tempfile::tempdir().unwrap();
        let error = install_archive(directory.path(), &test_archive(None)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn refuses_files_outside_package_roots() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join(".kobo")).unwrap();
        let error = install_archive(directory.path(), &test_archive(Some("unexpected/file"))).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn names_package_entries_with_slashes_on_every_platform() {
        // The required entries are spelled with slashes, so the sanitised path
        // has to be rendered the same way on Windows, where rebuilding it
        // component by component would otherwise separate it with backslashes.
        for required in REQUIRED_INSTALLED_FILES {
            let sanitized = sanitize_package_path(Path::new(required)).unwrap();
            assert_eq!(package_entry_name(&sanitized), *required);
        }
    }

    #[test]
    fn replaces_an_existing_install() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join(".kobo")).unwrap();
        install_archive(directory.path(), &test_archive(None)).unwrap();
        install_archive(directory.path(), &test_archive(None)).unwrap();
        assert!(directory.path().join(".adds/bokheim/fbink").is_file());
    }

    #[test]
    fn reinstall_preserves_books_databases_and_preferences() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join(".kobo")).unwrap();
        fs::create_dir_all(directory.path().join("Bokheim/Libraries/default/.bokheim")).unwrap();
        fs::create_dir_all(directory.path().join(".adds/bokheim/data")).unwrap();
        fs::write(directory.path().join("Bokheim/Libraries/default/book.epub"), b"book data").unwrap();
        fs::write(directory.path().join("Bokheim/Libraries/default/.bokheim/library.db"), b"database").unwrap();
        fs::write(directory.path().join(".adds/bokheim/reader_preferences.json"), b"preferences").unwrap();
        fs::write(directory.path().join(".adds/bokheim/data/canvas-mode"), b"canvas mode").unwrap();

        install_archive(directory.path(), &test_archive(None)).unwrap();

        assert_eq!(fs::read(directory.path().join("Bokheim/Libraries/default/book.epub")).unwrap(), b"book data");
        assert_eq!(fs::read(directory.path().join("Bokheim/Libraries/default/.bokheim/library.db")).unwrap(), b"database");
        assert_eq!(fs::read(directory.path().join(".adds/bokheim/reader_preferences.json")).unwrap(), b"preferences");
        assert_eq!(fs::read(directory.path().join(".adds/bokheim/data/canvas-mode")).unwrap(), b"canvas mode");
    }

    #[test]
    fn refuses_packages_that_contain_user_data() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join(".kobo")).unwrap();
        let error = install_archive(directory.path(), &test_archive(Some("Bokheim/Libraries/default/book.epub"))).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
