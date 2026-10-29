//! Ingest a native book already present on disk, whether copied or discovered.

use std::io::{self, Seek};
use std::path::{Path, PathBuf};

use book_model::BookFormat;
use sync_common::BookId;

pub struct IngestedFile {
    pub content_hash: BookId,
    pub size_bytes: u64,
    pub fingerprint: u64,
    pub format: BookFormat,
    pub inspection: book_inspection::InspectedBook,
}

/// Hash and inspect the same file bytes, rejecting concurrent changes.
pub async fn ingest_file(path: PathBuf, name: String, host: &dyn cpu_host::CpuHost) -> io::Result<IngestedFile> {
    let format = BookFormat::from_file_name(&name)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unsupported book format"))?;
    let (content_hash, size_bytes, fingerprint, reader) = client_platform_runtime::executor::run_blocking({
        let path = path.clone();
        move || -> io::Result<_> {
            let fingerprint = fingerprint(&path)?;
            let mut reader = library_files::filesystem::open(&path)?;
            let mut hasher = blake3::Hasher::new();
            hasher.update_reader(&mut reader)?;
            let size_bytes = hasher.count();
            if size_bytes == 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "empty book"));
            }
            require_unchanged(&path, fingerprint)?;
            let content_hash = BookId::from(hasher.finalize());
            reader.rewind()?;
            Ok((content_hash, size_bytes, fingerprint, reader))
        }
    })
    .await
    .map_err(|error| io::Error::other(error.to_string()))??;
    let inspection = cpu_host::submit_book(host, cpu_host::Inspect { source_name: name, format, content_hash: Some(content_hash) }, cpu_host::as_cpu_reader(reader)).await;
    // A file edited during inspection must be retried rather than committed
    // with a content_hash and metadata from different revisions.
    require_unchanged(&path, fingerprint)?;
    Ok(IngestedFile { content_hash, size_bytes, fingerprint, format, inspection: inspection.map_err(io::Error::other)? })
}

/// Inspect the reproducible archive of a native MP3 folder, while keeping its
/// tracks and sidecars as ordinary files at the library placement.
pub async fn ingest_audiobook_folder(path: PathBuf, name: String, host: &dyn cpu_host::CpuHost) -> io::Result<IngestedFile> {
    let (content_hash, size_bytes, fingerprint, reader) = client_platform_runtime::executor::run_blocking({
        let path = path.clone();
        move || -> io::Result<_> {
            let tracks = audiobook_folder::discover(&path)?;
            if tracks.is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "audiobook folder has no MP3 tracks"));
            }
            let fingerprint = library_files::filesystem::audiobook_fingerprint(&path, &tracks)?;
            require_folder_unchanged(&path, fingerprint)?;
            let mut archive = audiobook_folder::write_folder_archive(&path, tempfile::tempfile()?, &tracks)?;
            require_folder_unchanged(&path, fingerprint)?;
            archive.rewind()?;
            let mut hasher = blake3::Hasher::new();
            hasher.update_reader(&mut archive)?;
            archive.rewind()?;
            Ok((BookId::from(hasher.finalize()), hasher.count(), fingerprint, archive))
        }
    })
    .await
    .map_err(|error| io::Error::other(error.to_string()))??;
    let format = BookFormat::Mp3Folder;
    let inspection = cpu_host::submit_book(host, cpu_host::Inspect { source_name: name, format, content_hash: Some(content_hash) }, cpu_host::as_cpu_reader(reader)).await;
    require_folder_unchanged(&path, fingerprint)?;
    Ok(IngestedFile { content_hash, size_bytes, fingerprint, format, inspection: inspection.map_err(io::Error::other)? })
}

fn require_folder_unchanged(path: &Path, fingerprint: u64) -> io::Result<()> {
    let tracks = audiobook_folder::discover(path)?;
    if library_files::filesystem::audiobook_fingerprint(path, &tracks)? != fingerprint {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "audiobook folder changed during inspection"));
    }
    Ok(())
}

fn require_unchanged(path: &Path, fingerprint: u64) -> io::Result<()> {
    if self::fingerprint(path)? != fingerprint {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "book changed during ingestion"));
    }
    Ok(())
}

pub(crate) fn fingerprint(path: &Path) -> std::io::Result<u64> {
    let audio = library_files::filesystem::fingerprint(path)?;
    if !path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("m4b")) {
        return Ok(audio);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(&audio.to_le_bytes());
    if let Some(cover) = audiobook_folder::discover_m4b_cover(path)? {
        hasher.update(cover.name.as_bytes());
        hasher.update(&library_files::filesystem::fingerprint(cover.path)?.to_le_bytes());
    }
    let [a, b, c, d, e, f, g, h, ..] = *hasher.finalize().as_bytes();
    Ok(u64::from_le_bytes([a, b, c, d, e, f, g, h]))
}
