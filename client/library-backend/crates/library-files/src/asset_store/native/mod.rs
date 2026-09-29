//! Native library-file access and adjacent transfer staging.
mod projection;
pub(super) use projection::{create_directory, evict_book_files, prepare_directory_with_database, readable_book_paths, stage_directory_with_database};
use projection::{projection_error, readable_path};

use super::{AssetStoreError, ContentHash};
use book_access::BoxedBookReader;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
const RESERVED_STORAGE_BYTES: u64 = 128 * 1024 * 1024;
#[derive(Clone)]
pub(super) struct Handle(PathBuf, String, PathBuf, Option<super::BookPathLookup>, std::sync::Arc<std::sync::atomic::AtomicBool>);

impl std::fmt::Debug for Handle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Handle").field("root", &self.0).finish_non_exhaustive()
    }
}

pub(super) fn upload_stream(book: super::VerifiedBook) -> impl futures_util::Stream<Item = Result<Vec<u8>, std::io::Error>> + Send {
    futures_util::stream::try_unfold((book, 0u64), |(mut book, sent)| async move {
        client_platform_runtime::executor::run_blocking(move || {
            use std::io::Read as _;
            let mut chunk = vec![0_u8; 64 * 1024];
            let length = book.reader.read(&mut chunk)?;
            let sent = sent + length as u64;
            if sent > book.length || (length == 0 && sent != book.length) {
                return Err(std::io::Error::other("book length changed while streaming"));
            }
            chunk.truncate(length);
            Ok::<_, std::io::Error>((length != 0).then_some((chunk, (book, sent))))
        })
        .await
        .map_err(std::io::Error::other)?
    })
}

pub(super) fn upload_body(book: super::VerifiedBook) -> Result<reqwest::Body, AssetStoreError> {
    Ok(reqwest::Body::wrap_stream(upload_stream(book)))
}

