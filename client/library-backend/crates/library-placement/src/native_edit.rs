//! Interactive placement I/O. Slow preparation never holds SQLite's writer.
use crate::LibraryPlacement;
use client_runtime::BackendError;
use library_database::{executor::run_blocking, DatabaseError, NativePlacementEdit, NativePlacementPlan, NativePlacementResult};
use library_files::asset_store::{PreparedDirectory, PreparedPlacement};
use library_files::AssetStore;
use std::error::Error;
use std::io::ErrorKind;
use sync_common::{BookId, DirId};

struct PreparedEdit {
    directories: Vec<PreparedDirectory>,
    files: Vec<PreparedPlacement>,
    materialized_files: Vec<String>,
    materialized_directories: Vec<DirId>,
}

impl PreparedEdit {
    fn prepare(assets: &AssetStore, plan: &NativePlacementPlan, copy_move: bool) -> Result<Self, BackendError> {
        if !assets.root_available() {
            return Err(BackendError::message("library storage is unavailable"));
        }
        let mut prepared = Self { directories: Vec::new(), files: Vec::new(), materialized_files: Vec::new(), materialized_directories: Vec::new() };
        if let NativePlacementResult::CopiedDirectory(id) = plan.result {
            let root = plan.directories.iter().find(|directory| directory.id == id).ok_or("copied folder has no filesystem path")?;
            let directories = plan.directories.iter().map(|directory| (directory.id, directory.relative_path.clone())).collect::<Vec<_>>();
            let books = plan.files.iter().filter(|file| file.work.operation == "materialize").map(|file| (file.work.content_hash, file.resolved_path.clone(), file.book_paths.clone())).collect::<Vec<_>>();
            let (tree, files) = assets.prepare_directory_copy(&root.relative_path, &directories, &books)?;
            prepared.files.push(tree);
            prepared.materialized_files = files;
            prepared.materialized_directories.extend(directories.into_iter().map(|(id, _)| id));
            return Ok(prepared);
        }
        for directory in &plan.directories {
            prepared.directories.push(assets.inspect_directory(directory.id, &directory.relative_path, directory.projected_path.as_deref(), &directory.name, &directory.sibling_names)?);
            prepared.materialized_directories.push(directory.id);
        }
        for file in plan.files.iter().filter(|file| file.work.operation == "materialize") {
            let movable =
                if copy_move { Vec::new() } else { plan.files.iter().filter(|other| other.work.operation == "remove" && other.work.content_hash == file.work.content_hash).map(|other| other.work.original_path.clone()).collect::<Vec<_>>() };
            if let Some(output) = assets.prepare_placement(&file.work.content_hash, &file.resolved_path, &file.book_paths, &movable)? {
                prepared.files.push(output);
                prepared.materialized_files.push(file.resolved_path.to_string());
            }
        }
        Ok(prepared)
    }

    fn publish(self) -> Result<(), DatabaseError> {
        for directory in self.directories {
            match directory.publish_exact().map_err(DatabaseError::operation)? {
                Some(result) if result.renamed_to.is_none() => {}
                _ => return Err(DatabaseError::message("destination folder changed during the operation; please try again")),
            }
        }
        for file in self.files {
            file.publish_short().map_err(DatabaseError::operation)?;
        }
        Ok(())
    }
}

fn crosses_devices(mut error: &(dyn Error + 'static)) -> bool {
    loop {
        if error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == ErrorKind::CrossesDevices) {
            return true;
        }
        let Some(source) = error.source() else { return false };
        error = source;
    }
}

