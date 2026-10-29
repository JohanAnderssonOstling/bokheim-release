//! Place external files on disk, then import them in place.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Error};
use std::path::{Path, PathBuf};

use book_model::BookFormat;
use filesystem_scanner::filesystem_scan::is_audiobook_folder;
use filesystem_scanner::ingestion::{ingest_audiobook_folder, ingest_file};
use library_files::filesystem::{place_into_directory, walk};
use library_model::PublishedStagedBook;
use sync_common::DirId;

use client_platform_runtime::executor::run_blocking;
use client_runtime::BackendError;
use library_replica::filenames;

use crate::import::{validate_import_destination, ValidatedImport};
use crate::{DirectoryImport, ImportFailure, LibraryImport};

impl LibraryImport {
    pub async fn import_paths(&self, parent: DirId, paths: Vec<PathBuf>, moving: bool) -> Result<Vec<ImportFailure>, BackendError> {
        self.place_and_import_paths(parent, paths, moving).await
    }

    async fn import_destination(&self, parent: DirId) -> Result<PathBuf, BackendError> {
        validate_import_destination(&self.database, &parent)?;
        if !self.assets.local_access_enabled() {
            return Err(BackendError::message("library storage is unavailable"));
        }
        library_placement::prepare_directory(&self.assets, &self.database, parent).await?;
        validate_import_destination(&self.database, &parent)?;
        let root = PathBuf::from(&self.root).canonicalize()?;
        Ok(self.database.dir_path_components(&parent)?.into_iter().fold(root, |path, component| path.join(component.name.as_str())))
    }

    /// Reader-backed picker entries are copied into hidden staging first, then
    /// go through the same placement and in-place import as filesystem drops.
    pub async fn import_selected_directory(&self, parent: DirId, directory: DirectoryImport, create_root: bool, cancelled: &dyn Fn() -> bool) -> Result<Vec<ImportFailure>, BackendError> {
        let destination = self.import_destination(parent).await?;
        let staging = tempfile::Builder::new().prefix(".bokheim-copy-").tempdir_in(destination)?;
        let payload = if create_root { staging.path().join(filenames::validate_component(&directory.name)?) } else { staging.path().to_owned() };
        fs::create_dir_all(&payload)?;
        for path in directory.directories {
            fs::create_dir_all(selected_path(&payload, &path)?)?;
        }
        let mut failures = Vec::new();
        let mut failed_members = Vec::new();
        for file in directory.files {
            if cancelled() {
                return Err(BackendError::message("directory import cancelled"));
            }
            if !selected_folder_file_supported(&file.path) {
                continue;
            }
            let path = selected_path(&payload, &file.path)?;
            if path.try_exists()? {
                failures.push(ImportFailure::file(&file.path, "duplicate selected file path"));
                if audiobook_member(&path) {
                    failed_members.push(path);
                }
                continue;
            }
            let copied_path = path.clone();
            let result = async {
                let mut reader = file.source.open().await.map_err(BackendError::message)?;
                run_blocking(move || -> io::Result<()> {
                    fs::create_dir_all(copied_path.parent().ok_or_else(|| Error::other("selected file has no parent"))?)?;
                    let mut writer = fs::OpenOptions::new().write(true).create_new(true).open(copied_path)?;
                    io::copy(&mut reader, &mut writer)?;
                    Ok(())
                })
                .await
                .map_err(BackendError::operation)?
                .map_err(BackendError::operation)
            }
            .await;
            if let Err(error) = result {
                // A failed copy stays hidden; publish only completed entries.
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                failures.push(ImportFailure::file(&file.path, &error));
                if audiobook_member(&path) {
                    failed_members.push(path);
                }
            }
        }
        if cancelled() {
            return Err(BackendError::message("directory import cancelled"));
        }
        if !failed_members.is_empty() {
            let complete = run_blocking({
                let payload = payload.clone();
                move || discard_incomplete_audiobooks(&payload, &failed_members)
            })
            .await
            .map_err(BackendError::operation)??;
            if !complete {
                return Ok(failures);
            }
        }
        if cancelled() {
            return Err(BackendError::message("directory import cancelled"));
        }
        let paths = if create_root {
            vec![payload]
        } else {
            // Loose tracks and sidecars belong to their enclosing folder;
            // they are not standalone books when importing only its contents.
            fs::read_dir(&payload)?.map(|entry| entry.map(|entry| entry.path())).collect::<io::Result<Vec<_>>>()?.into_iter().filter(|path| path.is_dir() || supported(path)).collect()
        };
        // Staging is on the destination filesystem: this publishes by rename,
        // without copying again, before book inspection starts.
        failures.extend(self.place_and_import_paths(parent, paths, true).await?);
        Ok(failures)
    }

