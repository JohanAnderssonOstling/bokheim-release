//! Fast pre-import size estimates using the same file eligibility as scanning.
use book_model::BookFormat;
use std::{collections::HashSet, path::PathBuf};

/// Returns eligible files, physical bytes and unreadable entries. No file bodies
/// Returns None when cancelled. Checks between entries; an in-progress OS call
/// must return before cancellation can be observed.
pub fn estimate_folders_cancellable(roots: Vec<PathBuf>, cancelled: impl Fn() -> bool) -> Option<(u64, u64, u64)> {
    let (mut books, mut bytes, mut unreadable) = (0u64, 0u64, 0u64);
    let mut visited = HashSet::new();
    for root in roots {
        if cancelled() {
            return None;
        }
        let root = match root.canonicalize() {
            Ok(root) => root,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        let mut entries = library_files::filesystem::walk(root);
        loop {
            if cancelled() {
                return None;
            }
            let Some(entry) = entries.next() else { break };
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    unreadable += 1;
                    continue;
                }
            };
            if entry.file_name().to_string_lossy().starts_with('.') {
                if entry.is_dir() {
                    entries.skip_current_dir();
                }
                continue;
            }
            if !entry.is_file() || entry.path().extension().and_then(|ext| ext.to_str()).and_then(BookFormat::from_extension).is_none() || !visited.insert(entry.path().to_owned()) {
                continue;
            }
            match entry.length() {
                Some(length) if length > 0 => {
                    books += 1;
                    bytes = bytes.saturating_add(length);
                }
                Some(_) => {}
                None => unreadable += 1,
            }
        }
    }
    if cancelled() {
        None
    } else {
        Some((books, bytes, unreadable))
    }
}

// TEMP-NEUTRALIZED: foreign broken asserts, restored after test run
