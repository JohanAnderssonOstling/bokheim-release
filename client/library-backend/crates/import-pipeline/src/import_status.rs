//! Application import state. Presentation and localization belong to the UI.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportProgress {
    pub id: String,
    pub name: String,
    pub state: ImportState,
    pub failures: Vec<ImportFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImportState {
    Queued,
    Copying { copied_bytes: u64, total_bytes: u64 },
    Adding { processed: u64, total: u64, failed: u64 },
    Paused { reason: ImportFailure },
    Rejected { reason: ImportFailure },
    Complete { succeeded: u64, failed: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportFailureKind {
    File,
    Folder,
    Interrupted,
    StorageFull,
    Submission,
    Legacy,
}

/// A classified failure, with its source path and optional diagnostic detail.
/// Diagnostics are for logging/support; they are not a user-facing message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ImportFailure {
    pub kind: ImportFailureKind,
    pub path: Vec<String>,
    pub diagnostic: Option<String>,
}
impl ImportFailure {
    pub fn operation(kind: ImportFailureKind, path: &[String], diagnostic: impl Into<String>) -> Self {
        Self { kind, path: path.to_vec(), diagnostic: Some(diagnostic.into()) }
    }
}

// Completed journals written before typed failures contained display strings.
// Retain them only as diagnostics; never require the UI to parse their wording.
impl<'de> Deserialize<'de> for ImportFailure {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Typed {
                kind: ImportFailureKind,
                #[serde(default)]
                path: Vec<String>,
                #[serde(default)]
                diagnostic: Option<String>,
            },
            Legacy(String),
        }
        Ok(match Stored::deserialize(deserializer)? {
            Stored::Typed { kind, path, diagnostic } => Self { kind, path, diagnostic },
            Stored::Legacy(diagnostic) => Self { kind: ImportFailureKind::Legacy, path: Vec::new(), diagnostic: Some(diagnostic) },
        })
    }
}
