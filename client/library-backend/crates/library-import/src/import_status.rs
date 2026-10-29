//! Import failures. Presentation and localization belong to the UI.
use client_runtime::{Classified, Failure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportFailureKind {
    File,
    Folder,
}

/// A classified failure, with its source path and optional diagnostic detail.
/// Diagnostics are for logging/support; they are not a user-facing message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportFailure {
    pub kind: ImportFailureKind,
    pub path: Vec<String>,
    pub diagnostic: Option<String>,
    #[serde(default)]
    pub failure: Option<Failure>,
}
impl ImportFailure {
    pub fn file(path: &[String], error: &(impl Classified + ?Sized)) -> Self {
        Self::classified(ImportFailureKind::File, path, error)
    }

    pub fn folder(path: &[String], error: &(impl Classified + ?Sized)) -> Self {
        Self::classified(ImportFailureKind::Folder, path, error)
    }

    fn classified(kind: ImportFailureKind, path: &[String], error: &(impl Classified + ?Sized)) -> Self {
        let description = match kind {
            ImportFailureKind::File => "",
            ImportFailureKind::Folder => "folder ",
        };
        log::warn!("Could not import {description}{}: {error}", path.join("/"));
        Self { kind, path: path.to_vec(), diagnostic: Some(error.to_string()), failure: error.failure() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_runtime::BackendError;

    #[test]
    fn import_failures_keep_actionable_causes_separate_from_diagnostics() {
        let error = BackendError::operation(Failure::CloudStorageFull).context("retained pending provider import");
        let path = ["Book.epub".into()];
        for (kind, failure) in [(ImportFailureKind::File, ImportFailure::file(&path, &error)), (ImportFailureKind::Folder, ImportFailure::folder(&path, &error))] {
            let received: ImportFailure = serde_json::from_slice(&serde_json::to_vec(&failure).unwrap()).unwrap();
            assert_eq!(received.kind, kind);
            assert_eq!(received.failure, Some(Failure::CloudStorageFull));
            assert_eq!(received.path, vec!["Book.epub"]);
            assert_eq!(received.diagnostic.as_deref(), Some(error.to_string().as_str()));
        }
    }
}