impl LibraryPlacement {
    async fn edit_native(&self, edit: NativePlacementEdit) -> Result<NativePlacementResult, BackendError> {
        let mut plan = self.database.plan_native_placement(edit)?;
        // Leases coordinate the source bytes with background eviction and
        // replay while the actor remains available for queries and other work.
        let mut hashes = plan.files.iter().map(|file| file.work.content_hash).collect::<Vec<_>>();
        hashes.sort();
        hashes.dedup();
        let mut leases = Vec::new();
        for hash in hashes {
            let copying = matches!(plan.result, NativePlacementResult::CopiedBook(_) | NativePlacementResult::CopiedDirectory(_) | NativePlacementResult::RestoredBookPlacement);
            leases.push(if copying { self.assets.lease_book(&hash).await? } else { self.assets.try_exclusive_lease(&hash)?.ok_or("book is currently in use; please try again")? });
        }
        for copy_move in [false, true] {
            let assets = self.assets.clone();
            let snapshot = plan.clone();
            let prepared = run_blocking(move || PreparedEdit::prepare(&assets, &snapshot, copy_move)).await.map_err(BackendError::operation)??;
            if !prepared.files.iter().any(PreparedPlacement::renames_source) {
                if let Some(source) = plan.move_source.clone() {
                    let assets = self.assets.clone();
                    let retirement = run_blocking(move || assets.prepare_move_source_staging(&source.book, &source.physical_path)).await.map_err(BackendError::operation)??;
                    if let Some((retirement, staged)) = retirement {
                        self.database.record_native_move_staging(&mut plan, &staged)?;
                        // If publication or the final commit fails, the existing
                        // durable replay jobs restore the source instead.
                        self.events.files.wake();
                        retirement.publish_short()?;
                    }
                }
            }
            let files = prepared.materialized_files.clone();
            let directories = prepared.materialized_directories.clone();
            match self.database.commit_native_placement(plan.clone(), &files, &directories, || {
                if !self.assets.root_available() {
                    return Err(DatabaseError::message("library storage is unavailable"));
                }
                prepared.publish()
            }) {
                Err(error) if !copy_move && crosses_devices(&error) && matches!(plan.result, NativePlacementResult::TransferredBook { .. }) => continue,
                result => {
                    let result = result?;
                    self.events.notify_contents_changed();
                    return Ok(result);
                }
            }
        }
        unreachable!("second attempt returns its result")
    }

    pub async fn copy_directory(&self, source: &DirId, destination: &DirId) -> Result<DirId, BackendError> {
        if !self.assets.local_access_enabled() || self.database.storage_policy()?.external().is_some() {
            return self.copy_directory_metadata(source, destination);
        }
        match self.edit_native(NativePlacementEdit::CopyDirectory { source: *source, destination: *destination }).await? {
            NativePlacementResult::CopiedDirectory(id) => Ok(id),
            _ => unreachable!(),
        }
    }

    pub async fn copy_book_to_directory(&self, book: &BookId, source: Option<&DirId>, destination: &DirId) -> Result<bool, BackendError> {
        if !self.assets.local_access_enabled() || self.database.storage_policy()?.external().is_some() {
            return self.copy_book_to_directory_metadata(book, source, destination);
        }
        match self.edit_native(NativePlacementEdit::CopyBook { book: *book, source: source.copied(), destination: *destination }).await? {
            NativePlacementResult::CopiedBook(created) => Ok(created),
            _ => unreachable!(),
        }
    }

    pub async fn restore_book_placement(&self, book: &BookId, destination: &DirId) -> Result<(), BackendError> {
        if !self.assets.local_access_enabled() || self.database.storage_policy()?.external().is_some() {
            return self.restore_book_placement_metadata(book, destination);
        }
        self.edit_native(NativePlacementEdit::RestoreBookPlacement { book: *book, destination: *destination }).await?;
        Ok(())
    }

    pub async fn move_book_placement(&self, book: &BookId, source: &DirId, destination: &DirId) -> Result<bool, BackendError> {
        if !self.assets.local_access_enabled() || self.database.storage_policy()?.external().is_some() {
            return self.move_book_placement_metadata(book, source, destination);
        }
        match self.edit_native(NativePlacementEdit::TransferBook { book: *book, source: *source, destination: *destination, moving: true }).await? {
            NativePlacementResult::TransferredBook { created, .. } => Ok(created),
            _ => unreachable!(),
        }
    }

