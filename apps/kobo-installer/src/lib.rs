use kobo_package::{MENU_CONFIGURATION as NICKEL_MENU_CONFIGURATION_PATH, REQUIRED_FILES as REQUIRED_INSTALLED_FILES};
use kobo_package::{Policy, destination, inspect, visit};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use update_files::existing_regular_file;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use tar::EntryType;

mod device_monitor;
pub use device_monitor::{DeviceDetection, DeviceMonitor};

const KOBO_MARKER_DIRECTORY: &str = ".kobo";
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const NICKEL_MENU_ARCHIVE: &[u8] = include_bytes!("../../desktop-gpui/kobo/nickelmenu-install/KoboRoot.tgz");
const NICKEL_MENU_CONFIGURATION: &[u8] = include_bytes!("../../desktop-gpui/kobo/nickelmenu");
const NICKEL_MENU_ICONS: &[(&str, &[u8])] = &[
    (".adds/bokheim/nickelmenu-icons/bokheim.png", include_bytes!("../../ui/design-tokens/assets/icon/bokheim-kobo.png")),
    (".adds/bokheim/nickelmenu-icons/bokheim-active.png", include_bytes!("../../ui/design-tokens/assets/icon/bokheim-kobo-active.png")),
];
const NICKEL_MENU_SHA256: &str = "c65077192df4c0857b0ff3704d708053355b311ce2497721c6d29ab5526199d2";
pub use client_platform_kobo::{EjectOutcome, eject_device};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KoboDevice {
    root: PathBuf,
    display_name: String,
}

impl KoboDevice {
    pub fn profile(&self) -> io::Result<&'static kobo_profile::Profile> {
        kobo_package::installed_profile(&fs::read_to_string(self.root.join(".kobo/version"))?)
    }

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

pub fn install_archive_with_progress(device_root: &Path, archive: &[u8], mut on_progress: impl FnMut(InstallProgress)) -> io::Result<InstallSummary> {
    let device_root = validate_device_root(device_root)?;
    let profile = kobo_package::installed_profile(&fs::read_to_string(device_root.join(".kobo/version"))?)?;
    kobo_package::validate_device(archive, Policy::Usb, profile)?;
    let total_bytes = validate_archive_size(archive, Some(&device_root))? + NICKEL_MENU_ARCHIVE.len() as u64 + NICKEL_MENU_ICONS.iter().map(|(_, bytes)| bytes.len() as u64).sum::<u64>();
    validate_nickel_menu_install(&device_root)?;
    for (path, _) in NICKEL_MENU_ICONS {
        existing_regular_file(&device_root.join(path))?;
    }
    let mut summary = InstallSummary::default();
    let mut copied_bytes = 0_u64;
    on_progress(InstallProgress { copied_bytes, total_bytes });
    visit(archive, Policy::Usb, |entry, relative| {
        let entry_type = entry.header().entry_type();
        let destination = destination(&device_root, &relative, entry_type.is_dir())?;
        if entry_type.is_dir() {
            fs::create_dir_all(&destination)?;
            summary.directories += 1;
        } else if entry_type.is_file() {
            let mut report_progress = |bytes| {
                copied_bytes = copied_bytes.saturating_add(bytes);
                on_progress(InstallProgress { copied_bytes, total_bytes });
            };
            summary.bytes += if relative == Path::new(NICKEL_MENU_CONFIGURATION_PATH) {
                // Older downloaded packages may contain diagnostic menu entries.
                // Always install the current Bokheim launch configuration.
                install_file_atomically(&mut &NICKEL_MENU_CONFIGURATION[..], &destination, &mut report_progress)?
            } else {
                install_file_atomically(entry, &destination, &mut report_progress)?
            };
            summary.files += 1;
        }
        Ok(())
    })?;

    for (path, bytes) in NICKEL_MENU_ICONS {
        summary.bytes += install_file_atomically(&mut &bytes[..], &device_root.join(path), &mut |bytes| {
            copied_bytes = copied_bytes.saturating_add(bytes);
            on_progress(InstallProgress { copied_bytes, total_bytes });
        })?;
        summary.files += 1;
    }
    for required in REQUIRED_INSTALLED_FILES {
        if !device_root.join(required).is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("installation is incomplete: {required} is missing")));
        }
    }
    validate_nickel_menu_install(&device_root)?;
    summary.bytes += install_file_atomically(&mut &NICKEL_MENU_ARCHIVE[..], &device_root.join(".kobo/KoboRoot.tgz"), &mut |bytes| {
        copied_bytes = copied_bytes.saturating_add(bytes);
        on_progress(InstallProgress { copied_bytes, total_bytes });
    })?;
    summary.files += 1;
    Ok(summary)
}

