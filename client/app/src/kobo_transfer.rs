//! Registry and manifest policy for transferring libraries to a mounted Kobo.
use client_platform_kobo::KoboDevice;
use library_registry::{read_library_manifest, LibraryEntry, LIBRARY_MANIFEST_FILENAME};
use std::{fs, io, path::Path};

const APP_HIDDEN_DIR: &str = ".bokheim";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KoboTransferSummary {
    pub libraries: usize,
    pub copied_files: usize,
    pub unchanged_files: usize,
}

pub fn transfer_libraries_to_kobo(device: &KoboDevice, libraries: &[LibraryEntry]) -> io::Result<KoboTransferSummary> {
    let libraries_dir = device.prepare_libraries_dir()?;

    let mut summary = KoboTransferSummary::default();
    for library in libraries {
        transfer_library(library, &libraries_dir, &mut summary)?;
        summary.libraries += 1;
    }
    Ok(summary)
}

fn transfer_library(library: &LibraryEntry, libraries_dir: &Path, summary: &mut KoboTransferSummary) -> io::Result<()> {
    let source = Path::new(library.storage_locator()).canonicalize()?;
    let source_manifest = read_library_manifest(&source).map_err(invalid_data)?.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("library has no {LIBRARY_MANIFEST_FILENAME}: {}", source.display())))?;
    if &source_manifest.0 != library.library_id() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("library manifest identifies {}, but the registry identifies {}", source_manifest.0, library.library_id())));
    }

    let destination = libraries_dir.join(library.library_id().to_string());
    fs::create_dir_all(&destination)?;
    let destination = destination.canonicalize()?;
    if !destination.starts_with(libraries_dir) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "library destination escapes the Kobo libraries directory"));
    }
    if source == destination {
        return Ok(());
    }
    if let Some((destination_id, _)) = read_library_manifest(&destination).map_err(invalid_data)? {
        if destination_id != *library.library_id() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("destination {} belongs to library {}", destination.display(), destination_id)));
        }
    }

    copy_library_tree(&source, &destination, true, summary)?;
    copy_file_if_changed(&source.join(LIBRARY_MANIFEST_FILENAME), &destination.join(LIBRARY_MANIFEST_FILENAME), summary)?;
    Ok(())
}

fn copy_library_tree(source: &Path, destination: &Path, is_root: bool, summary: &mut KoboTransferSummary) -> io::Result<()> {
    for child in fs::read_dir(source)? {
        let child = child?;
        let name = child.file_name();
        if is_root && (name == APP_HIDDEN_DIR || name == LIBRARY_MANIFEST_FILENAME) {
            continue;
        }
        let file_type = child.file_type()?;
        if file_type.is_symlink() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("refusing to transfer symbolic link {}", child.path().display())));
        }
        let target = destination.join(&name);
        if file_type.is_dir() {
            fs::create_dir_all(&target)?;
            copy_library_tree(&child.path(), &target, false, summary)?;
        } else if file_type.is_file() {
            copy_file_if_changed(&child.path(), &target, summary)?;
        }
    }
    Ok(())
}

