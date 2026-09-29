//! Decode browser progress into the library import model.
use crate::{ImportFailure, ImportFailureKind, ImportProgress, ImportState};
use serde::Deserialize;

/// The existing browser wire protocol is decoded exactly once into typed state.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireImportProgress {
    id: String,
    name: String,
    phase: WirePhase,
    copied_bytes: Option<u64>,
    total_bytes: Option<u64>,
    processed: Option<u64>,
    total_files: Option<u64>,
    failed: Option<u64>,
    error: Option<String>,
    storage_full: Option<bool>,
    #[serde(default)]
    failures: Option<Vec<ImportFailure>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum WirePhase {
    Queued,
    Copying,
    Adding,
    Failed,
    Complete,
}
impl TryFrom<WireImportProgress> for ImportProgress {
    type Error = &'static str;
    fn try_from(value: WireImportProgress) -> Result<Self, Self::Error> {
        let failures = value.failures.unwrap_or_default();
        let state = match value.phase {
            WirePhase::Queued => ImportState::Queued,
            WirePhase::Copying => {
                let copied_bytes = value.copied_bytes.ok_or("missing copied byte count")?;
                let total_bytes = value.total_bytes.ok_or("missing total byte count")?;
                if copied_bytes > total_bytes {
                    return Err("copied byte count exceeds total");
                }
                ImportState::Copying { copied_bytes, total_bytes }
            }
            WirePhase::Adding => {
                let processed = value.processed.ok_or("missing processed file count")?;
                let total = value.total_files.ok_or("missing total file count")?;
                let failed = value.failed.unwrap_or_default();
                if failed > processed || processed > total {
                    return Err("invalid import file counts");
                }
                ImportState::Adding { processed, total, failed }
            }
            WirePhase::Failed => {
                ImportState::Paused { reason: ImportFailure { kind: if value.storage_full == Some(true) { ImportFailureKind::StorageFull } else { ImportFailureKind::Interrupted }, path: Vec::new(), diagnostic: value.error } }
            }
            WirePhase::Complete => {
                let total = value.total_files.ok_or("missing total file count")?;
                let failed_files = value.failed.unwrap_or_else(|| failures.iter().filter(|failure| failure.kind == ImportFailureKind::File).count() as u64);
                if failed_files > total {
                    return Err("failed file count exceeds total");
                }
                ImportState::Complete { succeeded: total - failed_files, failed: (failures.len() as u64).max(failed_files) }
            }
        };
        Ok(Self { id: value.id, name: value.name, state, failures })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decode(json: &str) -> Result<ImportProgress, String> {
        let wire: WireImportProgress = serde_json::from_str(json).map_err(|error| error.to_string())?;
        wire.try_into().map_err(str::to_owned)
    }
    #[test]
    fn progress_decodes_counts_without_presentation() {
        let progress = decode(r#"{"id":"job","name":"Books","phase":"copying","copiedBytes":25,"totalBytes":100}"#).unwrap();
        assert_eq!(progress.state, ImportState::Copying { copied_bytes: 25, total_bytes: 100 });
        let value = serde_json::to_value(progress).unwrap();
        assert!(value.get("label").is_none() && value.get("percent").is_none() && value.get("phase").is_none());
        assert!(decode(r#"{"id":"job","name":"Books","phase":"unknown"}"#).is_err());
        assert!(decode(r#"{"id":"job","name":"Books","phase":"copying","copiedBytes":101,"totalBytes":100}"#).is_err());
        assert!(decode(r#"{"id":"job","name":"Books","phase":"adding","processed":1.5,"totalFiles":2}"#).is_err());
    }
    #[test]
    fn legacy_completed_failures_remain_readable_as_diagnostics() {
        let progress = decode(r#"{"id":"job","name":"Books","phase":"complete","totalFiles":5,"failed":1,"failures":["old display string",{"kind":"folder","path":["Shelf"],"diagnostic":"io"}]}"#).unwrap();
        assert_eq!(progress.state, ImportState::Complete { succeeded: 4, failed: 2 });
        assert_eq!(progress.failures[0].kind, ImportFailureKind::Legacy);
        assert_eq!(progress.failures[1].kind, ImportFailureKind::Folder);
        let json = serde_json::to_string(&progress).unwrap();
        assert_eq!(serde_json::from_str::<ImportProgress>(&json).unwrap(), progress);
    }
    #[test]
    fn failed_progress_only_identifies_confirmed_full_storage() {
        let full = decode(r#"{"id":"job","name":"Books","phase":"failed","error":"QuotaExceededError","storageFull":true}"#).unwrap();
        assert!(matches!(full.state, ImportState::Paused { reason: ImportFailure { kind: ImportFailureKind::StorageFull, .. } }));
        let interrupted = decode(r#"{"id":"job","name":"Books","phase":"failed","error":"network interrupted"}"#).unwrap();
        assert!(matches!(interrupted.state, ImportState::Paused { reason: ImportFailure { kind: ImportFailureKind::Interrupted, .. } }));
    }
}
