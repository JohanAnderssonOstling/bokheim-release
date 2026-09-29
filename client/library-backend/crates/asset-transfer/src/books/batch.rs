use super::*;
use library_database::BookUploadIntent;
use sync_common::{
    api::assets::{BlobManifestEntry, BlobManifestRequest},
    book_batch as contract,
};

impl<C, S> BookTransfers<C, S>
where
    C: Fn() -> Result<sync_transport::SyncCredentials, TransferError> + Sync,
    S: Fn(ContentHash, DownloadState) + Sync,
{
    pub async fn upload_batch(&self, snapshot: &library_database::TransferSnapshot, intents: &[BookUploadIntent], out: &mut library_database::TransferOutcome) -> Result<Vec<Result<(), TransferError>>, TransferError> {
        let credentials = self.credentials()?;
        let mut outcomes = (0..intents.len()).map(|_| Ok(())).collect::<Vec<_>>();
        let mut prepared = Vec::new();
        let mut entries = Vec::new();
        let mut trace = sync_transport::PerformanceTrace::new("book_batch_upload", "verify_local_files");
        for (index, intent) in intents.iter().enumerate() {
            let result = async {
                let asset = snapshot.asset_for(&intent.content_hash).cloned().ok_or_else(|| TransferError::retryable("transfer snapshot changed during drain"))?;
                let versions = asset.upload.local_versions;
                if asset.upload.intent.as_ref() != Some(intent) {
                    return Ok(None);
                }
                let paths = asset.upload_paths;
                let book = self.assets.verified_book_version_reader_at_paths(intent.content_hash, Some(intent.checksum), paths).await?.ok_or_else(|| TransferError::rejected("local book is unavailable or invalid"))?;
                if book.length != intent.size_bytes || book.checksum != intent.checksum {
                    return Err(TransferError::retryable("book changed before batch upload"));
                }
                Ok(Some((book, versions)))
            }
            .await;
            match result {
                Ok(Some((book, versions))) => {
                    entries.push(BlobManifestEntry { content_hash: intent.content_hash, checksum: intent.checksum, size_bytes: intent.size_bytes });
                    prepared.push((index, book, versions));
                }
                Ok(None) => {}
                Err(error) => outcomes[index] = Err(error),
            }
        }
        if entries.is_empty() {
            trace.finish(true);
            return Ok(outcomes);
        }
        let payload_bytes = contract::payload_bytes(&entries).ok_or_else(|| TransferError::retryable("invalid upload batch size"))?;
        let manifest = sync_common::transport::encode(&BlobManifestRequest { blobs: entries.clone() }).map_err(TransferError::retryable)?;
        if manifest.len() > contract::MAX_MANIFEST_BYTES {
            return Err(TransferError::retryable("upload batch manifest is too large"));
        }
        let length = 4 + manifest.len() as u64 + payload_bytes;
        let mut prefix = (manifest.len() as u32).to_be_bytes().to_vec();
        prefix.extend(manifest);
        type ChunkStream = std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Vec<u8>, std::io::Error>> + Send>>;
        let mut streams: Vec<ChunkStream> = vec![Box::pin(futures_util::stream::once(async move { Ok(prefix) }))];
        let mut completion = Vec::new();
        for (index, book, versions) in prepared {
            streams.push(Box::pin(book.into_upload_stream()));
            completion.push((index, versions));
        }
        trace.phase("http_batch");
        let response =
            transfer_deadline(upload_timeout(length), sync_transport::upload_book_batch(&self.http_client, &credentials, &self.library_id, &entries, length, reqwest::Body::wrap_stream(futures_util::stream::iter(streams).flatten())))
                .await?
                .map_err(|error| match error {
                    sync_transport::SyncRequestError::AuthenticationRequired => TransferError::AuthenticationRequired,
                    error => TransferError::retryable(error),
                })?;
        self.check_transfer_account(&credentials)?;
        let Some(response) = response else {
            trace.phase("individual_fallback");
            for (index, _) in completion {
                let intent = &intents[index];
                outcomes[index] = self.upload_blob_version(snapshot, &intent.content_hash, Some(intent), true, out).await;
            }
            trace.finish(true);
            return Ok(outcomes);
        };
        trace.phase("local_completion_transaction");
        self.check_transfer_account(&credentials)?;
        let mut successful = Vec::new();
        for ((index, versions), result) in completion.iter().zip(response.results) {
            let index = *index;
            let intent = &intents[index];
            if result.status != 200 {
                outcomes[index] = Err(match sync_transport::classify_asset_status(result.status) {
                    sync_transport::AssetStatus::AuthenticationRequired => TransferError::AuthenticationRequired,
                    sync_transport::AssetStatus::Rejected => TransferError::rejected(format!("batch book upload HTTP {}", result.status)),
                    _ => TransferError::retryable(format!("batch book upload HTTP {}", result.status)),
                });
                continue;
            }
            successful.push((index, library_database::BookUploadCompletion { content_hash: intent.content_hash, local_versions: versions.clone(), intent: intent.clone() }));
        }
        let completed = successful.iter().map(|(_, completion)| completion.clone()).collect::<Vec<_>>();
        // Currency is re-validated when the orchestrator commits this record;
        // a revision that changed mid-drain is rejected there and the next
        // planning pass retries the current version.
        out.record(library_database::TransferWrite::CompleteUploadedBatch { completions: completed });
        trace.finish(true);
        Ok(outcomes)
    }
}
