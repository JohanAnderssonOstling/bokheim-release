//! Host transfer coordination. Browser ownership survives requesting-tab loss;
//! native execution is owned by the application's transfer queue.
use super::worker::TransferWorker;
use crate::ClaimedTransferJob;
use crate::{TransferError, TransferJob, TransferOperation};
#[cfg(not(target_arch = "wasm32"))]
use futures_util::StreamExt;
use library_database::{TransferOutcome, TransferSnapshot};
#[cfg(target_arch = "wasm32")]
use library_model::{DownloadProgress, DownloadState};
#[cfg(target_arch = "wasm32")]
use library_replica::BlobKind;
#[cfg(target_arch = "wasm32")]
use library_runtime::events::LibraryEvent;

#[cfg(not(target_arch = "wasm32"))]
impl TransferWorker {
    pub(super) async fn upload_claimed_books_with_refresh(&self, snapshot: &TransferSnapshot, claimed: &[ClaimedTransferJob], out: &mut TransferOutcome) -> Result<Vec<Result<(), TransferError>>, TransferError> {
        if !self.asset_storage_enabled() {
            return Ok(claimed.iter().map(|_| Ok(())).collect());
        }
        let _interest = self.notification_interest.work();
        let intents = claimed
            .iter()
            .map(|item| match &item.job.operation {
                TransferOperation::UploadBlob { intent: Some(intent), negotiated: true, .. } => Ok(intent.clone()),
                _ => Err(TransferError::retryable("invalid upload batch job")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if intents.iter().any(|intent| intent.size_bytes > sync_common::book_batch::MAX_BOOK_BYTES) {
            // The batch endpoint cannot carry these books. Each upload owns a
            // fresh snapshot and commits as soon as its HTTP request finishes,
            // so progress does not wait for the rest of the claimed group.
            let results = futures_util::stream::iter(claimed.iter().map(|item| {
                let job = item.job.clone();
                async move {
                    let guard = self.refresh_guard(super::RefreshScope::Planned)?;
                    match self.execute_owned_job(job.clone()).await {
                        Err(TransferError::AuthenticationRequired) => {
                            self.refresh_after_auth(guard).await?;
                            self.execute_owned_job(job).await
                        }
                        result => result,
                    }
                }
            }))
            .buffered(crate::runner::MAX_CONCURRENT_LARGE_UPLOADS)
            .collect()
            .await;
            return Ok(results);
        }
        let guard = self.refresh_guard(super::RefreshScope::Planned)?;
        let result = self.book_transfers().upload_batch(snapshot, &intents, out).await;
        if !matches!(result, Err(TransferError::AuthenticationRequired)) {
            return result;
        }
        self.refresh_after_auth(guard).await?;
        self.book_transfers().upload_batch(snapshot, &intents, out).await
    }

    #[cfg(test)]
    pub(crate) async fn handle_job(&self, job: TransferJob) -> Result<(), TransferError> {
        self.execute_owned_job(job).await
    }
}

#[cfg(target_arch = "wasm32")]
impl TransferWorker {
    pub(super) async fn upload_claimed_books_with_refresh(&self, snapshot: &TransferSnapshot, claimed: &[ClaimedTransferJob], out: &mut TransferOutcome) -> Result<Vec<Result<(), TransferError>>, TransferError> {
        crate::runner::TransferHost::execute_uploads_sequentially(self, snapshot, claimed, out).await
    }

    pub(crate) async fn handle_job(&self, job: TransferJob) -> Result<(), TransferError> {
        if let TransferOperation::DownloadBlob { content_hash, .. } = &job.operation {
            let credentials = self.credentials()?;
            let user = self.live_user_id().unwrap_or_default();
            let key = format!("{}|{}|{}|download|{}", credentials.server_url(), user, self.library_id, content_hash);
            for attempt in 0..2 {
                let mut worker = self.clone();
                worker.progress_key = Some(key.clone());
                let owned_job = job.clone();
                let expected_user = user.clone();
                let original = job.clone();
                // The coordinator retains the executor even if one requesting tab disconnects.
                let run = move || -> client_platform_web::transport::BytesFuture {
                    Box::pin(async move {
                        let result = async {
                            if worker.live_user_id().unwrap_or_default() != expected_user {
                                return Err(TransferError::retryable("account changed before download execution"));
                            }
                            worker.execute_owned_job(owned_job).await
                        }
                        .await;
                        let abandoned = if result.is_ok() {
                            if let TransferOperation::DownloadBlob { content_hash, placement } = &original.operation {
                                worker.commit_connection().ok().and_then(|database| database.book_placement_expects_hash(placement, content_hash).ok()).is_some_and(|live| !live)
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        rmp_serde::to_vec_named(&(result, abandoned)).map_err(|error| error.to_string())
                    })
                };
                let event_tx = self.event_tx.clone();
                let progress_hash = *content_hash;
                let progress = move |fraction: f64| {
                    if let Some(tx) = &event_tx {
                        let value = DownloadProgress::new(fraction as f32).unwrap_or(DownloadProgress::ZERO);
                        let _ = tx.try_send(LibraryEvent::DownloadStatusChanged { content_hash: progress_hash, state: DownloadState::Downloading(value) });
                    }
                };
                let response = client_platform_web::transport::bridge::transfer_request_with_progress(key.clone(), run, progress).map_err(|error| TransferError::retryable(format!("transfer coordinator unavailable: {error:?}")))?;
                let bytes = response.await.map_err(TransferError::retryable)?;
                let (result, abandoned): (Result<(), TransferError>, bool) = rmp_serde::from_slice(&bytes).map_err(TransferError::retryable)?;
                if self.live_user_id().unwrap_or_default() != user {
                    return Err(TransferError::retryable("account changed during shared download"));
                }
                // Followers need their own committed-state notification even
                // when another tab executed the job and produced its outcome.
                let state = self.commit_connection()?.transfer_download_state(content_hash, self.assets.exists(BlobKind::Book, content_hash)?)?;
                self.send_download_status(*content_hash, state);
                result?;
                if let TransferOperation::DownloadBlob { content_hash, placement } = &job.operation {
                    // The first executor may have lost its placement during HTTP.
                    // A follower with a still-live placement gets one fresh pass.
                    if abandoned && !self.assets.exists(BlobKind::Book, content_hash)? {
                        let live = self.commit_connection().ok().and_then(|database| database.book_placement_expects_hash(placement, content_hash).ok()).unwrap_or(false);
                        if live {
                            if attempt == 0 {
                                continue;
                            }
                            return Err(TransferError::retryable("shared download finished without book bytes"));
                        }
                    }
                }
                return Ok(());
            }
            unreachable!("shared download attempts return or exhaust with an error");
        }
        self.execute_owned_job(job).await
    }
}
