//! Recover folder identity before reopening device-owned library resources.

use super::{platform, LibraryEntry, LocalRegistry, RegistryResult};
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use sync_common::LibraryId;

pub enum LibraryRootRecovery {
    Unchanged(LibraryEntry),
    Recovered(LibraryEntry),
}

fn identity_key(id: &LibraryId) -> String {
    format!("library-root-storage:{id}")
}

fn pending_key(id: &LibraryId) -> String {
    format!("library-root-recovery:{id}")
}

fn storage_identity(root: &Path) -> RegistryResult<Option<Vec<u8>>> {
    match fs::read(root.join(".bokheim/storage-identity")) {
        Ok(bytes) if !bytes.is_empty() => Ok(Some(bytes)),
        Ok(_) => Err("library storage identity is empty".into()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

impl LocalRegistry {
    pub fn remember_library_root(&mut self, id: &LibraryId) -> RegistryResult<()> {
        let entry = self.entry(id)?.ok_or("library not found")?;
        let identity = storage_identity(Path::new(entry.storage_locator()))?.ok_or("library storage identity is missing")?;
        self.write_value(&identity_key(id), &identity)?;
        self.remove_value(&pending_key(id))
    }

    pub fn recover_library_root(&mut self, id: &LibraryId) -> RegistryResult<LibraryRootRecovery> {
        let entry = self.entry(id)?.ok_or("library not found")?;
        if self.is_detached(id)? {
            return Err("library is pending deletion".into());
        }
        let result = self.recover_registered_root(id, entry);
        if result.is_err() {
            // Clearing local availability after a failed repair also requires
            // reopening when the original mount returns unchanged.
            self.write_value(&pending_key(id), b"1")?;
        }
        result
    }

    fn recover_registered_root(&mut self, id: &LibraryId, entry: LibraryEntry) -> RegistryResult<LibraryRootRecovery> {
        let root = Path::new(entry.storage_locator());
        let manifest = platform::read_manifest(root)?;
        let previous = self.read_value(&identity_key(id))?;
        let identity = storage_identity(root)?;
        let intact = manifest.as_ref().is_some_and(|(found, _)| found == id) && root.join(".bokheim/library.json").is_file() && (previous.is_none() || identity.is_some());
        if intact {
            let changed = self.read_value(&pending_key(id))?.is_some() || (previous.is_some() && previous != identity);
            if changed {
                self.write_value(&pending_key(id), b"1")?;
            }
            return Ok(if changed { LibraryRootRecovery::Recovered(entry) } else { LibraryRootRecovery::Unchanged(entry) });
        }

        // Search the configured library parent and parents already selected by
        // this device. Never create search directories or recursively walk disks.
        let mut parents = HashSet::from([PathBuf::from(self.effective_library_root()?)]);
        for registered in self.entries()? {
            if let Some(parent) = Path::new(registered.storage_locator()).parent() {
                parents.insert(parent.to_owned());
            }
        }
        let mut found = HashSet::new();
        for parent in parents {
            let entries = match fs::read_dir(parent) {
                Ok(entries) => entries,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            for candidate in entries {
                let candidate = candidate?;
                if !candidate.file_type()?.is_dir() {
                    continue;
                }
                let path = candidate.path();
                if path == root {
                    continue;
                }
                if platform::read_manifest(&path).ok().flatten().is_some_and(|(found, _)| found == *id) {
                    found.insert(path.canonicalize()?);
                }
            }
        }
        if found.len() > 1 {
            return Err("multiple folders have this library identity".into());
        }
        let locator = found.into_iter().next().map(|path| path.to_string_lossy().into_owned()).unwrap_or_else(|| entry.storage_locator().to_owned());
        // This also refuses to overwrite a different library at the saved path.
        let recovered = platform::prepare_attached_library(*id, entry.library_name(), locator)?;
        self.connection.execute("UPDATE library_registry SET storage_path=?2 WHERE library_id=?1", (id.to_string(), recovered.storage_locator()))?;
        self.write_value(&pending_key(id), b"1")?;
        Ok(LibraryRootRecovery::Recovered(recovered))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{read_library_manifest, LibraryRegistry};

    fn fixture() -> (tempfile::TempDir, LibraryRegistry, LibraryEntry) {
        let directory = tempfile::tempdir().unwrap();
        let registry = LibraryRegistry::open(directory.path().join("app").to_string_lossy()).unwrap();
        let entry = registry.create_with_asset_storage("Books", Some(directory.path().join("mount/books").to_string_lossy().into_owned()), false).unwrap();
        fs::write(Path::new(entry.storage_locator()).join(".bokheim/storage-identity"), b"original").unwrap();
        registry.remember_library_root(entry.library_id()).unwrap();
        (directory, registry, entry)
    }

    #[test]
    fn missing_root_recreates_only_metadata_with_the_existing_identity() {
        let (_directory, registry, entry) = fixture();
        let root = Path::new(entry.storage_locator());
        fs::remove_dir_all(root).unwrap();
        let LibraryRootRecovery::Recovered(recovered) = registry.recover_library_root(entry.library_id()).unwrap() else { panic!("expected recovery") };
        assert_eq!(recovered.library_id(), entry.library_id());
        assert_eq!(recovered.storage_locator(), entry.storage_locator());
        assert_eq!(read_library_manifest(root).unwrap().unwrap().0, *entry.library_id());
        assert_eq!(fs::read_dir(root).unwrap().count(), 2);
        assert_eq!(fs::read_dir(root.join(".bokheim")).unwrap().count(), 1);
        assert!(matches!(registry.recover_library_root(entry.library_id()).unwrap(), LibraryRootRecovery::Recovered(_)), "recovery stays pending until resources open successfully");
    }

    #[test]
    fn discovery_relocates_the_same_library_before_recreating_its_old_path() {
        let (_directory, registry, entry) = fixture();
        let old = Path::new(entry.storage_locator());
        let moved = old.with_file_name("moved");
        fs::write(old.join("book.epub"), b"keep book").unwrap();
        fs::rename(old, &moved).unwrap();
        let LibraryRootRecovery::Recovered(recovered) = registry.recover_library_root(entry.library_id()).unwrap() else { panic!("expected relocation") };
        assert_eq!(Path::new(recovered.storage_locator()), moved);
        assert!(!old.exists());
        assert_eq!(fs::read(moved.join("book.epub")).unwrap(), b"keep book");
        assert!(!registry.asset_storage_enabled(entry.library_id()).unwrap());
        registry.remember_library_root(entry.library_id()).unwrap();
        assert!(matches!(registry.recover_library_root(entry.library_id()).unwrap(), LibraryRootRecovery::Unchanged(_)));
    }

    #[test]
    fn empty_mountpoint_and_missing_hidden_metadata_are_repaired() {
        let (_directory, registry, entry) = fixture();
        let root = Path::new(entry.storage_locator());
        fs::remove_dir_all(root.join(".bokheim")).unwrap();
        assert!(matches!(registry.recover_library_root(entry.library_id()).unwrap(), LibraryRootRecovery::Recovered(_)));
        assert_eq!(read_library_manifest(root).unwrap().unwrap().0, *entry.library_id());
        fs::write(root.join(".bokheim/storage-identity"), b"recreated").unwrap();
        registry.remember_library_root(entry.library_id()).unwrap();
        assert!(matches!(registry.recover_library_root(entry.library_id()).unwrap(), LibraryRootRecovery::Unchanged(_)));
        fs::remove_dir_all(root).unwrap();
        fs::create_dir(root).unwrap();
        assert!(matches!(registry.recover_library_root(entry.library_id()).unwrap(), LibraryRootRecovery::Recovered(_)));
    }

    #[test]
    fn returning_mount_with_the_same_library_and_another_storage_marker_reopens() {
        let (_directory, registry, entry) = fixture();
        let marker = Path::new(entry.storage_locator()).join(".bokheim/storage-identity");
        fs::write(&marker, b"underlying disk").unwrap();
        assert!(matches!(registry.recover_library_root(entry.library_id()).unwrap(), LibraryRootRecovery::Recovered(_)));
        registry.remember_library_root(entry.library_id()).unwrap();
        fs::write(&marker, b"original").unwrap();
        assert!(matches!(registry.recover_library_root(entry.library_id()).unwrap(), LibraryRootRecovery::Recovered(_)));
        registry.remember_library_root(entry.library_id()).unwrap();
        assert!(matches!(registry.recover_library_root(entry.library_id()).unwrap(), LibraryRootRecovery::Unchanged(_)));
    }

    #[test]
    fn recovery_does_not_overwrite_a_different_library() {
        let (directory, registry, entry) = fixture();
        fs::remove_dir_all(entry.storage_locator()).unwrap();
        let other = LibraryRegistry::open(directory.path().join("other-app").to_string_lossy()).unwrap();
        let replacement = other.create("Other", Some(entry.storage_locator().to_owned())).unwrap();
        assert!(registry.recover_library_root(entry.library_id()).is_err());
        assert_eq!(read_library_manifest(Path::new(entry.storage_locator())).unwrap().unwrap().0, *replacement.library_id());
    }

    #[test]
    fn a_failed_recovery_requires_reopening_when_the_original_marker_returns() {
        let (_directory, registry, entry) = fixture();
        let marker = Path::new(entry.storage_locator()).join(".bokheim/storage-identity");
        fs::write(&marker, b"").unwrap();
        assert!(registry.recover_library_root(entry.library_id()).is_err());
        fs::write(&marker, b"original").unwrap();
        assert!(matches!(registry.recover_library_root(entry.library_id()).unwrap(), LibraryRootRecovery::Recovered(_)));
        registry.remember_library_root(entry.library_id()).unwrap();
        assert!(matches!(registry.recover_library_root(entry.library_id()).unwrap(), LibraryRootRecovery::Unchanged(_)));
    }
}
