//! Import display text and percentages, independent of notification widgets.

pub(super) fn browser_presentation(state: &app::ImportState) -> (String, Option<f32>) {
    use app::ImportState;
    let percent = |done: u64, total: u64| (total > 0).then(|| (100. * done as f64 / total as f64).clamp(0., 100.) as f32);
    match state {
        ImportState::Queued => ("Preparing import…".into(), None),
        ImportState::Copying { copied_bytes, total_bytes } => ("Copying books to browser storage…".into(), percent(*copied_bytes, *total_bytes)),
        ImportState::Adding { processed, total, .. } => (format!("Adding books: {processed} of {total}"), percent(*processed, *total)),
        ImportState::Paused { reason } => (import_failure_message(reason), None),
        ImportState::Rejected { .. } => ("Could not start the import. Select the files and try again.".into(), None),
        ImportState::Complete { failed, .. } => (
            match failed {
                0 => "Import complete".into(),
                1 => "Import complete. One item could not be imported.".into(),
                n => format!("Import complete. {n} items could not be imported."),
            },
            None,
        ),
    }
}

pub(crate) fn import_failure_message(failure: &app::ImportFailure) -> String {
    use app::ImportFailureKind;
    match failure.kind {
        ImportFailureKind::File => match failure.path.last() {
            Some(name) => format!("Could not import “{name}”"),
            None => "Could not import a book".into(),
        },
        ImportFailureKind::Folder => {
            if let Some(hidden) = failure.path.iter().position(|part| part.starts_with('.')) {
                return match hidden.checked_sub(1).and_then(|i| failure.path.get(i)) {
                    Some(parent) => format!("A folder in “{parent}” could not be imported"),
                    None => "A folder could not be imported".into(),
                };
            }
            match failure.path.last() {
                Some(name) => format!("Could not import folder “{name}”"),
                None => "A folder could not be imported".into(),
            }
        }
        ImportFailureKind::Interrupted => "Import paused. Try again to continue.".into(),
        ImportFailureKind::StorageFull => "Browser storage is full. Free up space, then retry the import.".into(),
        ImportFailureKind::Submission => "Could not start the import. Select the files and try again.".into(),
        ImportFailureKind::Legacy => "An item could not be imported.".into(),
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    use app::{ImportFailure, ImportFailureKind, ImportState};
    #[test]
    fn typed_progress_formats_in_the_ui() {
        assert_eq!(browser_presentation(&ImportState::Copying { copied_bytes: 25, total_bytes: 100 }).1, Some(25.));
        assert_eq!(browser_presentation(&ImportState::Copying { copied_bytes: 0, total_bytes: 0 }).1, None);
        assert_eq!(browser_presentation(&ImportState::Adding { processed: 4, total: 10, failed: 1 }), ("Adding books: 4 of 10".into(), Some(40.)));
        assert_eq!(browser_presentation(&ImportState::Complete { succeeded: 9, failed: 1 }).0, "Import complete. One item could not be imported.");
    }
    #[test]
    fn failure_messages_hide_internal_paths_and_diagnostics() {
        let failure = ImportFailure { kind: ImportFailureKind::Folder, path: vec!["Diplomati".into(), ".bookrium".into(), "internal-id".into()], diagnostic: Some("SQL diagnostic".into()) };
        assert_eq!(import_failure_message(&failure), "A folder in “Diplomati” could not be imported");
        let failure = ImportFailure { kind: ImportFailureKind::File, path: vec!["private".into(), "Book.epub".into()], diagnostic: Some("SQL diagnostic".into()) };
        assert_eq!(import_failure_message(&failure), "Could not import “Book.epub”");
    }
    #[test]
    fn paused_import_explains_full_browser_storage() {
        let reason = ImportFailure { kind: ImportFailureKind::StorageFull, path: Vec::new(), diagnostic: Some("private browser error".into()) };
        assert_eq!(browser_presentation(&ImportState::Paused { reason }).0, "Browser storage is full. Free up space, then retry the import.");
    }
}
