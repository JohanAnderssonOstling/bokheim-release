use super::TransferStatus;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{Error, ErrorKind, Write};
use std::path::{Path, PathBuf};

const FORMAT_VERSION: u32 = 1;
pub(crate) const HISTORY_LIMIT: usize = 50;

#[derive(Debug, Deserialize, Serialize)]
struct TransferHistoryFile<T = VecDeque<TransferStatus>> {
    version: u32,
    entries: T,
}

#[derive(Debug)]
pub struct MessagePackTransferHistory {
    path: PathBuf,
}

impl MessagePackTransferHistory {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub(crate) fn load(&self) -> Result<VecDeque<TransferStatus>, Box<dyn std::error::Error>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(VecDeque::new()),
            Err(error) => return Err(error.into()),
        };
        let mut file: TransferHistoryFile = rmp_serde::from_slice(&bytes).map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        if file.version != FORMAT_VERSION {
            return Err(Error::new(ErrorKind::InvalidData, format!("unsupported transfer history version {}", file.version)).into());
        }
        file.entries.truncate(HISTORY_LIMIT);
        Ok(file.entries)
    }

    // A single history writer serializes atomic snapshot book.
    pub(crate) fn store(&self, entries: &VecDeque<TransferStatus>) -> Result<(), Box<dyn std::error::Error>> {
        let bytes = rmp_serde::to_vec_named(&TransferHistoryFile { version: FORMAT_VERSION, entries })?;
        let parent = self.path.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temporary = self.path.with_extension("msgpack.new");
        {
            let mut file = std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        // Same-directory replacement keeps the previous file visible until
        // the fully written temporary file is published.
        std::fs::rename(&temporary, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{TransferOrigin, TransferState};
    use super::*;
    use library_replica::TransferJobKind;
    use sync_common::ContentHash;

    fn status(id: usize) -> TransferStatus {
        TransferStatus {
            id: format!("history-{id}"),
            kind: TransferJobKind::DownloadBook,
            content_hash: ContentHash::new(&format!("{id:064x}")),
            state: TransferState::Completed,
            origin: TransferOrigin::UserInitiated,
            attempts: 1,
            completed_items: 1,
            total_items: 1,
            failed_content_hash: None,
            file_name: None,
            last_error: None,
            next_retry_at: None,
            created_at: id.to_string(),
            updated_at: id.to_string(),
        }
    }

    #[test]
    fn named_messagepack_history_survives_recreation_and_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("transfer-history-v1.msgpack");
        let history = MessagePackTransferHistory::new(&path);
        history.store(&(0..55).rev().map(status).collect()).unwrap();
        let restored = MessagePackTransferHistory::new(path).load().unwrap();
        assert_eq!(restored.len(), HISTORY_LIMIT);
        assert_eq!(restored.front().unwrap().id, "history-54");
        assert_eq!(restored.back().unwrap().id, "history-5");
    }

    #[test]
    fn corrupt_history_is_non_authoritative() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("transfer-history-v1.msgpack");
        std::fs::write(&path, b"not messagepack").unwrap();
        assert!(MessagePackTransferHistory::new(path).load().is_err());
    }
    #[test]
    fn failed_staging_preserves_the_previous_history() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("history.msgpack");
        let history = MessagePackTransferHistory::new(&path);
        history.store(&VecDeque::from([status(1)])).unwrap();
        std::fs::create_dir(path.with_extension("msgpack.new")).unwrap();
        assert!(history.store(&VecDeque::from([status(2), status(1)])).is_err());
        let restored = history.load().unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, "history-1");
    }

    #[test]
    fn book_never_moves_a_directory_out_of_the_way() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("history.msgpack");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("retained"), b"user data").unwrap();
        let history = MessagePackTransferHistory::new(&path);
        assert!(history.store(&VecDeque::from([status(1)])).is_err());
        assert_eq!(std::fs::read(path.join("retained")).unwrap(), b"user data");
    }
}

pub(crate) mod writer;

/// Ordered persistence of bounded completed-transfer snapshots.
/// Implementations must preserve submit order and report pending write errors in flush.
pub trait HistoryPersistence: Send + Sync {
    fn submit(&self, entries: VecDeque<TransferStatus>);
    fn flush(&self) -> std::io::Result<()>;
}

impl HistoryPersistence for writer::HistoryWriter {
    fn submit(&self, entries: VecDeque<TransferStatus>) {
        self.submit(entries);
    }
    fn flush(&self) -> std::io::Result<()> {
        self.flush()
    }
}
