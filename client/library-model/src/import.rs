//! Progress of an explicit import, separate from filesystem scan progress.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportFileStage {
    Preparing,
    Copying { copied_bytes: u64 },
    Saving,
    Identifying,
    ReadingMetadata,
    Publishing,
    AddingToLibrary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportFileProgress {
    pub name: String,
    pub total_bytes: Option<u64>,
    pub stage: ImportFileStage,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryImportProgress {
    pub total: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub current: Option<ImportFileProgress>,
}