fn invalid_data(error: Box<dyn std::error::Error + Send + Sync>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn copy_file_if_changed(source: &Path, destination: &Path, summary: &mut KoboTransferSummary) -> io::Result<()> {
    if client_platform_kobo::copy_file_if_changed(source, destination)? {
        summary.copied_files += 1;
    } else {
        summary.unchanged_files += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_registry::LibraryRegistry;

    fn fixture() -> (tempfile::TempDir, LibraryRegistry, LibraryEntry, KoboDevice) {
        let temp = tempfile::tempdir().unwrap();
        let registry = LibraryRegistry::open(temp.path().join("state").to_string_lossy()).unwrap();
        let entry = registry.create("Books", None).unwrap();
        let mount = temp.path().join("kobo");
        fs::create_dir_all(mount.join(".kobo")).unwrap();
        let device = KoboDevice::from_mount(&mount).unwrap();
        (temp, registry, entry, device)
    }

    #[test]
    fn transfers_content_and_manifest_but_skips_local_state_and_unchanged_files() {
        let (_temp, _registry, entry, device) = fixture();
        let source = Path::new(entry.storage_locator());
        fs::create_dir_all(source.join("Shelf")).unwrap();
        fs::write(source.join("Shelf/book.epub"), b"first").unwrap();
        fs::create_dir_all(source.join(APP_HIDDEN_DIR)).unwrap();
        fs::write(source.join(APP_HIDDEN_DIR).join("library.db"), b"local state").unwrap();
        let summary = transfer_libraries_to_kobo(&device, &[entry.clone()]).unwrap();
        assert_eq!(summary, KoboTransferSummary { libraries: 1, copied_files: 2, unchanged_files: 0 });
        let destination = device.libraries_dir().join(entry.library_id().to_string());
        assert_eq!(fs::read(destination.join("Shelf/book.epub")).unwrap(), b"first");
        assert!(!destination.join(APP_HIDDEN_DIR).exists());
        assert_eq!(read_library_manifest(&destination).unwrap().unwrap().0, *entry.library_id());

        let summary = transfer_libraries_to_kobo(&device, &[entry.clone()]).unwrap();
        assert_eq!(summary, KoboTransferSummary { libraries: 1, copied_files: 0, unchanged_files: 2 });
        fs::write(source.join("Shelf/book.epub"), b"other").unwrap();
        let summary = transfer_libraries_to_kobo(&device, &[entry]).unwrap();
        assert_eq!(summary, KoboTransferSummary { libraries: 1, copied_files: 1, unchanged_files: 1 });
        assert_eq!(fs::read(destination.join("Shelf/book.epub")).unwrap(), b"other");
    }

    #[test]
    fn rejects_a_source_manifest_that_disagrees_with_the_registry() {
        let (_temp, registry, entry, device) = fixture();
        let other = registry.create("Other", None).unwrap();
        fs::copy(Path::new(other.storage_locator()).join(LIBRARY_MANIFEST_FILENAME), Path::new(entry.storage_locator()).join(LIBRARY_MANIFEST_FILENAME)).unwrap();
        let error = transfer_libraries_to_kobo(&device, &[entry.clone()]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!device.libraries_dir().join(entry.library_id().to_string()).exists());
    }

    #[test]
    fn rejects_a_destination_belonging_to_another_library() {
        let (_temp, registry, entry, device) = fixture();
        let other = registry.create("Other", None).unwrap();
        let destination = device.libraries_dir().join(entry.library_id().to_string());
        fs::create_dir_all(&destination).unwrap();
        fs::copy(Path::new(other.storage_locator()).join(LIBRARY_MANIFEST_FILENAME), destination.join(LIBRARY_MANIFEST_FILENAME)).unwrap();
        let error = transfer_libraries_to_kobo(&device, &[entry]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(read_library_manifest(&destination).unwrap().unwrap().0, *other.library_id());
    }

    #[test]
    #[cfg(unix)]
    fn failed_content_transfer_does_not_publish_a_manifest() {
        let (temp, _registry, entry, device) = fixture();
        let outside = temp.path().join("outside.epub");
        fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, Path::new(entry.storage_locator()).join("linked.epub")).unwrap();
        let destination = device.libraries_dir().join(entry.library_id().to_string());
        assert_eq!(transfer_libraries_to_kobo(&device, &[entry]).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(!destination.join(LIBRARY_MANIFEST_FILENAME).exists());
    }

    #[test]
    fn rejects_a_disconnected_device() {
        let (_temp, _registry, entry, device) = fixture();
        fs::remove_dir(device.root().join(".kobo")).unwrap();
        assert_eq!(transfer_libraries_to_kobo(&device, &[entry]).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    #[cfg(unix)]
    fn rejects_a_destination_outside_the_mount() {
        let (temp, _registry, entry, device) = fixture();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, device.root().join("Bokheim")).unwrap();
        assert_eq!(transfer_libraries_to_kobo(&device, &[entry]).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read_dir(outside.join("Libraries")).unwrap().count(), 0);
    }
}
