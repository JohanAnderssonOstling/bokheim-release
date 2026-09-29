//! One bounded Audible attempt per member of the current enrichment batch.
use crate::library::LibrarySession;
use crate::BackendError;
use metadata_contract::audible::{LookupResponse, LookupStatus};
use web_time::{SystemTime, UNIX_EPOCH};

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

impl LibrarySession {
    /// Attempt each eligible member once; retry deadlines never extend this batch.
    pub(crate) async fn run_audible_batch(&self, scope: &std::collections::HashSet<crate::ContentHash>) -> Result<(), BackendError> {
        let members: Vec<_> = scope.iter().copied().collect();
        let candidates = self.db.audible_batch_candidates(now(), &members).map_err(BackendError::operation)?;
        for hash in candidates {
            let job = self.db.claim_audible_batch(now(), hash).map_err(BackendError::operation)?;
            let Some(job) = job else { continue };
            let response = self.metadata().audible_lookup(&job.request).await.unwrap_or_else(|error| LookupResponse {
                status: LookupStatus::Unavailable,
                candidates: vec![],
                selected_asin: None,
                selected_region: None,
                chapter_plan: None,
                used_website_fallback: false,
                detail: error.to_string(),
            });
            let committed = self.db.resolve_audible_batch(&job, &response, now() + 300, now()).map_err(BackendError::operation)?;
            if committed {
                if response.status == LookupStatus::Unavailable {
                    log::warn!("Audiobook enrichment unavailable for {}: {}; retry scheduled", job.request.title, response.detail);
                } else {
                    log::info!("Audiobook enrichment for {}: {:?}: {}", job.request.title, response.status, response.detail);
                }
                if response.status == LookupStatus::Supported {
                    // Refresh readers without emitting a new preparation request.
                    self.publish_update(library_model::LibraryUpdate::Contents);
                }
                self.sync_wake.wake();
            }
        }
        Ok(())
    }

    #[cfg(test)]
    async fn run_audible_jobs(&self) -> Result<(), BackendError> {
        let snapshot = self.db.upload_preparation_snapshot().map_err(BackendError::operation)?;
        let scope = self.db.prepared_upload_batch(&snapshot.hashes()).map_err(BackendError::operation)?;
        self.run_audible_batch(&scope.hashes()).await
    }
}
