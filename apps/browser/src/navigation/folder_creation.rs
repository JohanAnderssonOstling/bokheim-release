//! Shared review rules for filesystem roots and provider-backed directory trees.
use gpui::SelectedDirectory;
use library_backend::BookFormat;

fn eligible(path: &[String]) -> bool {
    !path.iter().any(|part| part.starts_with('.')) && path.last().and_then(|name| std::path::Path::new(name).extension()).and_then(|ext| ext.to_str()).and_then(BookFormat::from_import_extension).is_some()
}

pub(super) fn retain_eligible_files(directory: &mut SelectedDirectory) {
    directory.directories.retain(|path| !path.iter().any(|part| part.starts_with('.')));
    directory.files.retain(|file| eligible(&file.path) && file.size_bytes != Some(0));
}

/// Path-backed roots are estimated by the backend. Provider files already carry
/// metadata, so counting them never consumes their one-shot read capabilities.
pub(super) fn estimate_selected_files(directories: &[SelectedDirectory]) -> (u64, u64, u64) {
    let (mut books, mut bytes, mut unavailable) = (0, 0u64, 0);
    for directory in directories.iter().filter(|directory| directory.local_path.is_none()) {
        for file in &directory.files {
            if !eligible(&file.path) || file.size_bytes == Some(0) {
                continue;
            }
            books += 1;
            match file.size_bytes {
                Some(size) => bytes = bytes.saturating_add(size),
                None => unavailable += 1,
            }
        }
    }
    (books, bytes, unavailable)
}

pub(super) fn estimate_label((books, bytes, unavailable): (u64, u64, u64)) -> String {
    if unavailable > 0 && bytes == 0 {
        return format!("{books} eligible books · size unavailable");
    }
    format!(
        "{books} eligible books · {}{}{}",
        if unavailable > 0 { "at least " } else { "estimated " },
        crate::services::format_storage_bytes(bytes),
        if unavailable > 0 { format!(" · {unavailable} entries could not be checked") } else { String::new() }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(path: &[&str], size: Option<u64>) -> gpui::SelectedDirectoryFile {
        gpui::SelectedDirectoryFile::new(path.iter().map(|part| (*part).to_owned()).collect(), || panic!("estimation must not open a book")).with_size_bytes(size)
    }
    #[test]
    fn provider_estimate_preserves_file_capabilities_and_filters_like_native_scan() {
        let mut directory = SelectedDirectory {
            local_path: None,
            name: "Books".into(),
            directories: vec![],
            files: vec![file(&["Book.EPUB"], Some(100)), file(&["Audio.m4b"], Some(200)), file(&["Unknown.pdf"], None), file(&[".hidden", "hidden.epub"], Some(999)), file(&["notes.txt"], Some(999)), file(&["empty.epub"], Some(0))],
        };
        retain_eligible_files(&mut directory);
        assert_eq!(directory.files.len(), 3);
        assert_eq!(estimate_selected_files(&[directory]), (3, 300, 1));
    }
    #[test]
    fn unavailable_sizes_are_not_shown_as_zero() {
        assert_eq!(estimate_label((2, 0, 2)), "2 eligible books · size unavailable");
        assert!(estimate_label((2, 1024, 1)).contains("at least"));
    }
    #[test]
    fn size_switches_to_gb_at_one_gb() {
        assert!(estimate_label((5, 500_000_000, 0)).contains("MB"));
        assert!(estimate_label((5, 1_000_000_000, 0)).contains("GB"));
    }
}
