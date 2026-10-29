//! Native library-file access and adjacent transfer staging.

use crate::APP_HIDDEN_DIR;
use crate::filesystem::{copy_entry, rename_entry};
use client_platform_native::filesystem::parent_directory;
use content_address::{ContentHash, hash_file};
use fs2::{FileExt, lock_contended_error};
use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
pub(crate) mod projection;
pub(crate) use projection::readable_path;
pub(crate) use projection::{create_directory, evict_book_files};

pub(crate) use super::{AssetStoreError, BookId};
pub(crate) use book_access::BoxedBookReader;
pub(crate) use library_model::RelativeBookPath;
pub(crate) use std::collections::HashSet;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) const RESERVED_STORAGE_BYTES: u64 = 128 * 1024 * 1024;
pub(crate) type RootFailure = Box<dyn FnOnce() + Send + 'static>;
#[derive(Clone)]
pub(crate) struct Handle {
    pub(crate) library_root: PathBuf,
    pub(crate) assets_root: PathBuf,
    pub(crate) storage_identity: String,
    pub(crate) leases_root: PathBuf,
    pub(crate) enabled: bool,
    pub(crate) root_failure: Arc<Mutex<Option<RootFailure>>>,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Handle").field("root", &self.assets_root).finish_non_exhaustive()
    }
}

pub(crate) fn upload_stream(book: super::VerifiedBook) -> impl futures_util::Stream<Item = Result<Vec<u8>, Error>> + Send {
    futures_util::stream::try_unfold((book, 0u64), |(mut book, sent)| async move {
        client_platform_runtime::executor::run_blocking(move || {
            use std::io::Read as _;
            let mut chunk = vec![0_u8; 64 * 1024];
            let length = book.reader.read(&mut chunk)?;
            let sent = sent + length as u64;
            if sent > book.length || (length == 0 && sent != book.length) {
                return Err(Error::other("book length changed while streaming"));
            }
            chunk.truncate(length);
            Ok::<_, Error>((length != 0).then_some((chunk, (book, sent))))
        })
        .await
        .map_err(Error::other)?
    })
}

pub(crate) struct Lease {
    // Closing the owned file releases its lock when the lease is dropped.
    _file: File,
}

