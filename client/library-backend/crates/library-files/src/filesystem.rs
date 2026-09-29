//! Concrete native filesystem access for a library root.
//!
//! This module deliberately exposes bytes, paths, and directory entries only.
//! It does not classify books or persist scan decisions.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub struct DirectoryWalk {
    entries: walkdir::IntoIter,
}

pub struct DirectoryEntry {
    path: PathBuf,
    file_name: std::ffi::OsString,
    directory: bool,
    file: bool,
    symlink: bool,
    depth: usize,
    length: Option<u64>,
}

impl DirectoryEntry {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn file_name(&self) -> &OsStr {
        &self.file_name
    }
    pub fn is_dir(&self) -> bool {
        self.directory
    }
    pub fn is_file(&self) -> bool {
        self.file
    }
    pub fn is_symlink(&self) -> bool {
        self.symlink
    }
    pub fn depth(&self) -> usize {
        self.depth
    }
    /// `None` means the entry could not be stat'ed without aborting the walk.
    pub fn length(&self) -> Option<u64> {
        self.length
    }
}

impl DirectoryWalk {
    pub fn next(&mut self) -> Option<Result<DirectoryEntry, std::io::Error>> {
        self.entries.next().map(|entry| {
            let entry = entry.map_err(walk_error)?;
            let kind = entry.file_type();
            Ok(DirectoryEntry {
                path: entry.path().to_path_buf(),
                file_name: entry.file_name().to_os_string(),
                directory: kind.is_dir(),
                file: kind.is_file(),
                symlink: kind.is_symlink(),
                depth: entry.depth(),
                length: entry.metadata().ok().map(|metadata| metadata.len()),
            })
        })
    }

    pub fn skip_current_dir(&mut self) {
        self.entries.skip_current_dir();
    }
}

pub fn walk(root: impl AsRef<Path>) -> DirectoryWalk {
    DirectoryWalk { entries: walkdir::WalkDir::new(root).follow_links(false).min_depth(1).into_iter() }
}

pub fn read_to_string(path: impl AsRef<Path>) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}
pub fn write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}
pub fn open(path: impl AsRef<Path>) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

pub fn fingerprint(path: impl AsRef<Path>) -> std::io::Result<u64> {
    client_platform_native::file_fingerprint::file_metadata_fingerprint(&std::fs::metadata(path)?)
}

/// Fingerprint the ordered tracks as one scanned placement. Both the scanner
/// and download publisher use this so a fresh download is already up to date.
pub fn audiobook_fingerprint(folder: &Path, tracks: &[audiobook_folder::Track]) -> std::io::Result<u64> {
    let mut hasher = blake3::Hasher::new();
    for track in tracks {
        hasher.update(track.name.as_bytes());
        hasher.update(&fingerprint(&track.path)?.to_le_bytes());
    }
    if let Some(nfo) = audiobook_folder::discover_nfo(folder)? {
        hasher.update(nfo.name.as_bytes());
        hasher.update(&fingerprint(&nfo.path)?.to_le_bytes());
    }
    if let Some(cue) = audiobook_folder::discover_cue(folder)? {
        hasher.update(cue.name.as_bytes());
        hasher.update(&fingerprint(&cue.path)?.to_le_bytes());
    }
    if let Some(cover) = audiobook_folder::discover_cover(folder)? {
        hasher.update(cover.name.as_bytes());
        hasher.update(&fingerprint(&cover.path)?.to_le_bytes());
    }
    Ok(u64::from_le_bytes(hasher.finalize().as_bytes()[..8].try_into().unwrap()))
}

fn walk_error(error: walkdir::Error) -> std::io::Error {
    let message = error.to_string();
    error.into_io_error().unwrap_or_else(|| std::io::Error::other(message))
}
