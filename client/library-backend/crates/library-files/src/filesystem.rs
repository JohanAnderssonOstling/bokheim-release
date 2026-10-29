//! Concrete native filesystem access for a library root.
//!
//! This module deliberately exposes bytes, paths, and directory entries only.
//! It does not classify books or persist scan decisions.

use client_platform_native::filesystem::retry_transient;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Error, ErrorKind};
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
    pub fn next(&mut self) -> Option<Result<DirectoryEntry, Error>> {
        self.entries.next().map(|entry| {
            let entry = entry.map_err(|error| {
                let message = error.to_string();
                error.into_io_error().unwrap_or_else(|| Error::other(message))
            })?;
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
    retry_transient(|| fs::read_to_string(path.as_ref()))
}
pub fn write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}
pub fn open(path: impl AsRef<Path>) -> std::io::Result<File> {
    retry_transient(|| File::open(path.as_ref()))
}

pub fn fingerprint(path: impl AsRef<Path>) -> std::io::Result<u64> {
    client_platform_native::file_fingerprint::file_fingerprint(path)
}

/// Fingerprint the ordered tracks as one scanned placement. Both the scanner
/// and download publisher use this so a fresh download is already up to date.
pub fn audiobook_fingerprint(folder: &Path, tracks: &[audiobook_archive::Track]) -> std::io::Result<u64> {
    let mut hasher = blake3::Hasher::new();
    for track in tracks {
        hasher.update(track.name.as_bytes());
        hasher.update(&fingerprint(&track.path)?.to_le_bytes());
    }
    let mapping = folder.join(audiobook_folder::PATH_MAPPING_FILE);
    if mapping.try_exists()? {
        hasher.update(audiobook_folder::PATH_MAPPING_FILE.as_bytes());
        hasher.update(&fingerprint(mapping)?.to_le_bytes());
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
    let [a, b, c, d, e, f, g, h, ..] = *hasher.finalize().as_bytes();
    Ok(u64::from_le_bytes([a, b, c, d, e, f, g, h]))
}

/// Places a file or complete folder before any book inspection begins.
/// A path already in this directory is a retry and stays in place.
pub fn place_into_directory(source: &Path, directory: &Path, occupied: &[String], moving: bool) -> io::Result<PathBuf> {
    place_into_directory_with_intent(source, directory, occupied, moving, |_| Ok(()))
}

/// Records each candidate destination before publishing it, so an owner can
/// recover a move interrupted between the rename and its database acknowledgement.
pub fn place_into_directory_with_intent(source: &Path, directory: &Path, occupied: &[String], moving: bool, mut before_publish: impl FnMut(&Path) -> io::Result<()>) -> io::Result<PathBuf> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(Error::other("only regular files and folders can be added to a library"));
    }
    let source = source.canonicalize()?;
    let directory = directory.canonicalize()?;
    if source.parent() == Some(directory.as_path()) {
        return Ok(source);
    }
    if directory.starts_with(&source) {
        return Err(Error::other("a folder cannot be placed inside itself"));
    }
    let name = source.file_name().and_then(OsStr::to_str).ok_or_else(|| Error::other("invalid file name"))?;
    let mut names = occupied.to_vec();
    names.extend(fs::read_dir(&directory)?.map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned())).collect::<Result<Vec<_>, _>>()?);
    let candidates: Box<dyn Iterator<Item = String> + '_> =
        if metadata.is_dir() { Box::new(library_replica::folder_name_candidates(name, names.iter().map(String::as_str))) } else { Box::new(library_replica::file_name_candidates(name, names.iter().map(String::as_str))) };
    let mut staging: Option<tempfile::TempDir> = None;
    for name in candidates {
        let destination = directory.join(name);
        if destination.symlink_metadata().is_ok() {
            continue;
        }
        before_publish(&destination)?;
        if moving && staging.is_none() {
            match rename_entry(&source, &destination) {
                Ok(()) => return Ok(destination),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) if error.kind() == ErrorKind::CrossesDevices => {}
                Err(error) => return Err(error),
            }
        }
        // A complete file or folder is copied under a hidden directory on the
        // destination filesystem. The scanner skips it, including after a crash.
        let temporary = match &mut staging {
            Some(temporary) => temporary,
            vacant @ None => {
                let temporary = tempfile::Builder::new().prefix(".bokheim-copy-").tempdir_in(&directory)?;
                copy_entry(&source, &temporary.path().join("contents"))?;
                vacant.insert(temporary)
            }
        };
        let staged = temporary.path().join("contents");
        match rename_entry(&staged, &destination) {
            Ok(()) => {
                // Cross-filesystem moves delete the original only after the
                // completed copy has been renamed into its final location.
                if moving {
                    if metadata.is_dir() {
                        fs::remove_dir_all(&source)?;
                    } else {
                        fs::remove_file(&source)?;
                    }
                }
                return Ok(destination);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!("numeric name suffix space is unbounded")
}

pub(crate) fn rename_entry(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(target_os = "android")]
    {
        // Filename candidates and destination checks above handle conflicts.
        // Shared storage may reject RENAME_NOREPLACE, so use regular rename.
        fs::rename(source, destination)
    }
    #[cfg(not(target_os = "android"))]
    {
        client_platform_native::filesystem::rename_noclobber(source, destination)
    }
}

pub(crate) fn copy_entry(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        let mut reader = File::open(source)?;
        let mut writer = fs::OpenOptions::new().write(true).create_new(true).open(destination)?;
        io::copy(&mut reader, &mut writer)?;
        fs::set_permissions(destination, metadata.permissions())?;
    } else {
        return Err(Error::other("cannot copy symbolic links or special files"));
    }
    Ok(())
}