    pub async fn queue_directory_move(&self, directory: &DirId, parent: Option<&DirId>, name: Option<&str>) -> Result<String, BackendError> {
        if !self.assets.local_access_enabled() || self.database.storage_policy()?.external().is_some() {
            return self.queue_directory_move_metadata(directory, parent, name);
        }
        let name = name.map(library_replica::filenames::validate_component).transpose()?;
        match self.edit_native(NativePlacementEdit::MoveDirectory { directory: *directory, parent: parent.copied(), name }).await? {
            NativePlacementResult::MovedDirectory(name) => Ok(name),
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_inspection::InspectedBook;
    use book_model::{BookDetails, BookFormat, BookRecord};
    use library_database::{Database, ImportCommit};
    use library_model::PublishedStagedBook;
    use library_runtime::{events::LibraryEventRouter, scheduler::SyncScheduler};
    use std::{fs, sync::Arc};
    use sync_common::ROOT_DIR_ID;
    use tempfile::TempDir;

    const BYTES: &[u8] = b"verified local book bytes";

    fn fixture() -> (TempDir, LibraryPlacement, BookId, DirId) {
        let root = TempDir::new().unwrap();
        let assets = AssetStore::open(root.path().to_str().unwrap()).unwrap();
        let database = Arc::new(Database::open(root.path().join("state.sqlite")).unwrap());
        database.initialize_library().unwrap();
        let scheduler = || SyncScheduler::new().0;
        let (events, _) = LibraryEventRouter::new(scheduler(), scheduler(), scheduler(), scheduler());
        let placement = LibraryPlacement { database, assets, events };
        let folder = placement.create_directory(&ROOT_DIR_ID, "Shelf").unwrap().id;
        let hash = BookId::from_bytes(*blake3::hash(BYTES).as_bytes());
        fs::write(root.path().join("book.epub"), BYTES).unwrap();
        placement
            .database
            .commit_import(ImportCommit {
                parent_id: ROOT_DIR_ID,
                file_name: "book.epub".into(),
                format: BookFormat::Epub,
                content_hash: hash,
                size_bytes: BYTES.len() as u64,
                inspection_version: 1,
                inspection: Some(InspectedBook { pdf: None, audiobook: None, metadata: BookRecord { metadata: BookDetails { title: "Book".into(), ..Default::default() }, description: String::new() }, toc: vec![] }),
                published: PublishedStagedBook { name: "book.epub".into(), relative_path: "/book.epub".into(), published: true, fingerprint: None },
                request_thumbnail: false,
                restore_paths: vec![],
            })
            .unwrap();
        (root, placement, hash, folder)
    }

    #[test]
    fn copy_staging_keeps_placement_unchanged_and_releases_database_writer() {
        let (root, placement, book, destination) = fixture();
        let original = placement.database.book_paths(&book).unwrap();
        let plan = placement.database.plan_native_placement(NativePlacementEdit::CopyBook { book, source: Some(ROOT_DIR_ID), destination }).unwrap();
        let staged = PreparedEdit::prepare(&placement.assets, &plan, false).unwrap();
        assert_eq!(placement.database.book_paths(&book).unwrap(), original);
        assert!(!root.path().join("Shelf/book.epub").exists());
        // An independent writer succeeds while copied bytes are staged.
        let other = Database::open(root.path().join("state.sqlite")).unwrap();
        other.create_directory(&ROOT_DIR_ID, &"Concurrent".into()).unwrap();
        assert!(placement.database.commit_native_placement(plan, &staged.materialized_files.clone(), &[], || staged.publish()).is_err());
        assert!(!root.path().join("Shelf/book.epub").exists());
        assert_eq!(fs::read(root.path().join("book.epub")).unwrap(), BYTES);
        assert!(placement.database.native_file_work().unwrap().is_empty());
    }

    #[tokio::test]
    async fn copy_publishes_verified_bytes_before_reporting_a_new_placement() {
        let (root, placement, book, destination) = fixture();
        assert!(placement.copy_book_to_directory(&book, Some(&ROOT_DIR_ID), &destination).await.unwrap());
        assert_eq!(fs::read(root.path().join("Shelf/book.epub")).unwrap(), BYTES);
        assert_eq!(fs::read(root.path().join("book.epub")).unwrap(), BYTES);
        assert!(placement.database.native_file_work().unwrap().is_empty());
        assert!(!placement.copy_book_to_directory(&book, Some(&ROOT_DIR_ID), &destination).await.unwrap());
    }

    #[tokio::test]
    async fn move_renames_before_committing_and_preserves_existing_destinations_for_undo() {
        let (root, placement, book, destination) = fixture();
        assert!(placement.move_book_placement(&book, &ROOT_DIR_ID, &destination).await.unwrap());
        assert!(!root.path().join("book.epub").exists());
        assert_eq!(fs::read(root.path().join("Shelf/book.epub")).unwrap(), BYTES);
        assert!(!placement.database.native_file_work().unwrap().iter().any(|work| work.operation == "materialize"));
        crate::run_file_jobs(&placement.assets, &placement.database).await.unwrap();
        assert!(placement.copy_book_to_directory(&book, Some(&destination), &ROOT_DIR_ID).await.unwrap());
        assert!(!placement.move_book_placement(&book, &ROOT_DIR_ID, &destination).await.unwrap());
        assert_eq!(fs::read(root.path().join("Shelf/book.epub")).unwrap(), BYTES);
    }

    #[tokio::test]
    async fn undo_of_a_move_restores_bytes_before_restoring_the_source_placement() {
        let (root, placement, book, destination) = fixture();
        placement.move_book_placement(&book, &ROOT_DIR_ID, &destination).await.unwrap();
        assert!(!root.path().join("book.epub").exists());
        placement.restore_book_placement(&book, &ROOT_DIR_ID).await.unwrap();
        placement.remove_book_from_directory(&book, &destination).unwrap();
        crate::run_file_jobs(&placement.assets, &placement.database).await.unwrap();
        assert_eq!(fs::read(root.path().join("book.epub")).unwrap(), BYTES);
        assert!(placement.database.native_file_work().unwrap().is_empty());
        assert!(placement.database.book_paths(&book).unwrap().iter().any(|path| path.as_str() == "/book.epub"));
    }

    #[tokio::test]
    async fn copied_tree_includes_nested_books_and_empty_directories() {
        let (root, placement, book, shelf) = fixture();
        let nested = placement.create_directory(&shelf, "Nested").unwrap().id;
        placement.create_directory(&nested, "Empty").unwrap();
        placement.copy_book_to_directory(&book, Some(&ROOT_DIR_ID), &nested).await.unwrap();
        let copy = placement.copy_directory(&shelf, &ROOT_DIR_ID).await.unwrap();
        let path = placement.database.native_directory_snapshot(&copy).unwrap().unwrap().relative_path;
        let directory = root.path().join(path.trim_start_matches('/'));
        assert_eq!(fs::read(directory.join("Nested/book.epub")).unwrap(), BYTES);
        assert!(directory.join("Nested/Empty").is_dir());
        assert_eq!(fs::read_to_string(directory.join(".biblos_uuid")).unwrap(), copy.to_string());
        assert!(placement.database.native_file_work().unwrap().is_empty());
    }

    #[tokio::test]
    async fn folder_rename_moves_unmanaged_contents_with_its_projection() {
        let (root, placement, book, shelf) = fixture();
        placement.copy_book_to_directory(&book, Some(&ROOT_DIR_ID), &shelf).await.unwrap();
        fs::write(root.path().join("Shelf/notes.txt"), b"keep me").unwrap();
        assert_eq!(placement.queue_directory_move(&shelf, None, Some("Renamed")).await.unwrap(), "Renamed");
        assert!(!root.path().join("Shelf").exists());
        assert_eq!(fs::read(root.path().join("Renamed/book.epub")).unwrap(), BYTES);
        assert_eq!(fs::read(root.path().join("Renamed/notes.txt")).unwrap(), b"keep me");
        assert!(placement.database.book_paths(&book).unwrap().iter().any(|path| path.as_str() == "/Renamed/book.epub"));
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_at_publication_rolls_back_the_placement() {
        use std::os::unix::fs::PermissionsExt;
        let (root, placement, book, destination) = fixture();
        let plan = placement.database.plan_native_placement(NativePlacementEdit::CopyBook { book, source: Some(ROOT_DIR_ID), destination }).unwrap();
        let staged = PreparedEdit::prepare(&placement.assets, &plan, false).unwrap();
        let original = placement.database.book_paths(&book).unwrap();
        let folder = root.path().join("Shelf");
        let permissions = fs::metadata(&folder).unwrap().permissions();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
        let result = placement.database.commit_native_placement(plan, &staged.materialized_files.clone(), &[], || staged.publish());
        fs::set_permissions(&folder, permissions).unwrap();
        assert!(result.is_err());
        assert_eq!(placement.database.book_paths(&book).unwrap(), original);
        assert!(placement.database.native_file_work().unwrap().is_empty());
        assert!(!folder.join("book.epub").exists());
        assert_eq!(fs::read(root.path().join("book.epub")).unwrap(), BYTES);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn denied_move_does_not_fall_back_to_copying_into_a_writable_destination() {
        use std::os::unix::fs::PermissionsExt;
        let (root, placement, book, destination) = fixture();
        let original = placement.database.book_paths(&book).unwrap();
        let permissions = fs::metadata(root.path()).unwrap().permissions();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let result = placement.move_book_placement(&book, &ROOT_DIR_ID, &destination).await;
        fs::set_permissions(root.path(), permissions).unwrap();
        assert!(result.is_err());
        assert_eq!(placement.database.book_paths(&book).unwrap(), original);
        assert_eq!(fs::read(root.path().join("book.epub")).unwrap(), BYTES);
        assert!(!root.path().join("Shelf/book.epub").exists());
    }

    #[test]
    fn reading_progress_can_change_while_a_copy_is_staged() {
        let (root, placement, book, destination) = fixture();
        let plan = placement.database.plan_native_placement(NativePlacementEdit::CopyBook { book, source: Some(ROOT_DIR_ID), destination }).unwrap();
        let staged = PreparedEdit::prepare(&placement.assets, &plan, false).unwrap();
        placement.database.update_reading_position(&book, "epubcfi(/6/2)", Some(42.0), None).unwrap();
        placement.database.commit_native_placement(plan, &staged.materialized_files.clone(), &[], || staged.publish()).unwrap();
        assert_eq!(fs::read(root.path().join("Shelf/book.epub")).unwrap(), BYTES);
        assert_eq!(placement.database.browse_book_detail(&book).unwrap().book.progress, 42.0);
    }

    #[tokio::test]
    async fn copy_completes_an_existing_replay_job_before_committing_its_placement() {
        let (root, placement, book, destination) = fixture();
        placement.database.copy_book_to_directory(&book, Some(&ROOT_DIR_ID), &destination).unwrap();
        placement.database.remove_book_placement(&book, &destination).unwrap();
        assert!(!placement.database.native_file_work().unwrap().is_empty());
        assert!(placement.copy_book_to_directory(&book, Some(&ROOT_DIR_ID), &destination).await.unwrap());
        assert_eq!(fs::read(root.path().join("Shelf/book.epub")).unwrap(), BYTES);
        assert!(!placement.database.native_file_work().unwrap().iter().any(|work| work.operation == "materialize"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cross_device_move_stages_the_copy_and_retires_source_after_commit() {
        use std::os::unix::{
            fs::{symlink, MetadataExt},
            prelude::PermissionsExt,
        };
        let (root, placement, book, destination) = fixture();
        let other = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        if fs::metadata(root.path()).unwrap().dev() == fs::metadata(other.path()).unwrap().dev() {
            return; // This host does not provide two writable filesystems.
        }
        // A symlink models a mounted destination without requiring mount privileges.
        fs::rename(root.path().join("Shelf/.biblos_uuid"), other.path().join(".biblos_uuid")).unwrap_or_else(|_| {
            fs::copy(root.path().join("Shelf/.biblos_uuid"), other.path().join(".biblos_uuid")).unwrap();
            fs::remove_file(root.path().join("Shelf/.biblos_uuid")).unwrap();
        });
        fs::remove_dir(root.path().join("Shelf")).unwrap();
        symlink(other.path(), root.path().join("Shelf")).unwrap();
        assert!(placement.move_book_placement(&book, &ROOT_DIR_ID, &destination).await.unwrap());
        assert_eq!(fs::read(other.path().join("book.epub")).unwrap(), BYTES);
        assert!(!root.path().join("book.epub").exists(), "the source is retired before committing the move");
        assert!(placement.database.native_file_work().unwrap().iter().any(|work| work.operation == "remove" && work.original_path.file_name().starts_with(".bokheim-move-")));
        crate::run_file_jobs(&placement.assets, &placement.database).await.unwrap();
        assert!(!root.path().join("book.epub").exists());
        // Permissions on the copy are ordinary file permissions, not a stored cache.
        assert_ne!(fs::metadata(other.path().join("book.epub")).unwrap().permissions().mode() & 0o400, 0);
    }

    #[tokio::test]
    async fn interrupted_long_move_restores_source_from_persisted_replay_work() {
        let (root, placement, book, destination) = fixture();
        let mut plan = placement.database.plan_native_placement(NativePlacementEdit::TransferBook { book, source: ROOT_DIR_ID, destination, moving: true }).unwrap();
        let copied = PreparedEdit::prepare(&placement.assets, &plan, true).unwrap();
        let source = plan.move_source.clone().unwrap();
        let (retirement, staged) = placement.assets.prepare_move_source_staging(&book, &source.physical_path).unwrap().unwrap();
        placement.database.record_native_move_staging(&mut plan, &staged).unwrap();
        retirement.publish_short().unwrap();
        assert!(!root.path().join("book.epub").exists());
        assert_eq!(fs::read(root.path().join(staged.as_str().trim_start_matches('/'))).unwrap(), BYTES);
        drop(copied); // The app stops before publishing or committing the move.
        let reopened = Database::open(root.path().join("state.sqlite")).unwrap();
        reopened.initialize_library().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = fs::metadata(root.path()).unwrap().permissions();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o555)).unwrap();
            let blocked = crate::run_file_jobs(&placement.assets, &reopened).await;
            fs::set_permissions(root.path(), permissions).unwrap();
            assert!(blocked.is_err());
            assert_eq!(fs::read(root.path().join(staged.as_str().trim_start_matches('/'))).unwrap(), BYTES);
            assert!(reopened.native_file_work().unwrap().iter().any(|work| work.operation == "remove" && work.original_path == staged));
        }
        crate::run_file_jobs(&placement.assets, &reopened).await.unwrap();
        assert_eq!(fs::read(root.path().join("book.epub")).unwrap(), BYTES);
        assert!(!root.path().join("Shelf/book.epub").exists());
        assert!(!root.path().join(staged.as_str().trim_start_matches('/')).exists());
        assert!(reopened.native_file_work().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn denied_publication_after_source_retirement_recovers_the_original_placement() {
        use std::os::unix::fs::PermissionsExt;
        let (root, placement, book, destination) = fixture();
        let original = placement.database.book_paths(&book).unwrap();
        let mut plan = placement.database.plan_native_placement(NativePlacementEdit::TransferBook { book, source: ROOT_DIR_ID, destination, moving: true }).unwrap();
        let copied = PreparedEdit::prepare(&placement.assets, &plan, true).unwrap();
        let source = plan.move_source.clone().unwrap();
        let (retirement, staged) = placement.assets.prepare_move_source_staging(&book, &source.physical_path).unwrap().unwrap();
        placement.database.record_native_move_staging(&mut plan, &staged).unwrap();
        retirement.publish_short().unwrap();
        let folder = root.path().join("Shelf");
        let permissions = fs::metadata(&folder).unwrap().permissions();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
        let result = placement.database.commit_native_placement(plan, &copied.materialized_files.clone(), &[], || copied.publish());
        fs::set_permissions(&folder, permissions).unwrap();
        assert!(result.is_err());
        crate::run_file_jobs(&placement.assets, &placement.database).await.unwrap();
        assert_eq!(fs::read(root.path().join("book.epub")).unwrap(), BYTES);
        assert!(!folder.join("book.epub").exists());
        assert_eq!(placement.database.book_paths(&book).unwrap(), original);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_background_materialization_keeps_the_source_until_destination_is_verified() {
        use std::os::unix::fs::PermissionsExt;
        let (root, placement, book, destination) = fixture();
        // Existing queued work, including work received through sync, can
        // encounter a read-only destination after its logical change commits.
        placement.database.transfer_book_placement(&book, &ROOT_DIR_ID, &destination, true).unwrap();
        let folder = root.path().join("Shelf");
        let permissions = fs::metadata(&folder).unwrap().permissions();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
        let failed = crate::run_file_jobs(&placement.assets, &placement.database).await;
        fs::set_permissions(&folder, permissions).unwrap();
        assert!(failed.is_err());
        assert_eq!(fs::read(root.path().join("book.epub")).unwrap(), BYTES);
        assert!(!folder.join("book.epub").exists());
        crate::run_file_jobs(&placement.assets, &placement.database).await.unwrap();
        assert_eq!(fs::read(folder.join("book.epub")).unwrap(), BYTES);
        assert!(!root.path().join("book.epub").exists());
    }
}