    async fn place_and_import_paths(&self, parent: DirId, paths: Vec<PathBuf>, moving: bool) -> Result<Vec<ImportFailure>, BackendError> {
        let destination = self.import_destination(parent).await?;
        let root = PathBuf::from(&self.root).canonicalize()?;
        let destination_relative = destination.strip_prefix(&root).map_err(BackendError::operation)?.to_owned();
        let occupied = self.database.shared_occupied_file_names(&parent)?;
        // Place the complete selection on disk before inspecting any books.
        let (placed, mut failures) = run_blocking(move || {
            let mut placed = Vec::new();
            let mut failures = Vec::new();
            for path in paths {
                let result = if path.is_dir() || supported(&path) { place_into_directory(&path, &destination, &occupied, moving) } else { Err(io::Error::other("not a supported book or folder")) };
                match result {
                    Ok(path) => placed.push(path),
                    Err(error) => failures.push(failure(&path, &BackendError::from(error))),
                }
            }
            (placed, failures)
        })
        .await
        .map_err(BackendError::operation)?;
        let inventory = run_blocking({
            let root = root.clone();
            move || inventory(&root, &placed)
        })
        .await
        .map_err(BackendError::operation)??;
        failures.extend(inventory.failures.into_iter().map(|(path, message)| ImportFailure::folder(&[path.to_string_lossy().into_owned()], &message)));
        let mut parents = HashMap::from([(destination_relative, parent)]);
        // Existing folders are reused on retry; register new folders at their
        // actual paths instead of creating another physical copy of the tree.
        for relative in inventory.directories {
            let Some(directory) = relative.parent().and_then(|path| parents.get(path)).copied() else {
                failures.push(failure(&relative, "parent folder could not be imported"));
                continue;
            };
            match self.adopt_directory(directory, &root, &relative) {
                Ok(id) => {
                    parents.insert(relative, id);
                }
                Err(error) => failures.push(failure(&relative, &error)),
            }
        }
        for relative in inventory.files {
            let Some(directory) = relative.parent().and_then(|path| parents.get(path)).copied() else {
                failures.push(failure(&relative, "parent folder could not be imported"));
                continue;
            };
            if let Err(error) = self.import_file_in_place(directory, &root, &relative).await {
                failures.push(failure(&relative, &error));
            }
        }
        Ok(failures)
    }

    /// Registers a folder placed under `directory`, or reuses the one already
    /// registered there on retry, and marks it on disk with its id.
    fn adopt_directory(&self, directory: DirId, root: &Path, relative: &Path) -> Result<DirId, BackendError> {
        let name = file_name(relative)?;
        if let Some(id) = self.database.child_directory(&directory, name)? {
            fs::write(root.join(relative).join(".biblos_uuid"), id.to_string())?;
            return Ok(id);
        }
        let (_, path) = self.database.directory_creation_path(&directory, name)?;
        if Path::new(&path) != relative {
            return Err(BackendError::message("destination folder changed before import"));
        }
        let id = DirId::new_v4();
        self.database.register_created_directory(&id, &directory, name, &path)?;
        fs::write(root.join(relative).join(".biblos_uuid"), id.to_string())?;
        Ok(id)
    }

