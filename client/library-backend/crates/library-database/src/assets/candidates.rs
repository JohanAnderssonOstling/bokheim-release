use super::TransfersSql;
use crate::*;
use std::collections::HashMap;
use sync_common::ContentHash;

struct CandidateFound {
    work_id: i64,
    content_hash: ContentHash,
    row_id: Option<i64>,
    deleted_at: Option<i64>,
}

#[derive(Default)]
struct CandidateFlags {
    thumbnail_upload_allowed: bool,
    thumbnail_remote: bool,
    thumbnail_pending: bool,
    requested: bool,
    rejected: bool,
    file_work_pending: bool,
}

impl Database {
    pub(super) fn candidate_page_of(&self, after: i64, through: i64) -> Result<Vec<AssetCandidate>, DatabaseError> {
        let mut found = Vec::new();
        self.connection.transfer_candidate_page(after, through, |row| {
            found.push(CandidateFound { work_id: row.get(0)?, content_hash: ContentHash::new(&row.get::<_, String>(1)?), row_id: row.get(2)?, deleted_at: row.get(3)? });
            Ok(())
        })?;
        self.assemble_candidates(found)
    }

    pub fn candidate_row_of(&self, hash: &ContentHash) -> Result<Option<AssetCandidate>, DatabaseError> {
        let mut found = None;
        self.connection.transfer_candidate_row(hash.as_str(), |row| {
            found = Some(CandidateFound { work_id: row.get(0)?, content_hash: ContentHash::new(&row.get::<_, String>(1)?), row_id: row.get(2)?, deleted_at: row.get(3)? });
            Ok(())
        })?;
        Ok(self.assemble_candidates(found.into_iter().collect())?.pop())
    }

    fn assemble_candidates(&self, found: Vec<CandidateFound>) -> Result<Vec<AssetCandidate>, DatabaseError> {
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let hashes = serde_json::to_string(&found.iter().map(|candidate| candidate.content_hash.as_str()).collect::<Vec<_>>()).map_err(DatabaseError::operation)?;
        let mut flags = HashMap::new();
        self.connection.transfer_candidate_flags(&hashes, |row| {
            flags.insert(
                ContentHash::new(&row.get::<_, String>(0)?),
                CandidateFlags { thumbnail_upload_allowed: row.get(1)?, thumbnail_remote: row.get(2)?, thumbnail_pending: row.get(3)?, requested: row.get(4)?, rejected: row.get(5)?, file_work_pending: row.get(6)? },
            );
            Ok(())
        })?;
        found
            .into_iter()
            .map(|candidate| {
                let flag = flags.remove(&candidate.content_hash).unwrap_or_default();
                let path = self.book_path(&candidate.content_hash)?;
                Ok(AssetCandidate {
                    work_id: candidate.work_id,
                    content_hash: candidate.content_hash,
                    live: candidate.row_id.is_some() && candidate.deleted_at.is_none(),
                    path,
                    thumbnail_upload_allowed: flag.thumbnail_upload_allowed,
                    thumbnail_remote: flag.thumbnail_remote,
                    thumbnail_pending: flag.thumbnail_pending,
                    requested: flag.requested,
                    rejected: flag.rejected,
                    file_work_pending: flag.file_work_pending,
                })
            })
            .collect()
    }

    pub(super) fn book_path(&self, hash: &ContentHash) -> Result<Option<RelativeBookPath>, DatabaseError> {
        let paths = self.book_paths(hash)?;
        paths.into_iter().next().map(|path| RelativeBookPath::parse(&path).map_err(DatabaseError::operation)).transpose()
    }
}