pub(crate) fn lease(handle: &Handle, hash: &str, exclusive: bool) -> Result<Option<Lease>, AssetStoreError> {
    if !handle.enabled {
        return Err(AssetStoreError::operation("library storage is unavailable"));
    }
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(handle.leases_root.join(hash)).inspect_err(|_| {
        root_available(handle);
    })?;
    let result = if exclusive { FileExt::try_lock_exclusive(&file) } else { FileExt::try_lock_shared(&file) };
    match result {
        Ok(()) => Ok(Some(Lease { _file: file })),
        Err(error) if error.kind() == ErrorKind::WouldBlock || error.raw_os_error().is_some_and(|code| Some(code) == lock_contended_error().raw_os_error()) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn open(library_locator: &str) -> Result<Handle, AssetStoreError> {
    #[cfg(target_os = "android")]
    let private_leases = Some(client_platform_android::book::lease_root()?);
    #[cfg(not(target_os = "android"))]
    let private_leases: Option<&Path> = None;
    let library_root = PathBuf::from(library_locator);
    let root = library_root.join(APP_HIDDEN_DIR).join("assets");
    // Create children one level at a time. Never recreate a library root
    // that disappears after its identity was checked.
    for directory in [parent_directory(&root)?.to_path_buf(), root.clone(), root.join("thumbnail")] {
        create_child_directory(&directory)?;
    }
    let marker = parent_directory(&root)?.join("storage-identity");
    let identity = match std::fs::read_to_string(&marker) {
        Ok(value) => value,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            use std::io::Write as _;
            let value = uuid::Uuid::new_v4().to_string();
            match OpenOptions::new().write(true).create_new(true).open(&marker) {
                Ok(mut file) => {
                    file.write_all(value.as_bytes())?;
                    file.sync_all()?;
                    value
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => std::fs::read_to_string(&marker)?,
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
    Ok(Handle { library_root, assets_root: root, storage_identity: identity, leases_root: leases, enabled: true, root_failure: Default::default() })
}

pub(crate) fn root_available(handle: &Handle) -> bool {
    if !handle.enabled {
        return false;
    }
    let available = std::fs::read_to_string(handle.library_root.join(APP_HIDDEN_DIR).join("storage-identity")).is_ok_and(|value| value == handle.storage_identity);
    if !available {
        let callback = handle.root_failure.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(callback) = callback {
            callback();
        }
    }
    available
}

pub(crate) fn placement_path(handle: &Handle, relative: &RelativeBookPath) -> PathBuf {
    library_path(handle, relative.as_str())
}

/// A library folder path: "" for the root, "a/b" below it.
pub(crate) fn library_path(handle: &Handle, relative: &str) -> PathBuf {
    handle.library_root.join(relative.trim_start_matches('/'))
}

pub(crate) fn m4b_sidecar_cover(handle: &Handle, paths: &[RelativeBookPath]) -> Result<Option<Vec<u8>>, AssetStoreError> {
    use std::io::Read as _;
    for relative in paths {
        let path = placement_path(handle, relative);
        if !path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("m4b")) {
            continue;
        }
        if let Some(cover) = audiobook_folder::discover_m4b_cover(&path)? {
            let mut bytes = Vec::new();
            File::open(cover.path)?.take(audiobook_archive::MAX_COVER_BYTES + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > audiobook_archive::MAX_COVER_BYTES {
                return Err(AssetStoreError::operation("M4B cover exceeds byte limit"));
            }
            return Ok(Some(bytes));
        }
    }
    Ok(None)
}

pub(crate) fn content_matches(path: &Path, hash: &BookId) -> Result<bool, AssetStoreError> {
    let actual: ContentHash = if path.is_dir() {
        let tracks = audiobook_folder::discover(path)?;
        if tracks.is_empty() {
            return Ok(false);
        }
        audiobook_folder::folder_hash(path)?.into()
    } else {
        hash_file(&mut File::open(path)?)?
    };
    if let Some(expected) = hash.content_hash() {
        return Ok(actual == expected);
    }
    for ancestor in path.ancestors().skip(1) {
        let metadata = ancestor.join(APP_HIDDEN_DIR);
        if metadata.join("storage-identity").is_file() {
            let proof = metadata.join("assets").join("provider-proofs").join(hash.to_hex());
            return Ok(read_provider_proof(&proof)?.lines().take(64).any(|line| line.parse::<ContentHash>().ok() == Some(actual)));
        }
    }
    Ok(false)
}

fn read_provider_proof(path: &Path) -> Result<String, AssetStoreError> {
    use std::io::Read;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error.into()),
    };
    let mut proof = String::new();
    file.take(64 * 65 + 1).read_to_string(&mut proof)?;
    if proof.len() > 64 * 65 {
        return Err(AssetStoreError::operation("provider download proof exceeds its size limit"));
    }
    Ok(proof)
}

pub(crate) fn remember_provider_checksum(handle: &Handle, id: &BookId, digest: &BookId) -> Result<(), AssetStoreError> {
    use std::io::Write;
    if !id.is_external() || digest.is_external() || !root_available(handle) {
        return Err(AssetStoreError::operation("invalid provider download proof or unavailable library folder"));
    }
    let directory = handle.assets_root.join("provider-proofs");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(id.to_hex());
    let mut known = read_provider_proof(&path)?.lines().filter_map(|line| line.parse::<ContentHash>().ok()).take(63).collect::<Vec<_>>();
    let actual = digest.content_hash().expect("checked content digest");
    known.retain(|digest| *digest != actual);
    known.insert(0, actual);
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    for digest in known {
        writeln!(temporary, "{digest}")?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|error| AssetStoreError::operation(error.error))?;
    sync_parent(&path)
}

pub(crate) fn current_provider_checksum(handle: &Handle, id: &BookId) -> Result<Option<ContentHash>, AssetStoreError> {
    let proof = handle.assets_root.join("provider-proofs").join(id.to_hex());
    Ok(read_provider_proof(&proof)?.lines().next().and_then(|line| line.parse().ok()))
}

pub(crate) fn placement_conflicts(handle: &Handle, hash: &BookId, relative: &RelativeBookPath) -> Result<bool, AssetStoreError> {
    let path = placement_path(handle, relative);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !(metadata.is_file() || metadata.is_dir()) || metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Ok(!content_matches(&path, hash)?),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

// Hashing and copying happen before taking SQLite's writer. The final file
// action checks that verified paths have not changed while preparing the job.
pub(crate) struct VerifiedPath {
    pub(crate) path: PathBuf,
    pub(crate) hash: BookId,
    pub(crate) directory: bool,
    pub(crate) length: u64,
    pub(crate) modified: std::time::SystemTime,
}

impl VerifiedPath {
    pub(crate) fn new(path: PathBuf, hash: &BookId) -> Result<Self, AssetStoreError> {
        Self::matching(path, hash)?.ok_or_else(|| AssetStoreError::operation("file contains different book bytes"))
    }

    /// A different identity is distinct from an unreadable/invalid placement.
    /// Only cleanup may treat that identity mismatch as obsolete work.
    pub(crate) fn matching(path: PathBuf, hash: &BookId) -> Result<Option<Self>, AssetStoreError> {
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
            return Err(AssetStoreError::operation("unsupported book placement"));
        }
        let verified = Self { path, hash: *hash, directory: metadata.is_dir(), length: metadata.len(), modified: metadata.modified()? };
        if !content_matches(&verified.path, hash)? {
            return Ok(None);
        }
        verified.check()?;
        Ok(Some(verified))
    }

    pub(crate) fn check(&self) -> Result<(), AssetStoreError> {
        self.check_metadata()?;
        if self.directory && !content_matches(&self.path, &self.hash)? {
            return Err(AssetStoreError::operation("book changed while preparing file work"));
        }
        Ok(())
    }

    fn check_metadata(&self) -> Result<(), AssetStoreError> {
        let metadata = std::fs::symlink_metadata(&self.path)?;
        if metadata.is_dir() != self.directory || metadata.file_type().is_symlink() || metadata.len() != self.length || metadata.modified()? != self.modified {
            return Err(AssetStoreError::operation("book changed while preparing file work"));
        }
        Ok(())
    }
}

pub(crate) enum PreparedPlacement {
    /// Expected book is absent; the path may contain an unrelated replacement.
    Absent,
    Existing(VerifiedPath),
    Copied {
        output: tempfile::NamedTempFile,
        destination: PathBuf,
    },
    CopiedFolder {
        output: tempfile::TempDir,
        destination: PathBuf,
    },
    /// A move whose source is also queued for removal: nothing else can need
    /// it, so relocate instead of copying.
    Renamed {
        source: VerifiedPath,
        destination: PathBuf,
    },
    Removed(VerifiedPath),
}

impl PreparedPlacement {
    pub(crate) fn publish(self) -> Result<(), AssetStoreError> {
        self.publish_with_copy_fallback(true)
    }

    pub(crate) fn publish_short(self) -> Result<(), AssetStoreError> {
        self.publish_with_copy_fallback(false)
    }

    fn publish_with_copy_fallback(self, allow_copy: bool) -> Result<(), AssetStoreError> {
        match self {
            Self::Absent => Ok(()),
            Self::Existing(file) => {
                if allow_copy {
                    file.check()
                } else {
                    file.check_metadata()
                }
            }
            Self::Copied { output, destination } => {
                persist_book_noclobber(output.into_temp_path(), &destination).map_err(AssetStoreError::operation)?;
                sync_parent(&destination)
            }
            Self::CopiedFolder { output, destination } => {
                if destination.exists() {
                    return Err(AssetStoreError::operation("placement destination changed"));
                }
                rename_entry(output.path(), &destination)?;
                sync_parent(&destination)
            }
            Self::Renamed { source, destination } => {
                let verified = source;
                verified.check_metadata()?;
                let source = verified.path.clone();
                if destination.exists() {
                    return Err(AssetStoreError::operation("placement destination changed"));
                }
                match rename_entry(&source, &destination) {
                    Ok(()) => {}
                    Err(error) if allow_copy && error.kind() == ErrorKind::CrossesDevices => {
                        // Stage the whole copy before publication. A failed copy
                        // leaves both the source and destination untouched.
                        let staging = tempfile::Builder::new().prefix(".bokheim-copy-").tempdir_in(parent_directory(&destination)?)?;
                        let staged = staging.path().join("contents");
                        copy_entry(&source, &staged)?;
                        if !content_matches(&staged, &verified.hash)? {
                            return Err(AssetStoreError::operation("book changed during copy"));
                        }
                        verified.check()?;
                        rename_entry(&staged, &destination)?;
                        if source.is_dir() {
                            std::fs::remove_dir_all(&source)?;
                        } else {
                            std::fs::remove_file(&source)?;
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
                sync_parent(&destination)?;
                sync_parent(&source)
            }
            Self::Removed(source) => {
                source.check()?;
                if source.directory {
                    std::fs::remove_dir_all(&source.path)?;
                } else {
                    std::fs::remove_file(&source.path)?;
                }
                sync_parent(&source.path)
            }
        }
    }
}

pub(crate) fn prepare_removal(handle: &Handle, hash: &BookId, relative: &RelativeBookPath) -> Result<PreparedPlacement, AssetStoreError> {
    let source = placement_path(handle, relative);
    match std::fs::symlink_metadata(&source) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(PreparedPlacement::Absent),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    // The expected book is no longer at this path. Acknowledge only this
    // book's obsolete work/projection without touching the replacement bytes.
    let Some(source) = VerifiedPath::matching(source, hash)? else { return Ok(PreparedPlacement::Absent) };
    Ok(PreparedPlacement::Removed(source))
}

pub(crate) struct AsyncWriter {
    // Field order closes the handle before removing an abandoned temporary file.
    pub(crate) file: tokio::fs::File,
    pub(crate) temporary_path: tempfile::TempPath,
}

impl AsyncWriter {
    pub(crate) async fn write_all(&mut self, buffer: &[u8]) -> Result<(), AssetStoreError> {
        use tokio::io::AsyncWriteExt as _;
        self.file.write_all(buffer).await?;
        Ok(())
    }

    pub(crate) async fn finish_book(mut self) -> Result<tempfile::TempPath, AssetStoreError> {
        use tokio::io::AsyncWriteExt as _;
        self.file.flush().await?;
        self.file.sync_all().await?;
        drop(self.file);
        Ok(self.temporary_path)
    }
}

pub(crate) fn sync_parent(path: &Path) -> Result<(), AssetStoreError> {
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
pub(crate) use client_platform_android::book::persist_book_noclobber;

#[cfg(not(target_os = "android"))]
pub(crate) fn persist_book_noclobber(temporary: tempfile::TempPath, destination: &Path) -> Result<(), tempfile::PathPersistError> {
    client_platform_native::filesystem::persist_noclobber(temporary, destination)
}

pub(crate) struct StagedBook {
    // A cleanup guard; direct imports use the final path from the outset.
    pub(crate) temporary_path: tempfile::TempPath,
    pub(crate) content_hash: BookId,
    pub(crate) size_bytes: u64,
    pub(crate) direct: Option<DirectBook>,
}

pub(crate) struct CopiedBook {
    pub(crate) temporary_path: tempfile::TempPath,
    pub(crate) direct: Option<DirectBook>,
}

pub(crate) static DIRECT_IMPORTS: OnceLock<std::sync::Mutex<HashSet<PathBuf>>> = OnceLock::new();

pub(crate) struct DirectBook {
    pub(crate) path: PathBuf,
}

impl Drop for DirectBook {
    fn drop(&mut self) {
        if let Some(imports) = DIRECT_IMPORTS.get() {
            imports.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.path);
        }
    }
}

pub(crate) fn import_in_progress(path: &Path) -> bool {
    let Some(imports) = DIRECT_IMPORTS.get() else { return false };
    let imports = imports.lock().unwrap_or_else(PoisonError::into_inner);
    !imports.is_empty() && path.canonicalize().is_ok_and(|path| imports.contains(&path))
}

pub(crate) fn write_import_bytes(file: &mut File, source: &mut (dyn std::io::Read + Send), directory: &Path) -> Result<(), AssetStoreError> {
    use std::io::Write as _;
    let mut copied_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = match source.read(&mut buffer) {
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            result => result.map_err(|error| AssetStoreError::operation(format!("could not read the selected book: {error}")))?,
        };
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).map_err(|error| AssetStoreError::operation(format!("could not write the imported book in library folder '{}': {error}", directory.display())))?;
        copied_bytes += read as u64;
    }
    if copied_bytes == 0 {
        return Err(AssetStoreError::operation("book is empty".to_owned()));
    }
    file.sync_all().map_err(|error| AssetStoreError::operation(format!("could not flush the imported book in library folder '{}': {error}", directory.display())))?;
    Ok(())
}

pub(crate) fn write_capacity(handle: &Handle) -> Result<Option<u64>, AssetStoreError> {
    Ok(Some(fs2::available_space(&handle.assets_root)?.saturating_sub(RESERVED_STORAGE_BYTES)))
}

#[cfg(target_os = "android")]
pub(crate) fn open_playback_file(handle: &Handle, relative_path: &RelativeBookPath) -> Result<Option<File>, AssetStoreError> {
    match File::open(placement_path(handle, relative_path)) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn open_audiobook_track(handle: &Handle, folder: &RelativeBookPath, logical_name: &str) -> Result<Option<BoxedBookReader>, AssetStoreError> {
    let folder = placement_path(handle, folder);
    if !folder.try_exists()? {
        root_available(handle);
        return Ok(None);
    }
    open_file_reader(&audiobook_folder::resolve_track_path(&folder, logical_name)?)
}

pub(crate) fn open_book_at_path(handle: &Handle, relative_path: &RelativeBookPath) -> Result<Option<BoxedBookReader>, AssetStoreError> {
    let path = placement_path(handle, relative_path);
    if path.is_dir() {
        use std::io::Seek as _;
        let tracks = audiobook_folder::discover(&path)?;
        if tracks.is_empty() {
            return Ok(None);
        }
        let mut archive = audiobook_folder::write_folder_archive(&path, tempfile::tempfile()?, &tracks)?;
        archive.rewind()?;
        return Ok(Some(Box::new(archive)));
    }
    let result = open_file_reader(&path);
    if !matches!(&result, Ok(Some(_))) {
        root_available(handle);
    }
    result
}

fn open_file_reader(path: &Path) -> Result<Option<BoxedBookReader>, AssetStoreError> {
    match File::open(path) {
        Ok(file) => Ok(Some(Box::new(file))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn read(handle: &Handle, name: String) -> Result<Option<Vec<u8>>, AssetStoreError> {
    let Some(path) = readable_path(handle, name)? else { return Ok(None) };
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) async fn write(handle: &Handle, name: String, bytes: &[u8]) -> Result<(), AssetStoreError> {
    if name.starts_with("book/") {
        return Err(AssetStoreError::operation("native books require a library destination"));
    }
    let hash = name.strip_prefix("thumbnail/").and_then(|name| name.split('.').next()).ok_or_else(|| AssetStoreError::operation("unsupported native asset path"))?;
    let Some(lease) = lease(handle, hash, false)? else { return Err(AssetStoreError::operation("book is busy; retry thumbnail write")) };
    write_files(handle, vec![(name, bytes.to_owned())], lease).await
}

pub(crate) async fn write_thumbnail_set(handle: &Handle, hash: &BookId, bytes: &[u8], browse: &[u8], card_high_density: &[u8]) -> Result<(), AssetStoreError> {
    let Some(lease) = lease(handle, hash.to_hex().as_str(), true)? else { return Err(AssetStoreError::operation("thumbnail is being updated")) };
    write_files(
        handle,
        vec![
            (super::asset_name(super::BlobKind::Thumbnail, hash), bytes.to_owned()),
            (super::thumbnail_variant_name(hash, thumbnail::BROWSE_THUMBNAIL_WIDTH), browse.to_owned()),
            (super::thumbnail_variant_name(hash, thumbnail::THUMBNAIL_WIDTH), card_high_density.to_owned()),
        ],
        lease,
    )
    .await
}

async fn write_files(handle: &Handle, files: Vec<(String, Vec<u8>)>, lease: Lease) -> Result<(), AssetStoreError> {
    use std::io::Write as _;
    let root = handle.assets_root.clone();
    client_platform_runtime::executor::run_blocking(move || {
        let _lease = lease;
        for (name, bytes) in files {
            let destination = root.join(name);
            let mut file = tempfile::Builder::new().prefix(".bokheim-thumbnail.part-").tempfile_in(parent_directory(&destination)?)?;
            file.write_all(&bytes)?;
            file.as_file().sync_all()?;
            file.persist(&destination).map_err(|error| error.error)?;
            sync_parent(&destination)?;
        }
        Ok(())
    })
    .await
    .map_err(AssetStoreError::operation)?
}

pub(crate) fn remove(handle: &Handle, name: String) -> Result<bool, AssetStoreError> {
    if !handle.enabled {
        return Err(AssetStoreError::operation("library storage is unavailable"));
    }
    if name.starts_with("book/") {
        return Err(AssetStoreError::operation("native books require explicit paths"));
    }
    match std::fs::remove_file(handle.assets_root.join(name)) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn exists(handle: &Handle, name: String) -> Result<bool, AssetStoreError> {
    Ok(readable_path(handle, name)?.is_some())
}

#[cfg(test)]
mod lease_location_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn missing_files_report_root_loss_once_and_leave_healthy_roots_alone() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let library = tempfile::tempdir().unwrap();
        let store = super::super::AssetStore::open(library.path().to_str().unwrap()).unwrap();
        let failures = Arc::new(AtomicUsize::new(0));
        let reported = failures.clone();
        store.on_root_unavailable(move || {
            reported.fetch_add(1, Ordering::Relaxed);
        });
        let hash = BookId::from(blake3::hash(b"missing book"));
        let paths = [RelativeBookPath::parse("book.epub").unwrap()];

        assert!(!store.book_exists(&hash, &paths).unwrap());
        assert!(store.read(library_model::BlobKind::Thumbnail, &hash).unwrap().is_none());
        assert_eq!(failures.load(Ordering::Relaxed), 0, "missing individual files must not trigger root recovery");

        std::fs::remove_dir_all(library.path().join(APP_HIDDEN_DIR)).unwrap();
        assert!(!store.book_exists(&hash, &paths).unwrap());
        assert_eq!(failures.load(Ordering::Relaxed), 1);
        assert!(!store.root_available());
        assert!(!store.book_exists(&hash, &paths).unwrap());
        assert_eq!(failures.load(Ordering::Relaxed), 1, "the unavailable store must not flood the recovery queue");
    }

    #[test]
    fn a_returning_mount_reports_its_changed_identity_even_when_the_book_exists() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let library = tempfile::tempdir().unwrap();
        let store = super::super::AssetStore::open(library.path().to_str().unwrap()).unwrap();
        let failures = Arc::new(AtomicUsize::new(0));
        let reported = failures.clone();
        store.on_root_unavailable(move || {
            reported.fetch_add(1, Ordering::Relaxed);
        });
        let hash = BookId::from(blake3::hash(b"book"));
        let paths = [RelativeBookPath::parse("book.epub").unwrap()];
        std::fs::write(library.path().join("book.epub"), b"book").unwrap();
        assert!(store.book_exists(&hash, &paths).unwrap());
        assert_eq!(failures.load(Ordering::Relaxed), 0);
        std::fs::write(library.path().join(APP_HIDDEN_DIR).join("storage-identity"), b"returned mount").unwrap();
        assert!(!store.book_exists(&hash, &paths).unwrap());
        assert_eq!(failures.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cancelling_download_creation_does_not_leave_a_temporary_file() {
        let library = tempfile::tempdir().unwrap();
        let store = super::super::AssetStore::open(library.path().to_str().unwrap()).unwrap();
        let destination = library.path().join("book.epub");
        std::fs::write(&destination, b"existing book").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap();
        let (release, released) = std::sync::mpsc::channel();
        let blocker = runtime.spawn_blocking(move || released.recv_timeout(Duration::from_secs(5)).unwrap());
        runtime.block_on(async {
            let hash = BookId::from(blake3::hash(b"existing book"));
            let path = RelativeBookPath::parse("book.epub").unwrap();
            let mut pending = Box::pin(store.begin_book_write(&path, &hash));
            assert!(futures_util::poll!(pending.as_mut()).is_pending());
            drop(pending);
            release.send(()).unwrap();
            blocker.await.unwrap();
        });
        // Runtime shutdown waits for the cancelled call's blocking task too.
        drop(runtime);
        assert_eq!(std::fs::read(&destination).unwrap(), b"existing book");
        assert_eq!(std::fs::read_dir(library.path()).unwrap().count(), 2);
    }

    #[test]
    fn cancelled_thumbnail_write_retains_its_lease_until_book_finishes() {
        let library = tempfile::tempdir().unwrap();
        let handle = open(library.path().to_str().unwrap()).unwrap();
        let hash = blake3::hash(b"thumbnail owner").to_hex().to_string();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap();
        let (release, released) = std::sync::mpsc::channel();
        let blocker = runtime.spawn_blocking(move || released.recv_timeout(Duration::from_secs(5)).unwrap());
        runtime.block_on(async {
            let mut pending = Box::pin(write(&handle, format!("thumbnail/{hash}"), b"cover"));
            assert!(futures_util::poll!(pending.as_mut()).is_pending());
            assert!(lease(&handle, &hash, true).unwrap().is_none());
            drop(pending);
            assert!(lease(&handle, &hash, true).unwrap().is_none());
            release.send(()).unwrap();
            blocker.await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if lease(&handle, &hash, true).unwrap().is_some() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(std::fs::read(handle.assets_root.join("thumbnail").join(&hash)).unwrap(), b"cover");
        });
    }
}

#[cfg(test)]
mod prepared_file_tests {
    use super::*;

    #[test]
    fn prepared_removal_preserves_a_file_changed_before_publish() {
        let root = tempfile::tempdir().unwrap();
        let handle = open(root.path().to_str().unwrap()).unwrap();
        let path = root.path().join("book.epub");
        std::fs::write(&path, b"original").unwrap();
        let hash = BookId::from(blake3::hash(b"original"));
        let prepared = prepare_removal(&handle, &hash, &RelativeBookPath::parse("/book.epub").unwrap()).unwrap();
        std::fs::write(&path, b"externally changed book").unwrap();
        assert!(prepared.publish().is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"externally changed book");
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[tokio::test]
    async fn imported_and_renamed_m4b_uses_current_byte_identity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("book.m4b");
        std::fs::write(&path, include_bytes!("../../../tests/fixtures/embedded-cover.m4b")).unwrap();
        let original_identity = BookId::from(hash_file(&mut File::open(&path).unwrap()).unwrap());
        // A non-audio MP4 edit changes identity because all file bytes are hashed.
        use std::io::Write as _;
        OpenOptions::new().append(true).open(&path).unwrap().write_all(b"\0\0\0\x0cfreedata").unwrap();
        assert!(!content_matches(&path, &original_identity).unwrap());
        let identity = BookId::from(hash_file(&mut File::open(&path).unwrap()).unwrap());
        assert!(content_matches(&path, &identity).unwrap());
        let store = super::super::AssetStore::open(directory.path().to_str().unwrap()).unwrap();
        let copied = store.stage_book(RelativeBookPath::parse("copy.m4b").unwrap(), Box::new(File::open(&path).unwrap())).await.unwrap();
        assert!(content_matches(copied.path(), &identity).unwrap());
        // Renaming preserves bytes and therefore preserves identity.
        let renamed = directory.path().join("extensionless");
        std::fs::rename(path, &renamed).unwrap();
        assert!(content_matches(&renamed, &identity).unwrap());
        assert!(!content_matches(&renamed, &BookId::new(&"0".repeat(64))).unwrap());
    }

    #[test]
    fn provider_download_proofs_accept_known_revisions_but_reject_unrelated_bytes() {
        let root = tempfile::tempdir().unwrap();
        let handle = open(root.path().to_str().unwrap()).unwrap();
        let id = BookId::for_provider("google_drive", "account", "my_drive", "book");
        let path = root.path().join("book.m4b");
        std::fs::write(&path, b"first revision").unwrap();
        assert!(!content_matches(&path, &id).unwrap());
        remember_provider_checksum(&handle, &id, &BookId::from(blake3::hash(b"first revision"))).unwrap();
        assert!(content_matches(&path, &id).unwrap());
        remember_provider_checksum(&handle, &id, &BookId::from(blake3::hash(b"second revision"))).unwrap();
        assert!(content_matches(&path, &id).unwrap());
        std::fs::write(&path, b"second revision").unwrap();
        assert!(content_matches(&path, &id).unwrap());
        std::fs::write(&path, b"another book").unwrap();
        assert!(!content_matches(&path, &id).unwrap());
        assert!(remember_provider_checksum(&handle, &id, &id).is_err());
    }
}
pub(crate) fn unavailable(locator: &str) -> Handle {
    let library_root = PathBuf::from(locator);
    let root = library_root.join(APP_HIDDEN_DIR).join("assets");
    let leases = root.join("leases");
    Handle { library_root, assets_root: root, storage_identity: String::new(), leases_root: leases, enabled: false, root_failure: Default::default() }
}

pub(crate) fn create_child_directory(path: &Path) -> Result<(), Error> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}