pub(super) struct Lease(#[allow(dead_code)] std::fs::File);

pub(super) fn lease(handle: &Handle, hash: &str, exclusive: bool) -> Result<Option<Lease>, AssetStoreError> {
    if !enabled(handle) {
        return Err(AssetStoreError::operation("library storage is unavailable"));
    }
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(handle.2.join(hash))?;
    let result = if exclusive { fs2::FileExt::try_lock_exclusive(&file) } else { fs2::FileExt::try_lock_shared(&file) };
    match result {
        Ok(()) => Ok(Some(Lease(file))),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn purge_trash(handle: &Handle, hash: &ContentHash) -> Result<(), AssetStoreError> {
    match std::fs::remove_dir_all(handle.0.parent().unwrap().join("trash").join(hash.as_str())) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub(super) fn open(library_locator: &str) -> Result<Handle, AssetStoreError> {
    #[cfg(target_os = "android")]
    let private_leases = Some(client_platform_android::book::lease_root()?);
    #[cfg(not(target_os = "android"))]
    let private_leases = None;
    open_with_lease_root(library_locator, private_leases)
}

fn open_with_lease_root(library_locator: &str, private_leases: Option<&Path>) -> Result<Handle, AssetStoreError> {
    let root = Path::new(library_locator).join(crate::APP_HIDDEN_DIR).join("assets");
    // Create children one level at a time. Never recreate a library root
    // that disappears after its identity was checked.
    for directory in [root.parent().unwrap().to_path_buf(), root.clone(), root.join("thumbnail")] {
        create_child_directory(&directory)?;
    }
    let marker = root.parent().unwrap().join("storage-identity");
    let identity = match std::fs::read_to_string(&marker) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            use std::io::Write as _;
            let value = uuid::Uuid::new_v4().to_string();
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&marker) {
                Ok(mut file) => {
                    file.write_all(value.as_bytes())?;
                    file.sync_all()?;
                    value
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => std::fs::read_to_string(&marker)?,
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    // Android shared storage does not implement flock. Keep the same OS-backed
    // shared/exclusive locks on private storage, namespaced by canonical library
    // path so independently opened handles and processes coordinate correctly.
    let leases = match private_leases {
        Some(base) => base.join(blake3::hash(root.canonicalize()?.to_string_lossy().as_bytes()).to_hex().as_str()),
        None => root.join("leases"),
    };
    if private_leases.is_some() {
        std::fs::create_dir_all(&leases)?;
    } else {
        create_child_directory(&leases)?;
    }
    Ok(Handle(root, identity, leases, None, std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true))))
}

pub(super) fn root_available(handle: &Handle) -> bool {
    if !enabled(handle) {
        return false;
    }
    std::fs::read_to_string(handle.0.parent().unwrap().join("storage-identity")).is_ok_and(|value| value == handle.1)
}

pub(super) fn placement_path(handle: &Handle, relative: &str) -> Result<PathBuf, AssetStoreError> {
    if !root_available(handle) {
        return Err(AssetStoreError::operation("library storage is unavailable"));
    }
    checked_placement_path(handle.0.parent().unwrap().parent().unwrap(), relative)
}

pub(super) fn m4b_sidecar_cover(handle: &Handle, hash: &ContentHash) -> Result<Option<Vec<u8>>, AssetStoreError> {
    use std::io::Read as _;
    let Some(lookup) = handle.3.as_ref() else { return Ok(None); };
    for relative in lookup.paths(hash)? {
        let path = placement_path(handle, &relative)?;
        if !path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("m4b")) { continue; }
        if let Some(cover) = audiobook_folder::discover_m4b_cover(&path)? {
            let mut bytes = Vec::new();
            std::fs::File::open(cover.path)?.take(audiobook_folder::MAX_COVER_BYTES + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > audiobook_folder::MAX_COVER_BYTES { return Err(AssetStoreError::operation("M4B cover exceeds byte limit")); }
            return Ok(Some(bytes));
        }
    }
    Ok(None)
}

pub(super) fn local_mp3_folder_archive(handle: &Handle, hash: &ContentHash) -> Result<Option<BoxedBookReader>, AssetStoreError> {
    use std::io::Seek as _;
    let Some(lookup) = handle.3.as_ref() else { return Ok(None); };
    for relative in lookup.paths(hash)? {
        let path = placement_path(handle, &relative)?;
        if !path.is_dir() { continue; }
        let tracks = audiobook_folder::discover(&path)?;
        if tracks.is_empty() || ContentHash::new(audiobook_folder::identity(&tracks)?.to_hex().as_str()) != *hash { continue; }
        let nfo = audiobook_folder::discover_nfo(&path)?;
        let cue = audiobook_folder::discover_cue(&path)?;
        let cover = audiobook_folder::discover_cover(&path)?;
        let mut archive = audiobook_folder::write_archive_with_sidecars(tempfile::tempfile()?, &tracks, nfo.as_ref(), cue.as_ref(), cover.as_ref())?;
        archive.rewind()?;
        return Ok(Some(Box::new(archive)));
    }
    Ok(None)
}

pub(super) fn local_m4b_reader(handle: &Handle, hash: &ContentHash) -> Result<Option<BoxedBookReader>, AssetStoreError> {
    let Some(lookup) = handle.3.as_ref() else { return Ok(None); };
    for relative in lookup.paths(hash)? {
        let path = placement_path(handle, &relative)?;
        if !path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("m4b")) { continue; }
        if !path.is_file() || !content_matches(&path, hash)? { continue; }
        match std::fs::File::open(path) {
            Ok(file) => return Ok(Some(Box::new(file))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

fn checked_placement_path(root: &Path, relative: &str) -> Result<PathBuf, AssetStoreError> {
    let mut path = root.to_path_buf();
    for component in Path::new(relative.trim_start_matches('/')).components() {
        let std::path::Component::Normal(component) = component else {
            return Err(AssetStoreError::operation("invalid placement path"));
        };
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Err(AssetStoreError::operation("placement crosses a symbolic link")),
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    Ok(path)
}

pub(super) fn directory_available(handle: &Handle, relative: &str) -> bool {
    placement_path(handle, relative).is_ok_and(|path| path.is_dir())
}

fn content_matches(path: &Path, hash: &ContentHash) -> Result<bool, AssetStoreError> {
    if path.is_dir() {
        let tracks = audiobook_folder::discover(path)?;
        return Ok(!tracks.is_empty() && ContentHash::new(audiobook_folder::identity(&tracks)?.to_hex().as_str()) == *hash);
    }
    let mut file = std::fs::File::open(path)?;
    Ok(book_identity::identify(&mut file)? == *hash)
}

fn trash_path(handle: &Handle, hash: &ContentHash, relative: &str) -> PathBuf {
    handle.0.parent().unwrap().join("trash").join(hash.as_str()).join(blake3::hash(relative.as_bytes()).to_hex().as_str())
}

pub(super) fn placement_conflicts(handle: &Handle, hash: &ContentHash, relative: &str) -> Result<bool, AssetStoreError> {
    let (parent, name) = relative.rsplit_once('/').unwrap_or(("", relative));
    let path = placement_path(handle, parent)?.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !(metadata.is_file() || metadata.is_dir()) || metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Ok(!content_matches(&path, hash)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

// Hashing and copying happen before taking SQLite's writer. The final file
// action checks that verified paths have not changed while preparing the job.
pub(super) struct VerifiedPath {
    path: PathBuf,
    hash: ContentHash,
    directory: bool,
    length: u64,
    modified: std::time::SystemTime,
}

impl VerifiedPath {
    fn new(path: PathBuf, hash: &ContentHash) -> Result<Self, AssetStoreError> {
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
            return Err(AssetStoreError::operation("unsupported book placement"));
        }
        let verified = Self { path, hash: *hash, directory: metadata.is_dir(), length: metadata.len(), modified: metadata.modified()? };
        if !content_matches(&verified.path, hash)? {
            return Err(AssetStoreError::operation("file contains different book bytes"));
        }
        verified.check()?;
        Ok(verified)
    }

    fn check(&self) -> Result<(), AssetStoreError> {
        let metadata = std::fs::symlink_metadata(&self.path)?;
        if metadata.is_dir() != self.directory || metadata.file_type().is_symlink() || metadata.len() != self.length || metadata.modified()? != self.modified || (self.directory && !content_matches(&self.path, &self.hash)?) {
            return Err(AssetStoreError::operation("book changed while preparing file work"));
        }
        Ok(())
    }
}

pub(super) enum PreparedPlacement {
    Absent,
    Existing(VerifiedPath),
    Restore {
        output: tempfile::NamedTempFile,
        destination: PathBuf,
    },
    RestoreFolder {
        output: tempfile::TempDir,
        destination: PathBuf,
    },
    /// A move whose source is also queued for trash: nothing else can need
    /// it, so relocate instead of copying.
    Renamed {
        source: PathBuf,
        destination: PathBuf,
    },
    Trash {
        source: VerifiedPath,
        destination: PathBuf,
        existing: Option<VerifiedPath>,
    },
}

impl PreparedPlacement {
    pub(super) fn publish(self) -> Result<(), AssetStoreError> {
        match self {
            Self::Absent => Ok(()),
            Self::Existing(file) => file.check(),
            Self::Restore { output, destination } => {
                persist_book_noclobber(output.into_temp_path(), &destination).map_err(|error| AssetStoreError::operation(error))?;
                sync_parent(&destination)
            }
            Self::RestoreFolder { output, destination } => {
                if destination.exists() { return Err(AssetStoreError::operation("restore destination changed")); }
                std::fs::rename(output.path(), &destination)?;
                sync_parent(&destination)
            }
            Self::Renamed { source, destination } => {
                if destination.exists() {
                    return Err(AssetStoreError::operation("restore destination changed"));
                }
                match std::fs::rename(&source, &destination) {
                    Ok(()) => {}
                    // Cross-device: fall back to copy.
                    Err(_) => {
                        std::fs::copy(&source, &destination)?;
                        std::fs::remove_file(&source)?;
                    }
                }
                sync_parent(&destination)?;
                sync_parent(&source)
            }
            Self::Trash { source, destination, existing } => {
                source.check()?;
                if let Some(existing) = existing {
                    existing.check()?;
                    if source.directory { std::fs::remove_dir_all(&source.path)?; } else { std::fs::remove_file(&source.path)?; }
                } else {
                    // A concurrent replay for this hash is excluded by its lease.
                    if destination.exists() {
                        return Err(AssetStoreError::operation("Trash destination changed"));
                    }
                    std::fs::rename(&source.path, &destination)?;
                }
                sync_parent(&destination)?;
                sync_parent(&source.path)
            }
        }
    }
}

pub(super) fn prepare_trash(handle: &Handle, hash: &ContentHash, relative: &str) -> Result<PreparedPlacement, AssetStoreError> {
    let source = placement_path(handle, relative)?;
    match std::fs::symlink_metadata(&source) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(PreparedPlacement::Absent),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let source = VerifiedPath::new(source, hash)?;
    let destination = trash_path(handle, hash, relative);
    std::fs::create_dir_all(destination.parent().unwrap())?;
    let existing = if destination.exists() { Some(VerifiedPath::new(destination.clone(), hash)?) } else { None };
    Ok(PreparedPlacement::Trash { source, destination, existing })
}

pub(super) fn prepare_restore(handle: &Handle, hash: &ContentHash, relative: &str, expected: Option<&str>, source_paths: &[String], movable_sources: &[String]) -> Result<Option<PreparedPlacement>, AssetStoreError> {
    use std::io::Write as _;
    let destination = placement_path(handle, relative)?;
    if destination.exists() {
        return Ok(Some(PreparedPlacement::Existing(VerifiedPath::new(destination, hash)?)));
    }
    let matches_version = |path: &Path| -> Result<bool, AssetStoreError> {
        if !content_matches(path, hash)? {
            return Ok(false);
        }
        // The ZIP is a transport representation. The ordered source bytes
        // already established the folder's identity above.
        if path.is_dir() { return Ok(true); }
        let Some(expected) = expected else { return Ok(true) };
        let mut hasher = blake3::Hasher::new();
        hasher.update_reader(std::fs::File::open(path)?)?;
        Ok(hasher.finalize().to_hex().as_str() == expected)
    };
    let found = source_paths.iter().filter_map(|relative| placement_path(handle, relative).ok().map(|path| (relative, path))).find(|(_, path)| (path.is_file() || path.is_dir()) && matches_version(path).unwrap_or(false));
    // Only a source with a queued trash job for this path is safe to move.
    if let Some((source_relative, source)) = &found {
        if movable_sources.iter().any(|movable| movable == *source_relative) {
            if !destination.parent().unwrap().is_dir() {
                return Err(AssetStoreError::operation("restore folder is unavailable"));
            }
            return Ok(Some(PreparedPlacement::Renamed { source: source.clone(), destination }));
        }
    }
    let source = if let Some((_, source)) = found {
        Some(source)
    } else {
        let directory = handle.0.parent().unwrap().join("trash").join(hash.as_str());
        let mut found = None;
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries {
                let path = entry?.path();
                if (path.is_file() || path.is_dir()) && matches_version(&path)? {
                    found = Some(path);
                    break;
                }
            }
        }
        found
    };
    let Some(source) = source else { return Ok(None) };
    if !destination.parent().unwrap().is_dir() {
        return Err(AssetStoreError::operation("restore folder is unavailable"));
    }
    if source.is_dir() {
        let output = tempfile::Builder::new().prefix(".bokheim-restore-").tempdir_in(destination.parent().unwrap())?;
        for track in audiobook_folder::discover(&source)? {
            let target = output.path().join(&track.name);
            std::fs::create_dir_all(target.parent().unwrap())?;
            std::fs::copy(&track.path, target)?;
        }
        if let Some(nfo) = audiobook_folder::discover_nfo(&source)? {
            std::fs::copy(nfo.path, output.path().join(nfo.name))?;
        }
        if let Some(cue) = audiobook_folder::discover_cue(&source)? {
            std::fs::copy(cue.path, output.path().join(cue.name))?;
        }
        if let Some(cover) = audiobook_folder::discover_cover(&source)? {
            std::fs::copy(cover.path, output.path().join(cover.name))?;
        }
        if !matches_version(output.path())? {
            return Err(AssetStoreError::operation("audiobook changed during restore"));
        }
        return Ok(Some(PreparedPlacement::RestoreFolder { output, destination }));
    }
    let mut output = tempfile::Builder::new().prefix(".bokheim-restore-").tempfile_in(destination.parent().unwrap())?;
    let mut input = std::fs::File::open(source)?;
    std::io::copy(&mut input, &mut output)?;
    output.flush()?;
    output.as_file().sync_all()?;
    // Verify the staged bytes too: an external editor can change the source
    // between source discovery and copying it.
    if !matches_version(output.path())? {
        return Err(AssetStoreError::operation("book changed during copy"));
    }
    Ok(Some(PreparedPlacement::Restore { output, destination }))
}

fn path(handle: &Handle, name: String) -> PathBuf {
    handle.0.join(name)
}

pub(super) struct AsyncWriter {
    // Field order closes the handle before removing an abandoned temporary file.
    file: tokio::fs::File,
    temporary_path: tempfile::TempPath,
}

impl AsyncWriter {
    pub(super) async fn write_all(&mut self, buffer: &[u8]) -> Result<(), AssetStoreError> {
        use tokio::io::AsyncWriteExt as _;
        self.file.write_all(buffer).await?;
        Ok(())
    }

    pub(super) async fn finish_book(mut self) -> Result<tempfile::TempPath, AssetStoreError> {
        use tokio::io::AsyncWriteExt as _;
        self.file.flush().await?;
        self.file.sync_all().await?;
        drop(self.file);
        Ok(self.temporary_path)
    }
}

pub(super) fn sync_parent(path: &Path) -> Result<(), AssetStoreError> {
    #[cfg(unix)]
    {
        let parent = path.parent().ok_or_else(|| AssetStoreError::operation("asset has no parent"))?;
        client_platform_native::filesystem::sync_directory(parent).map_err(|error| AssetStoreError::operation(format!("could not sync changes to library folder '{}': {error}", parent.display())))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(target_os = "android")]
pub(super) use client_platform_android::book::persist_book_noclobber;

#[cfg(not(target_os = "android"))]
pub(super) fn persist_book_noclobber(temporary: tempfile::TempPath, destination: &Path) -> Result<(), tempfile::PathPersistError> {
    client_platform_native::filesystem::persist_noclobber(temporary, destination)
}

pub(super) async fn begin_write_at(final_path: PathBuf) -> Result<AsyncWriter, AssetStoreError> {
    let temporary =
        client_platform_runtime::executor::run_blocking(move || tempfile::Builder::new().prefix(".bokheim-download.part-").tempfile_in(final_path.parent().unwrap())).await.map_err(|error| AssetStoreError::operation(error))??;
    let (file, temporary_path) = temporary.into_parts();
    Ok(AsyncWriter { file: tokio::fs::File::from_std(file), temporary_path })
}

pub(super) struct StagedBook {
    // A cleanup guard; direct imports use the final path from the outset.
    pub(super) temporary_path: tempfile::TempPath,
    pub(super) content_hash: String,
    pub(super) checksum: ContentHash,
    pub(super) size_bytes: u64,
    pub(super) direct: Option<DirectBook>,
}

static DIRECT_IMPORTS: std::sync::OnceLock<std::sync::Mutex<HashSet<PathBuf>>> = std::sync::OnceLock::new();

pub(super) struct DirectBook {
    pub(super) relative: String,
    pub(super) path: PathBuf,
}

impl Drop for DirectBook {
    fn drop(&mut self) {
        DIRECT_IMPORTS.get().unwrap().lock().unwrap().remove(&self.path);
    }
}

pub(super) fn import_in_progress(path: &Path) -> bool {
    let Some(imports) = DIRECT_IMPORTS.get() else { return false };
    let imports = imports.lock().unwrap();
    !imports.is_empty() && path.canonicalize().is_ok_and(|path| imports.contains(&path))
}

pub(super) fn copy_book_to_destination(handle: &Handle, relative: &str, source: &mut (dyn std::io::Read + Send), mut occupied: Vec<String>, progress: Option<&super::ImportProgressObserver>) -> Result<(String, StagedBook), AssetStoreError> {
    let (parent, requested) = relative.rsplit_once('/').unwrap_or(("", relative));
    let directory = placement_path(handle, parent)?.canonicalize()?;
    occupied.extend(std::fs::read_dir(&directory)?.map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned())).collect::<Result<Vec<_>, _>>()?);
    loop {
        let name = library_replica::unique_file_name(requested, occupied.iter().map(String::as_str));
        let path = directory.join(&name);
        let imports = DIRECT_IMPORTS.get_or_init(Default::default);
        if !imports.lock().unwrap().insert(path.clone()) {
            occupied.push(name);
            continue;
        }
        let direct = DirectBook { relative: format!("{parent}/{name}"), path: path.clone() };
        let file = match std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                occupied.push(name);
                continue;
            }
            Err(error) => return Err(AssetStoreError::operation(format!("could not create the imported book '{}': {error}", path.display()))),
        };
        // Wrap the final file only to clean it up on copy/inspection failure.
        // No temporary filename, rename, hard link, or second copy is involved.
        let cleanup = tempfile::TempPath::try_from_path(path)?;
        let mut file = tempfile::NamedTempFile::from_parts(file, cleanup);
        let (content_hash, checksum, size_bytes) = write_import_bytes(file.as_file_mut(), source, &directory, progress)?;
        return Ok((name, StagedBook { temporary_path: file.into_temp_path(), content_hash, checksum, size_bytes, direct: Some(direct) }));
    }
}

pub(super) fn open_staged_reader(staged: &StagedBook) -> Result<BoxedBookReader, AssetStoreError> {
    Ok(Box::new(std::fs::File::open(&staged.temporary_path)?))
}

pub(super) fn stage_book_in(directory: &Path, source: &mut (dyn std::io::Read + Send), progress: Option<&super::ImportProgressObserver>) -> Result<StagedBook, AssetStoreError> {
    let mut file =
        tempfile::Builder::new().prefix(".bokheim-import.part-").tempfile_in(directory).map_err(|error| AssetStoreError::operation(format!("could not create an import file in library folder '{}': {error}", directory.display())))?;
    let (content_hash, checksum, size_bytes) = write_import_bytes(file.as_file_mut(), source, directory, progress)?;
    Ok(StagedBook { temporary_path: file.into_temp_path(), content_hash, checksum, size_bytes, direct: None })
}

fn write_import_bytes(file: &mut std::fs::File, source: &mut (dyn std::io::Read + Send), directory: &Path, progress: Option<&super::ImportProgressObserver>) -> Result<(String, ContentHash, u64), AssetStoreError> {
    use library_model::ImportFileStage;
    use std::io::Write as _;
    let report = |stage| {
        if let Some(progress) = progress {
            progress(stage);
        }
    };
    report(ImportFileStage::Copying { copied_bytes: 0 });
    let mut last_progress = std::time::Instant::now();
    let started = std::time::Instant::now();
    log::info!(target: "import_timing", "copy_started");
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = match source.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result.map_err(|error| AssetStoreError::operation(format!("could not read the selected book: {error}")))?,
        };
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).map_err(|error| AssetStoreError::operation(format!("could not write the imported book in library folder '{}': {error}", directory.display())))?;
        hasher.update(&buffer[..read]);
        if last_progress.elapsed() >= std::time::Duration::from_millis(100) {
            report(ImportFileStage::Copying { copied_bytes: hasher.count() });
            last_progress = std::time::Instant::now();
        }
    }
    if hasher.count() == 0 {
        return Err(AssetStoreError::operation("book is empty".to_owned()));
    }
    log::info!(target: "import_timing", "copy_checksum_finished bytes={} ms={:.3}", hasher.count(), started.elapsed().as_secs_f64() * 1000.0);
    report(ImportFileStage::Copying { copied_bytes: hasher.count() });
    report(ImportFileStage::Saving);
    let started = std::time::Instant::now();
    file.sync_all().map_err(|error| AssetStoreError::operation(format!("could not flush the imported book in library folder '{}': {error}", directory.display())))?;
    log::info!(target: "import_timing", "file_flush_finished ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
    let checksum = ContentHash::new(hasher.finalize().to_hex().as_str());
    report(ImportFileStage::Identifying);
    let started = std::time::Instant::now();
    let identity = book_identity::read(file)?.unwrap_or(checksum);
    log::info!(target: "import_timing", "identity_finished ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
    Ok((identity.to_string(), checksum, hasher.count()))
}

pub(super) fn write_capacity(handle: &Handle) -> Result<Option<u64>, AssetStoreError> {
    Ok(Some(available_bytes(handle)?.saturating_sub(RESERVED_STORAGE_BYTES)))
}

pub(super) fn available_bytes(handle: &Handle) -> Result<u64, AssetStoreError> {
    fs2::available_space(&handle.0).map_err(Into::into)
}

#[cfg(target_os = "android")]
pub(super) fn open_playback_file(handle: &Handle, name: String) -> Result<Option<std::fs::File>, AssetStoreError> {
    readable_path(handle, name)?.map(std::fs::File::open).transpose().map_err(Into::into)
}

pub(super) fn open_reader(handle: &Handle, name: String) -> Result<Option<BoxedBookReader>, AssetStoreError> {
    let Some(path) = readable_path(handle, name)? else { return Ok(None) };
    match std::fs::File::open(path) {
        Ok(file) => Ok(Some(Box::new(file))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn open_book_at_path(handle: &Handle, relative_path: &str) -> Result<Option<BoxedBookReader>, AssetStoreError> {
    let path = placement_path(handle, relative_path)?;
    match std::fs::File::open(path) {
        Ok(file) => Ok(Some(Box::new(file))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn open_pdf_at_path(handle: &Handle, relative_path: &str) -> Result<Option<(BoxedBookReader, String)>, AssetStoreError> {
    let path = placement_path(handle, relative_path)?;
    match std::fs::File::open(path) {
        Ok(file) => {
            let fingerprint = client_platform_native::file_fingerprint::file_metadata_fingerprint(&file.metadata()?)?.to_string();
            Ok(Some((Box::new(file), fingerprint)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn read(handle: &Handle, name: String) -> Result<Option<Vec<u8>>, AssetStoreError> {
    let Some(path) = readable_path(handle, name)? else { return Ok(None) };
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) async fn write(handle: &Handle, name: String, bytes: &[u8]) -> Result<(), AssetStoreError> {
    use std::io::Write as _;
    if name.starts_with("book/") {
        return Err(AssetStoreError::operation("native books require a library destination"));
    }
    let hash = name.strip_prefix("thumbnail/").and_then(|name| name.split('.').next()).ok_or_else(|| AssetStoreError::operation("unsupported native asset path"))?;
    let Some(lease) = lease(handle, hash, false)? else { return Err(AssetStoreError::operation("book is busy; retry thumbnail write")) };
    let destination = path(handle, name);
    let bytes = bytes.to_owned();
    client_platform_runtime::executor::run_blocking(move || {
        let _lease = lease;
        let mut file = tempfile::Builder::new().prefix(".bokheim-thumbnail.part-").tempfile_in(destination.parent().unwrap())?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        // Persist replaces atomically; a failed persist still owns and removes
        // the temporary file when its error is dropped.
        file.persist(&destination).map_err(|error| error.error)?;
        sync_parent(&destination)
    })
    .await
    .map_err(|error| AssetStoreError::operation(error))?
}

pub(super) fn remove(handle: &Handle, name: String) -> Result<bool, AssetStoreError> {
    if !enabled(handle) {
        return Err(AssetStoreError::operation("library storage is unavailable"));
    }
    if name.starts_with("book/") {
        return Ok(false);
    }
    match std::fs::remove_file(path(handle, name)) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn exists(handle: &Handle, name: String) -> Result<bool, AssetStoreError> {
    if let Some(hash) = name.strip_prefix("book/") {
        if !enabled(handle) { return Ok(false); }
        let hash = hash.parse::<ContentHash>().map_err(projection_error)?;
        for path in readable_book_paths(handle, &hash)? {
            if path.is_file() { return Ok(true); }
            if path.is_dir() && audiobook_folder::discover(&path).is_ok_and(|tracks| !tracks.is_empty()) { return Ok(true); }
        }
        return Ok(false);
    }
    Ok(readable_path(handle, name)?.is_some())
}

#[cfg(test)]
mod lease_location_tests {
    use super::*;

    #[test]
    fn initialization_never_recreates_a_moved_library_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        std::fs::create_dir(&root).unwrap();
        std::fs::rename(&root, temp.path().join("moved")).unwrap();
        assert!(open_with_lease_root(root.to_str().unwrap(), None).is_err());
        assert!(!root.exists());
        // A move between individual child creations cannot recreate parents.
        assert!(create_child_directory(&root.join(".bokheim/assets")).is_err());
        assert!(!root.exists());
    }

    #[test]
    fn cancelling_download_creation_does_not_leave_a_temporary_file() {
        let library = tempfile::tempdir().unwrap();
        let destination = library.path().join("book.epub");
        std::fs::write(&destination, b"existing book").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap();
        let (release, released) = std::sync::mpsc::channel();
        let blocker = runtime.spawn_blocking(move || released.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
        runtime.block_on(async {
            let mut pending = Box::pin(begin_write_at(destination.clone()));
            assert!(futures_util::poll!(pending.as_mut()).is_pending());
            drop(pending);
            release.send(()).unwrap();
            blocker.await.unwrap();
        });
        // Runtime shutdown waits for the cancelled call's blocking task too.
        drop(runtime);
        assert_eq!(std::fs::read(&destination).unwrap(), b"existing book");
        assert_eq!(std::fs::read_dir(library.path()).unwrap().count(), 1);
    }

    #[test]
    fn cancelled_thumbnail_write_retains_its_lease_until_book_finishes() {
        let library = tempfile::tempdir().unwrap();
        let handle = open(library.path().to_str().unwrap()).unwrap();
        let hash = blake3::hash(b"thumbnail owner").to_hex().to_string();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap();
        let (release, released) = std::sync::mpsc::channel();
        let blocker = runtime.spawn_blocking(move || released.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
        runtime.block_on(async {
            let mut pending = Box::pin(write(&handle, format!("thumbnail/{hash}"), b"cover"));
            assert!(futures_util::poll!(pending.as_mut()).is_pending());
            assert!(lease(&handle, &hash, true).unwrap().is_none());
            drop(pending);
            assert!(lease(&handle, &hash, true).unwrap().is_none());
            release.send(()).unwrap();
            blocker.await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if lease(&handle, &hash, true).unwrap().is_some() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(std::fs::read(handle.0.join("thumbnail").join(&hash)).unwrap(), b"cover");
        });
    }

    #[test]
    fn private_locks_coordinate_handles_without_using_shared_storage() {
        let library = tempfile::tempdir().unwrap();
        let private = tempfile::tempdir().unwrap();
        let first = open_with_lease_root(library.path().to_str().unwrap(), Some(private.path())).unwrap();
        let alias = library.path().join(".");
        let second = open_with_lease_root(alias.to_str().unwrap(), Some(private.path())).unwrap();
        assert!(!first.0.join("leases").exists());
        assert!(first.2.starts_with(private.path()));
        let reader = lease(&first, "book", false).unwrap().unwrap();
        let other_reader = lease(&second, "book", false).unwrap().unwrap();
        assert!(lease(&second, "book", true).unwrap().is_none());
        drop(reader);
        drop(other_reader);
        let writer = lease(&second, "book", true).unwrap().unwrap();
        assert!(lease(&first, "book", false).unwrap().is_none());
        drop(writer);
        assert!(lease(&first, "book", true).unwrap().is_some());
    }
}

#[cfg(test)]
mod prepared_file_tests {
    use super::*;

    #[test]
    fn prepared_trash_preserves_a_file_changed_before_book() {
        let root = tempfile::tempdir().unwrap();
        let handle = open(root.path().to_str().unwrap()).unwrap();
        let path = root.path().join("book.epub");
        std::fs::write(&path, b"original").unwrap();
        let hash = ContentHash::new(blake3::hash(b"original").to_hex().as_str());
        let prepared = prepare_trash(&handle, &hash, "/book.epub").unwrap();
        std::fs::write(&path, b"externally changed book").unwrap();
        assert!(prepared.publish().is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"externally changed book");
    }

    #[test]
    fn prepared_restore_preserves_a_new_destination() {
        let root = tempfile::tempdir().unwrap();
        let handle = open(root.path().to_str().unwrap()).unwrap();
        let hash = ContentHash::new(blake3::hash(b"original").to_hex().as_str());
        let retained = trash_path(&handle, &hash, "/book.epub");
        std::fs::create_dir_all(retained.parent().unwrap()).unwrap();
        std::fs::write(&retained, b"original").unwrap();
        let prepared = prepare_restore(&handle, &hash, "/book.epub", None, &[], &[]).unwrap().unwrap();
        let destination = root.path().join("book.epub");
        std::fs::write(&destination, b"unrelated user file").unwrap();
        assert!(prepared.publish().is_err());
        assert_eq!(std::fs::read(destination).unwrap(), b"unrelated user file");
        assert_eq!(std::fs::read(retained).unwrap(), b"original");
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn imported_and_renamed_m4b_retains_embedded_identity_after_edits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("book.m4b");
        std::fs::write(&path, include_bytes!("../../../tests/fixtures/embedded-cover.m4b")).unwrap();
        let identity = book_identity::ensure(&path).unwrap();
        // Alter an independent MP4 box without changing the identity metadata.
        use std::io::Write as _;
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"\0\0\0\x0cfreedata").unwrap();
        assert!(content_matches(&path, &identity).unwrap());
        let staged = stage_book_in(directory.path(), &mut std::fs::File::open(&path).unwrap(), None).unwrap();
        assert_eq!(staged.content_hash, identity.to_string());
        // Trash names need not retain a file extension.
        let trash = directory.path().join("extensionless");
        std::fs::rename(path, &trash).unwrap();
        assert!(content_matches(&trash, &identity).unwrap());
        assert!(!content_matches(&trash, &ContentHash::new(&"0".repeat(64))).unwrap());
    }
}
pub(super) fn with_book_paths(mut handle: Handle, lookup: super::BookPathLookup) -> Handle {
    handle.3 = Some(lookup);
    handle
}

pub(super) fn enabled(handle: &Handle) -> bool {
    handle.4.load(std::sync::atomic::Ordering::Acquire)
}
pub(super) fn suspend(handle: &Handle) {
    handle.4.store(false, std::sync::atomic::Ordering::Release);
}
pub(super) fn unavailable(locator: &str) -> Handle {
    let root = Path::new(locator).join(crate::APP_HIDDEN_DIR).join("assets");
    let leases = root.join("leases");
    Handle(root, String::new(), leases, None, std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)))
}

fn create_child_directory(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}