    async fn import_file_in_place(&self, parent: DirId, root: &Path, relative: &Path) -> Result<(), BackendError> {
        let name = file_name(relative)?;
        let path = root.join(relative);
        let (import, ingested) = if path.is_dir() {
            validate_import_destination(&self.database, &parent)?;
            let import = ValidatedImport { parent_id: parent, file_name: filenames::validate_component(name)?, format: BookFormat::Mp3Folder };
            (import, ingest_audiobook_folder(path, name.to_owned(), &*self.cpu).await?)
        } else {
            let import = self.validate_import(&parent, name)?;
            (import, ingest_file(path, name.to_owned(), &*self.cpu).await?)
        };
        if self.database.storage_policy()?.external().is_some() {
            let relative_path = self.database.native_book_path(parent, name)?.ok_or_else(|| BackendError::message("destination folder no longer exists"))?;
            let copied = if import.format == BookFormat::Mp3Folder {
                self.assets.stage_audiobook_folder(root.join(relative), relative_path).await?
            } else { self.assets.stage_book(relative_path, Box::new(fs::File::open(root.join(relative))?)).await? };
            let staged = copied.verify_transfer(ingested.content_hash).await?;
            self.finish_provider_import(import, staged, ingested.inspection).await?;
            return Ok(());
        }
        let _lease = self.assets.lease_book(&ingested.content_hash).await?;
        // A concurrent folder move must not register these bytes at another path.
        validate_import_destination(&self.database, &parent)?;
        let placement = self.database.native_book_path(parent, name)?.ok_or_else(|| BackendError::message("destination folder no longer exists"))?;
        if Path::new(placement.as_str().trim_start_matches('/')) != relative {
            return Err(BackendError::message("destination folder changed during import"));
        }
        let published = PublishedStagedBook { name: name.to_owned(), relative_path: placement.to_string(), published: true, fingerprint: Some(ingested.fingerprint.to_string()) };
        self.commit_import(import, ingested.content_hash, ingested.size_bytes, Some(ingested.inspection), published)
    }
}

struct Inventory {
    directories: Vec<PathBuf>,
    files: Vec<PathBuf>,
    failures: Vec<(PathBuf, String)>,
}

fn inventory(root: &Path, placed: &[PathBuf]) -> Result<Inventory, BackendError> {
    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut failures = Vec::new();
    for path in placed {
        let relative = path.strip_prefix(root).map_err(BackendError::operation)?.to_owned();
        if path.is_file() {
            files.push(relative);
            continue;
        }
        match is_audiobook_folder(path) {
            Ok(true) => {
                files.push(relative);
                continue;
            }
            Ok(false) => directories.push(relative),
            Err(error) => {
                failures.push((relative, error.to_string()));
                continue;
            }
        }
        let mut walk = walk(path);
        while let Some(entry) = walk.next() {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                if entry.is_dir() {
                    walk.skip_current_dir();
                }
                continue;
            }
            let relative = entry.path().strip_prefix(root).map_err(BackendError::operation)?.to_owned();
            if entry.is_dir() {
                match is_audiobook_folder(entry.path()) {
                    Ok(true) => {
                        files.push(relative);
                        walk.skip_current_dir();
                    }
                    Ok(false) => directories.push(relative),
                    Err(error) => {
                        failures.push((relative, error.to_string()));
                        walk.skip_current_dir();
                    }
                }
            } else if entry.is_file() && supported(entry.path()) {
                files.push(relative);
            }
        }
    }
    directories.sort();
    directories.dedup();
    files.sort();
    files.dedup();
    Ok(Inventory { directories, files, failures })
}