fn validate_nickel_menu_install(root: &Path) -> io::Result<()> {
    if hex::encode(Sha256::digest(NICKEL_MENU_ARCHIVE)) != NICKEL_MENU_SHA256 {
        return Err(io::Error::other("Embedded NickelMenu checksum mismatch"));
    }
    let version = fs::read_to_string(root.join(".kobo/version"))?;
    kobo_package::installed_profile(&version)?;
    let pending = root.join(".kobo/KoboRoot.tgz");
    if pending.exists() && fs::read(&pending)? != NICKEL_MENU_ARCHIVE {
        return Err(io::Error::other("A different Kobo firmware/update package is pending. Finish that update before installing Bokheim; it will not be overwritten."));
    }
    let upgrade = root.join(".kobo/upgrade");
    if upgrade.is_dir() && fs::read_dir(upgrade)?.next().is_some() {
        return Err(io::Error::other("A Kobo firmware upgrade is pending. Finish that upgrade before installing Bokheim."));
    }
    Ok(())
}

pub fn validate_archive(archive: &[u8]) -> io::Result<()> {
    validate_archive_size(archive, None).map(|_| ())
}

fn validate_archive_size(archive: &[u8], device_root: Option<&Path>) -> io::Result<u64> {
    let package = inspect(archive, Policy::Usb, device_root)?;
    // This installer replaces older menu configurations with its embedded one.
    package.files.iter().try_fold(0_u64, |total, file| {
        let bytes = if file.path == Path::new(NICKEL_MENU_CONFIGURATION_PATH) { NICKEL_MENU_CONFIGURATION.len() as u64 } else { file.bytes };
        total.checked_add(bytes).filter(|total| *total <= kobo_package::MAX_UNPACKED_BYTES).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the Kobo package is unexpectedly large"))
    })
}

fn validate_device_root(device_root: &Path) -> io::Result<PathBuf> {
    if !device_root.join(KOBO_MARKER_DIRECTORY).is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is not a mounted Kobo device", device_root.display())));
    }
    device_root.canonicalize()
}

#[cfg(test)]
fn sanitize_package_path(path: &Path, entry_type: EntryType) -> io::Result<PathBuf> {
    kobo_package::sanitize_path(path, entry_type, Policy::Usb)
}