/// Drop only the affected audiobooks while their copies are still hidden.
/// Checking the grouped inventory also catches failures in numbered discs.
/// A false result leaves the entire selected audiobook for staging cleanup.
fn discard_incomplete_audiobooks(payload: &Path, failed_members: &[PathBuf]) -> Result<bool, BackendError> {
    loop {
        let inventory = inventory(payload, &[payload.to_owned()])?;
        let candidates = inventory.files.into_iter().chain(inventory.failures.into_iter().map(|(path, _)| path));
        let mut discarded = false;
        for relative in candidates {
            let path = payload.join(relative);
            if !path.is_dir() || !failed_members.iter().any(|member| member.starts_with(&path)) {
                continue;
            }
            if path == payload {
                return Ok(false);
            }
            fs::remove_dir_all(path)?;
            discarded = true;
        }
        if !discarded {
            return Ok(true);
        }
        // Removing a child may make its parent look like one audiobook.
        // Check the final tree before allowing it out of staging as well.
    }
}

fn supported(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()).is_some_and(|name| BookFormat::from_file_name(name).is_some())
}

/// Keep the tracks and sidecars until the staged tree can be classified as
/// an audiobook or a regular folder containing individual books.
fn selected_folder_file_supported(path: &[String]) -> bool {
    let Some(name) = path.last() else { return false };
    let path = Path::new(name);
    supported(path) || audiobook_member(path)
}

fn audiobook_member(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "mp3" | "nfo" | "cue"))
        || path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| ["cover.jpg", "cover.jpeg", "cover.png", "cover.webp", "folder.jpg", "folder.jpeg", "folder.png", "folder.webp"].iter().any(|cover| name.eq_ignore_ascii_case(cover)))
}

fn file_name(path: &Path) -> Result<&str, BackendError> {
    path.file_name().and_then(|name| name.to_str()).ok_or_else(|| BackendError::message("invalid file name"))
}

fn failure(path: &Path, message: &(impl client_runtime::Classified + ?Sized)) -> ImportFailure {
    ImportFailure::file(&[path.to_string_lossy().into_owned()], message)
}