fn install_file_atomically<R: Read>(source: &mut R, destination: &Path, on_bytes: &mut impl FnMut(u64)) -> io::Result<u64> {
    let parent = update_files::parent_directory(destination)?;
    let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = destination.file_name().and_then(OsStr::to_str).unwrap_or("asset");
    let temporary = parent.join(format!(".{name}.bokheim-install-{}-{sequence}.tmp", std::process::id()));
    update_files::write_atomic(destination, &temporary, source, None, on_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Cursor;

    fn test_archive(extra_path: Option<&str>) -> Vec<u8> {
        test_archive_with_entry(extra_path.map(|path| (path, EntryType::Regular)))
    }

    fn test_archive_with_entry(extra: Option<(&str, EntryType)>) -> Vec<u8> {
        test_archive_for_models(extra, None)
    }

    fn test_archive_for_models(extra: Option<(&str, EntryType)>, info: Option<&str>) -> Vec<u8> {
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
            if let Some((path, entry_type)) = extra {
                let bytes: &[u8] = if entry_type.is_dir() { &[] } else { &[1] };
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(entry_type);
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive.append_data(&mut header, path, Cursor::new(bytes)).unwrap();
            }
            if let Some(info) = info {
                let mut header = tar::Header::new_gnu();
                header.set_size(info.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive.append_data(&mut header, ".adds/bokheim/BUILD-INFO.txt", Cursor::new(info.as_bytes())).unwrap();
            }
            archive.into_inner().unwrap().finish().unwrap();
        }
        compressed
    }


    #[test]
    fn reinstall_preserves_books_databases_and_preferences() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join(".kobo")).unwrap();
        fs::write(directory.path().join(".kobo/version"), "serial,4.1.15,4.38.23828,unused,unused,0000000000388").unwrap();
        fs::create_dir_all(directory.path().join("Bokheim/Libraries/default/.bokheim")).unwrap();
        fs::create_dir_all(directory.path().join(".adds/bokheim/data")).unwrap();
        fs::write(directory.path().join("Bokheim/Libraries/default/book.epub"), b"book data").unwrap();
        fs::write(directory.path().join("Bokheim/Libraries/default/.bokheim/library.db"), b"database").unwrap();
        fs::write(directory.path().join(".adds/bokheim/reader_preferences.json"), b"preferences").unwrap();
        fs::write(directory.path().join(".adds/bokheim/data/canvas-mode"), b"canvas mode").unwrap();
        fs::write(directory.path().join(".adds/bokheim/app-state-v1.sqlite3"), b"application database").unwrap();

        let mut updates = Vec::new();
        let summary = install_archive_with_progress(directory.path(), &test_archive(None), |progress| updates.push(progress)).unwrap();
        let last = updates.last().copied().unwrap();
        assert_eq!(last.copied_bytes, summary.bytes);
        assert_eq!(last.total_bytes, summary.bytes);
        assert!(updates.windows(2).all(|pair| pair[0].copied_bytes <= pair[1].copied_bytes));

        assert_eq!(fs::read(directory.path().join("Bokheim/Libraries/default/book.epub")).unwrap(), b"book data");
        assert_eq!(fs::read(directory.path().join("Bokheim/Libraries/default/.bokheim/library.db")).unwrap(), b"database");
        assert_eq!(fs::read(directory.path().join(".adds/bokheim/reader_preferences.json")).unwrap(), b"preferences");
        assert_eq!(fs::read(directory.path().join(".adds/bokheim/data/canvas-mode")).unwrap(), b"canvas mode");
        assert_eq!(fs::read(directory.path().join(".adds/bokheim/app-state-v1.sqlite3")).unwrap(), b"application database");
        assert_eq!(fs::read(directory.path().join(".kobo/KoboRoot.tgz")).unwrap(), NICKEL_MENU_ARCHIVE);
        assert_eq!(fs::read(directory.path().join(NICKEL_MENU_CONFIGURATION_PATH)).unwrap(), NICKEL_MENU_CONFIGURATION);
        for (path, bytes) in NICKEL_MENU_ICONS {
            assert_eq!(fs::read(directory.path().join(path)).unwrap(), *bytes);
        }
    }

    #[test]
    fn installs_declared_profiles_and_rejects_wrong_packages_before_writing() {
        let ids = kobo_profile::PROFILES.iter().map(|profile| profile.id.to_string()).collect::<Vec<_>>().join(",");
        let info = format!("CPU profile: generic\nSupported model IDs: {ids}\n");
        let archive = test_archive_for_models(None, Some(&info));
        for profile in kobo_profile::PROFILES {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir(root.path().join(".kobo")).unwrap();
            fs::write(root.path().join(".kobo/version"), format!("serial,kernel,4.38.1,x,x,{}", profile.id)).unwrap();
            if profile.id != 388 {
                assert!(install_archive_with_progress(root.path(), &test_archive(None), |_| panic!("incompatible package started copying")).is_err());
                assert!(!root.path().join(".adds").exists());
            }
            install_archive_with_progress(root.path(), &archive, |_| {}).unwrap();
            assert!(root.path().join(".adds/bokheim/desktop-gpui-kobo").is_file());
        }
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".kobo")).unwrap();
        fs::write(root.path().join(".kobo/version"), "serial,kernel,4.38.1,x,x,999").unwrap();
        assert!(install_archive_with_progress(root.path(), &archive, |_| panic!("unknown model started copying")).is_err());
        assert!(!root.path().join(".adds").exists());
    }

    #[test]
    fn a_pending_firmware_update_is_preserved_before_any_installation_writes() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join(".kobo")).unwrap();
        fs::write(directory.path().join(".kobo/version"), "serial,4.1.15,4.38.23828,unused,unused,0000000000388").unwrap();
        fs::write(directory.path().join(".kobo/KoboRoot.tgz"), b"firmware update").unwrap();
        assert!(install_archive_with_progress(directory.path(), &test_archive(None), |_| {}).is_err());
        assert_eq!(fs::read(directory.path().join(".kobo/KoboRoot.tgz")).unwrap(), b"firmware update");
        assert!(!directory.path().join(".adds").exists());
    }

    #[test]
    fn an_archive_cannot_overwrite_application_state_or_its_journal() {
        for name in ["app-state-v1.sqlite3", "app-state-v1.sqlite3-wal", "app-state-v1.sqlite3-shm"] {
            assert!(validate_archive(&test_archive(Some(&format!(".adds/bokheim/{name}")))).is_err());
        }
    }

    #[test]
    fn case_variants_cannot_overwrite_protected_files_or_directories() {
        for path in [
            ".adds/bokheim/DATA/user.db",
            ".adds/BOKHEIM/Libraries/book.epub",
            ".adds/bokheim/REMOTE-BOOK-CACHE/book.epub",
            ".adds/bokheim/Taxonomy/categories.db",
            ".adds/bokheim/APP-STATE-V1.SQLITE3",
            ".adds/bokheim/App-State-V1.sqlite3-WAL",
            ".adds/bokheim/READER_PREFERENCES.JSON",
            "Bokheim/LIBRARIES/default/book.epub",
        ] {
            assert!(validate_archive(&test_archive(Some(path))).is_err(), "accepted protected path: {path}");
        }
        for path in [".adds/bokheim/database/helper", ".adds/bokheim/data-backup-tool", "Bokheim/Libraries-helper"] {
            assert!(sanitize_package_path(Path::new(path), EntryType::Regular).is_ok(), "rejected unrelated path: {path}");
        }
    }

    #[test]
    fn a_case_alias_fails_before_any_installation_writes() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join(".kobo")).unwrap();
        fs::write(directory.path().join(".kobo/version"), "serial,4.1.15,4.38.23828,unused,unused,0000000000388").unwrap();
        fs::create_dir_all(directory.path().join(".adds/bokheim/data")).unwrap();
        let protected = directory.path().join(".adds/bokheim/data/user.db");
        fs::write(&protected, b"user data").unwrap();
        let mut progress_called = false;
        let result = install_archive_with_progress(directory.path(), &test_archive(Some(".adds/bokheim/DATA/user.db")), |_| progress_called = true);
        assert!(result.is_err());
        assert!(!progress_called);
        assert_eq!(fs::read(protected).unwrap(), b"user data");
        assert!(!directory.path().join(REQUIRED_INSTALLED_FILES[0]).exists());
    }

    #[test]
    fn the_library_root_must_be_a_directory_entry() {
        for path in ["Bokheim/Libraries", "Bokheim/LIBRARIES"] {
            assert!(validate_archive(&test_archive(Some(path))).is_err(), "accepted a file at the library root: {path}");
            assert!(validate_archive(&test_archive_with_entry(Some((path, EntryType::Directory)))).is_ok());
        }
    }

    #[test]
    fn file_entries_cannot_displace_existing_directories_before_installation() {
        for path in ["Bokheim/Libraries", "Bokheim/LIBRARIES", ".adds/bokheim/existing-directory"] {
            let directory = tempfile::tempdir().unwrap();
            fs::create_dir_all(directory.path().join(".kobo")).unwrap();
            fs::write(directory.path().join(".kobo/version"), "serial,4.1.15,4.38.23828,unused,unused,0000000000388").unwrap();
            fs::create_dir_all(directory.path().join("Bokheim/Libraries/default")).unwrap();
            let book = directory.path().join("Bokheim/Libraries/default/book.epub");
            fs::write(&book, b"book data").unwrap();
            let existing = directory.path().join(".adds/bokheim/existing-directory");
            fs::create_dir_all(&existing).unwrap();
            fs::write(existing.join("user.db"), b"user data").unwrap();
            let mut progress_called = false;
            let error = install_archive_with_progress(directory.path(), &test_archive(Some(path)), |_| progress_called = true).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(!progress_called, "installation started before rejecting {path}");
            assert_eq!(fs::read(book).unwrap(), b"book data");
            assert_eq!(fs::read(existing.join("user.db")).unwrap(), b"user data");
            assert!(!directory.path().join(REQUIRED_INSTALLED_FILES[0]).exists());
            assert_eq!(fs::read_dir(directory.path().join("Bokheim")).unwrap().count(), 1, "library directory was moved to a backup");
        }
    }

    #[test]
    fn a_library_root_directory_entry_preserves_existing_books() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join(".kobo")).unwrap();
        fs::write(directory.path().join(".kobo/version"), "serial,4.1.15,4.38.23828,unused,unused,0000000000388").unwrap();
        fs::create_dir_all(directory.path().join("Bokheim/Libraries/default")).unwrap();
        let book = directory.path().join("Bokheim/Libraries/default/book.epub");
        fs::write(&book, b"book data").unwrap();
        let archive = test_archive_with_entry(Some(("Bokheim/Libraries", EntryType::Directory)));
        let summary = install_archive_with_progress(directory.path(), &archive, |_| {}).unwrap();
        assert_eq!(summary.directories, 1);
        assert_eq!(fs::read(book).unwrap(), b"book data");
    }

    #[test]
    fn file_replacement_preserves_a_directory_created_while_staging() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("asset");
        let error = install_file_atomically(&mut Cursor::new(b"package bytes"), &destination, &mut |_| {
            fs::create_dir(&destination).unwrap();
            fs::write(destination.join("user.db"), b"user data").unwrap();
        }).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(destination.join("user.db")).unwrap(), b"user data");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1, "staging or backup files were left behind");
    }

    #[test]
    fn unsupported_firmware_fails_before_installation_writes() {
        for version in ["4.5.0", "5.0.0", ""] {
            let directory = tempfile::tempdir().unwrap();
            fs::create_dir(directory.path().join(".kobo")).unwrap();
            fs::write(directory.path().join(".kobo/version"), format!("serial,kernel,{version}")).unwrap();
            assert!(install_archive_with_progress(directory.path(), &test_archive(None), |_| {}).is_err());
            assert!(!directory.path().join(".adds").exists());
        }
    }

}