fn selected_path(root: &Path, components: &[String]) -> Result<PathBuf, BackendError> {
    if components.is_empty() {
        return Err(BackendError::message("selected path is empty"));
    }
    let mut path = root.to_owned();
    for component in components {
        path.push(filenames::validate_component(component)?);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    use library_database::Database;
    use library_files::AssetStore;
    use library_placement::LibraryPlacement;
    use library_runtime::{events::LibraryEventRouter, scheduler::SyncScheduler};
    use sync_common::ROOT_DIR_ID;

    use crate::{DirectoryImportFile, DirectoryImportReader, ImportSource};

    fn selected_file(path: &str, bytes: &[u8]) -> DirectoryImportFile {
        let bytes = bytes.to_vec();
        DirectoryImportFile { path: path.split('/').map(str::to_owned).collect(), source: ImportSource::new(Box::new(move || Box::pin(async move { Ok(Box::new(Cursor::new(bytes)) as DirectoryImportReader) }))) }
    }

    fn importer(root: &Path) -> LibraryImport {
        let database = Arc::new(Database::open(":memory:").unwrap());
        database.initialize_library().unwrap();
        let assets = AssetStore::open(root.to_str().unwrap()).unwrap();
        let (sync, _) = SyncScheduler::new();
        let (asset_work, _) = SyncScheduler::new();
        let (files, _) = SyncScheduler::new();
        let (enrichment, _) = SyncScheduler::new();
        let (events, _) = LibraryEventRouter::new(sync, asset_work, files, enrichment);
        let placement = Arc::new(LibraryPlacement { database: database.clone(), assets: assets.clone(), events: events.clone() });
        LibraryImport { library_id: DirId::new_v4(), cloud: Default::default(), provider_upload_lock: Default::default(), database, assets, cpu: Arc::new(client_platform_native::cpu_host::NativeHost), events, placement, root: root.to_str().unwrap().into() }
    }

    #[tokio::test]
    async fn dropped_mp3_folders_are_imported_with_tracks_without_a_scan() {
        for moving in [false, true] {
            for nested in [false, true] {
                for discs in [false, true] {
                    let library = tempfile::tempdir().unwrap();
                    let external = tempfile::tempdir().unwrap();
                    let source = external.path().join(if nested { "Collection" } else { "Audio" });
                    let audio = if nested { source.join("Audio") } else { source.clone() };
                    let (first, second) = if discs { ("Disc 1/01.mp3", "Disc 2/02.mp3") } else { ("01.mp3", "02.mp3") };
                    fs::create_dir_all(audio.join(first).parent().unwrap()).unwrap();
                    fs::create_dir_all(audio.join(second).parent().unwrap()).unwrap();
                    let track = include_bytes!("../../book-inspection/tests/fixtures/silence.mp3");
                    fs::write(audio.join(first), track).unwrap();
                    fs::write(audio.join(second), track).unwrap();
                    fs::write(audio.join("book.nfo"), b"Title: Audio\n").unwrap();
                    let importer = importer(library.path());
                    let shelf = importer.placement.create_directory(&ROOT_DIR_ID, "Shelf").unwrap().id;

                    let failures = importer.import_paths(shelf, vec![source.clone()], moving).await.unwrap();
                    assert!(failures.is_empty(), "{failures:?}");
                    assert_eq!(source.exists(), !moving);
                    let parent = if nested { importer.database.child_directory(&shelf, "Collection").unwrap().unwrap() } else { shelf };
                    let relative = if nested { "Shelf/Collection/Audio" } else { "Shelf/Audio" };
                    let placed = library.path().join(relative);
                    assert_eq!(fs::read(placed.join(first)).unwrap(), track);
                    assert_eq!(fs::read(placed.join("book.nfo")).unwrap(), b"Title: Audio\n");
                    assert!(importer.database.child_directory(&parent, "Audio").unwrap().is_none(), "an audiobook must be registered as a book");
                    let hashes = importer.database.upload_preparation_snapshot().unwrap().hashes();
                    assert_eq!(hashes.len(), 1, "the drop must register the audiobook before any scan");
                    let hash = *hashes.iter().next().unwrap();
                    assert_eq!(importer.database.book_paths(&hash).unwrap()[0].as_str(), format!("/{relative}"));
                    assert_eq!(importer.database.audiobook_tracks(&hash).unwrap().len(), 2);
                    assert!(importer.database.has_import_inspection(&hash, BookFormat::Mp3Folder, book_inspection::inspection_version(BookFormat::Mp3Folder)).unwrap());

                    let mut snapshot = importer.database.scan_snapshot().unwrap();
                    // This fixture has no placement worker to acknowledge the
                    // newly created directories; inspect their completed paths.
                    snapshot.excluded_paths.clear();
                    let scan = filesystem_scanner::filesystem_scan::discover(library.path(), &snapshot);
                    assert!(scan.readable);
                    assert_eq!(scan.files.len(), 1);
                    let inspected = filesystem_scanner::filesystem_scan::inspect(library.path(), &scan, &snapshot, &importer.cpu).await;
                    assert!(inspected.readable);
                    assert!(inspected.books.is_empty(), "the imported fingerprint and inspection must already match the scanner");

                    let failures = importer.import_paths(parent, vec![placed], true).await.unwrap();
                    assert!(failures.is_empty(), "{failures:?}");
                    assert_eq!(importer.database.upload_preparation_snapshot().unwrap().hashes(), hashes);
                    assert!(!library.path().join(format!("{relative} 2")).exists());
                }
            }
        }
    }

    #[test]
    fn dropped_inventory_distinguishes_audiobooks_from_mixed_folders() {
        let root = tempfile::tempdir().unwrap();
        let mixed = root.path().join("Mixed");
        let audio = mixed.join("Audio");
        fs::create_dir_all(&audio).unwrap();
        fs::write(mixed.join("book.epub"), b"book").unwrap();
        fs::write(mixed.join("sample.mp3"), b"audio").unwrap();
        fs::write(audio.join("01.mp3"), b"audio").unwrap();
        fs::write(audio.join("notes.txt"), b"sidecar").unwrap();

        let inventory = inventory(root.path(), &[mixed]).unwrap();
        assert_eq!(inventory.directories, vec![PathBuf::from("Mixed")]);
        assert_eq!(inventory.files, vec![PathBuf::from("Mixed/Audio"), PathBuf::from("Mixed/book.epub")]);
    }

    #[tokio::test]
    async fn selected_folder_detects_an_audiobook_before_creating_directories() {
        for discs in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let importer = importer(root.path());
            let track = include_bytes!("../../book-inspection/tests/fixtures/silence.mp3");
            let first = if discs { "Disc 1/01.mp3" } else { "01.mp3" };
            let second = if discs { "Disc 2/02.mp3" } else { "02.mp3" };
            let cue = format!("FILE \"{first}\" MP3\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nFILE \"{second}\" MP3\nTRACK 02 AUDIO\nINDEX 01 00:00:00\n");
            let mut cover = Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(2, 2).write_to(&mut cover, image::ImageFormat::Png).unwrap();
            let directory = DirectoryImport {
                name: "Audio".into(),
                directories: if discs { vec![vec!["Disc 1".into()], vec!["Disc 2".into()]] } else { Vec::new() },
                files: vec![selected_file(first, track), selected_file(second, track), selected_file("book.nfo", b"Title: Audio\n"), selected_file("book.cue", cue.as_bytes()), selected_file("cover.png", cover.get_ref())],
            };

            let failures = importer.import_selected_directory(ROOT_DIR_ID, directory, true, &|| false).await.unwrap();
            assert!(failures.is_empty(), "{failures:?}");
            assert!(importer.database.child_directory(&ROOT_DIR_ID, "Audio").unwrap().is_none());
            assert!(importer.database.has_book_named(&ROOT_DIR_ID, "Audio").unwrap());
            let hashes = importer.database.upload_preparation_snapshot().unwrap().hashes();
            assert_eq!(hashes.len(), 1);
            let hash = hashes.iter().next().unwrap();
            assert_eq!(importer.database.audiobook_tracks(hash).unwrap().len(), 2);
            for (path, bytes) in [(first, track.as_slice()), (second, track.as_slice()), ("book.nfo", b"Title: Audio\n".as_slice()), ("book.cue", cue.as_bytes()), ("cover.png", cover.get_ref().as_slice())] {
                assert_eq!(fs::read(root.path().join("Audio").join(path)).unwrap(), bytes);
            }
        }
    }

    #[tokio::test]
    async fn selected_mixed_folder_imports_regular_books_and_nested_audiobooks() {
        for create_root in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let importer = importer(root.path());
            let epub = include_bytes!("../../book-inspection/tests/fixtures/back-matter-evidence.epub");
            let track = include_bytes!("../../book-inspection/tests/fixtures/silence.mp3");
            let directory = DirectoryImport {
                name: "Books".into(),
                directories: vec![vec!["Nested".into()], vec!["Audio".into()]],
                files: vec![selected_file("book.epub", epub), selected_file("Nested/another.epub", epub), selected_file("sample.mp3", track), selected_file("Audio/01.mp3", track)],
            };

            let failures = importer.import_selected_directory(ROOT_DIR_ID, directory, create_root, &|| false).await.unwrap();
            assert!(failures.is_empty(), "{failures:?}");
            let parent = if create_root { importer.database.child_directory(&ROOT_DIR_ID, "Books").unwrap().unwrap() } else { ROOT_DIR_ID };
            assert!(importer.database.has_book_named(&parent, "book.epub").unwrap());
            let nested = importer.database.child_directory(&parent, "Nested").unwrap().unwrap();
            assert!(importer.database.has_book_named(&nested, "another.epub").unwrap());
            assert!(importer.database.has_book_named(&parent, "Audio").unwrap());
            assert!(importer.database.child_directory(&parent, "Audio").unwrap().is_none());
            let hashes = importer.database.upload_preparation_snapshot().unwrap().hashes();
            assert_eq!(hashes.len(), 2);
            assert_eq!(hashes.iter().map(|hash| importer.database.audiobook_tracks(hash).unwrap().len()).sum::<usize>(), 1);
        }
    }

    fn failed_selected_file(path: &str, during_copy: bool) -> DirectoryImportFile {
        struct FailedReader(bool);
        impl io::Read for FailedReader {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(Error::other("track copy interrupted"));
                }
                self.0 = true;
                let count = bytes.len().min(4);
                bytes[..count].copy_from_slice(&b"data"[..count]);
                Ok(count)
            }
        }
        DirectoryImportFile {
            path: path.split('/').map(str::to_owned).collect(),
            source: ImportSource::new(Box::new(move || {
                Box::pin(async move {
                    if during_copy {
                        Ok(Box::new(FailedReader(false)) as DirectoryImportReader)
                    } else {
                        Err("document provider could not open the file".into())
                    }
                })
            })),
        }
    }

    #[tokio::test]
    async fn failed_audiobook_members_never_leave_staging() {
        for member in ["02.mp3", "book.cue", "book.nfo", "cover.png", "Disc 2/02.mp3"] {
            for during_copy in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let importer = importer(root.path());
                let track = include_bytes!("../../book-inspection/tests/fixtures/silence.mp3");
                let first = if member.starts_with("Disc") { "Disc 1/01.mp3" } else { "01.mp3" };
                let directory = DirectoryImport { name: "Audio".into(), directories: Vec::new(), files: vec![selected_file(first, track), failed_selected_file(member, during_copy)] };

                let failures = importer.import_selected_directory(ROOT_DIR_ID, directory, true, &|| false).await.unwrap();
                assert_eq!(failures.len(), 1, "{member}: {failures:?}");
                assert!(!root.path().join("Audio").exists(), "the incomplete audiobook must stay hidden: {member}");
                assert!(!importer.database.has_book_named(&ROOT_DIR_ID, "Audio").unwrap());
                assert!(importer.database.upload_preparation_snapshot().unwrap().hashes().is_empty());
                assert!(fs::read_dir(root.path()).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().starts_with(".bokheim-copy-")));
            }
        }
    }

    #[tokio::test]
    async fn failed_nested_audiobook_does_not_block_complete_siblings() {
        for create_root in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let importer = importer(root.path());
            let track = include_bytes!("../../book-inspection/tests/fixtures/silence.mp3");
            let epub = include_bytes!("../../book-inspection/tests/fixtures/back-matter-evidence.epub");
            let directory = DirectoryImport {
                name: "Books".into(),
                directories: Vec::new(),
                files: vec![selected_file("Audio/Disc 1/01.mp3", track), failed_selected_file("Audio/Disc 2/02.mp3", true), selected_file("good.epub", epub), selected_file("Complete/01.mp3", track)],
            };

            let failures = importer.import_selected_directory(ROOT_DIR_ID, directory, create_root, &|| false).await.unwrap();
            assert_eq!(failures.len(), 1, "{failures:?}");
            let (parent, placed) = if create_root { (importer.database.child_directory(&ROOT_DIR_ID, "Books").unwrap().unwrap(), root.path().join("Books")) } else { (ROOT_DIR_ID, root.path().to_owned()) };
            assert!(importer.database.has_book_named(&parent, "good.epub").unwrap());
            assert!(importer.database.has_book_named(&parent, "Complete").unwrap());
            assert!(!placed.join("Audio").exists());
            assert_eq!(fs::read(placed.join("Complete/01.mp3")).unwrap(), track);
            assert_eq!(importer.database.upload_preparation_snapshot().unwrap().hashes().len(), 2);
        }
    }

    #[tokio::test]
    async fn removing_a_failed_child_does_not_publish_its_parent_as_an_audiobook() {
        let root = tempfile::tempdir().unwrap();
        let importer = importer(root.path());
        let track = include_bytes!("../../book-inspection/tests/fixtures/silence.mp3");
        let directory = DirectoryImport { name: "Books".into(), directories: Vec::new(), files: vec![selected_file("sample.mp3", track), selected_file("Audio/01.mp3", track), failed_selected_file("Audio/02.mp3", false)] };
        let failures = importer.import_selected_directory(ROOT_DIR_ID, directory, true, &|| false).await.unwrap();
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(!root.path().join("Books").exists());
        assert!(importer.database.upload_preparation_snapshot().unwrap().hashes().is_empty());
    }

    #[tokio::test]
    async fn audiobook_detection_failures_do_not_abort_other_books() {
        for moving in [false, true] {
            for nested in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let external = tempfile::tempdir().unwrap();
                let importer = importer(root.path());
                let source = if nested { external.path().join("Books") } else { external.path().to_owned() };
                fs::create_dir_all(source.join("Bad/Disc 1")).unwrap();
                fs::create_dir_all(source.join("Complete")).unwrap();
                let track = include_bytes!("../../book-inspection/tests/fixtures/silence.mp3");
                let epub = include_bytes!("../../book-inspection/tests/fixtures/back-matter-evidence.epub");
                fs::write(source.join("Bad/Disc 1/01.mp3"), track).unwrap();
                fs::write(source.join("Bad/first.cue"), b"FILE \"Disc 1/01.mp3\" MP3\n").unwrap();
                fs::write(source.join("Bad/second.cue"), b"FILE \"Disc 1/01.mp3\" MP3\n").unwrap();
                fs::write(source.join("good.epub"), epub).unwrap();
                fs::write(source.join("Complete/01.mp3"), track).unwrap();
                let paths = if nested { vec![source.clone()] } else { vec![source.join("Bad"), source.join("good.epub"), source.join("Complete")] };

                let failures = importer.import_paths(ROOT_DIR_ID, paths, moving).await.unwrap();
                assert_eq!(failures.len(), 1, "{failures:?}");
                assert_eq!(failures[0].kind, crate::ImportFailureKind::Folder);
                assert!(failures[0].diagnostic.as_deref().unwrap().contains("multiple matching sidecars"));
                let (parent, placed) = if nested { (importer.database.child_directory(&ROOT_DIR_ID, "Books").unwrap().unwrap(), root.path().join("Books")) } else { (ROOT_DIR_ID, root.path().to_owned()) };
                assert!(importer.database.has_book_named(&parent, "good.epub").unwrap());
                assert!(importer.database.has_book_named(&parent, "Complete").unwrap());
                assert!(importer.database.child_directory(&parent, "Bad").unwrap().is_none());
                assert!(!importer.database.has_book_named(&parent, "Bad").unwrap());
                assert_eq!(importer.database.upload_preparation_snapshot().unwrap().hashes().len(), 2);
                assert_eq!(fs::read(placed.join("Bad/Disc 1/01.mp3")).unwrap(), track);
                assert_eq!(source.join("Bad").exists(), !moving);
            }
        }
    }

    #[test]
    fn irrelevant_cue_errors_do_not_hide_regular_folder_books() {
        let root = tempfile::tempdir().unwrap();
        let mixed = root.path().join("Mixed");
        fs::create_dir(&mixed).unwrap();
        fs::write(mixed.join("book.epub"), b"book").unwrap();
        fs::write(mixed.join("sample.mp3"), b"audio").unwrap();
        fs::write(mixed.join("first.cue"), b"cue").unwrap();
        fs::write(mixed.join("second.cue"), b"cue").unwrap();

        let inventory = inventory(root.path(), &[mixed]).unwrap();
        assert!(inventory.failures.is_empty());
        assert_eq!(inventory.directories, vec![PathBuf::from("Mixed")]);
        assert_eq!(inventory.files, vec![PathBuf::from("Mixed/book.epub")]);
    }
}
