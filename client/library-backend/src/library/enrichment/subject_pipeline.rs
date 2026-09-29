//! Execute shared policy in bounded background batches. File/HTTP work never
//! runs inside a database command, and successful ISBN responses serve authors too.
use super::{enrichment_parallelism, OPEN_LIBRARY_EDITION_IDENTITY_PROVIDER, OPEN_LIBRARY_RICH_PROVIDER};

use crate::{library::LibrarySession, BackendError};
use metadata_contract::MetadataEnrichmentRequest;
use std::collections::{HashMap, HashSet};

use book_enrichment::{SubjectPhase, SubjectStep};
use futures_util::{stream, StreamExt};

impl LibrarySession {
    pub(super) async fn cached_metadata_enrichment(&self, request: &MetadataEnrichmentRequest) -> Result<metadata_contract::RichMetadataEnrichmentResponse, BackendError> {
        let requested = request.isbns.clone();
        let cached = self.db.cached_isbn_enrichment(requested).map_err(BackendError::operation)?;
        let mut responses = Vec::new();
        let mut missing = Vec::new();
        for (isbn, response) in cached {
            if let Some(response) = response {
                let response: metadata_contract::RichMetadataEnrichmentResponse = serde_json::from_str(&response).map_err(BackendError::operation)?;
                responses.push(response);
            } else {
                missing.push(isbn);
            }
        }
        if !missing.is_empty() {
            let response = self.metadata().enrichment(&MetadataEnrichmentRequest { isbns: missing.clone() }).await.map_err(BackendError::operation)?;
            for result in &response.results {
                if !missing.contains(&result.requested_isbn) {
                    continue;
                }
                // Store only this ISBN's record, rather than cloning the whole
                // batch once per ISBN.
                let single = metadata_contract::RichMetadataEnrichmentResponse { snapshot: response.snapshot.clone(), results: vec![result.clone()] };
                let isbn = result.requested_isbn.clone();
                let json = serde_json::to_string(&single).map_err(BackendError::operation)?;
                self.db.cache_isbn_json(library_database::IsbnCacheOutcome { isbn, json }).map_err(BackendError::operation)?;
            }
            responses.push(response);
        }
        let Some(mut combined) = responses.pop() else { return self.metadata().enrichment(request).await.map_err(BackendError::operation) };
        for response in responses {
            combined.results.extend(response.results);
        }
        combined.results.sort_by(|a, b| a.requested_isbn.cmp(&b.requested_isbn));
        combined.results.dedup_by(|a, b| a.requested_isbn == b.requested_isbn);
        Ok(combined)
    }

    pub(super) async fn enrich_subject_pipeline(&self) -> Result<usize, BackendError> {
        let batch = self.db.upload_preparation_snapshot().map_err(BackendError::operation)?;
        self.enrich_subject_batch(&batch.hashes()).await
    }

    pub(super) async fn enrich_subject_batch(&self, scope: &HashSet<crate::ContentHash>) -> Result<usize, BackendError> {
        let mut updated = 0;
        // Keep detailed per-book progress in memory while this run moves
        // through the library-wide phases. SQLite stores only the current
        // phase, so a restart repeats work from that phase's beginning.
        let mut running_pipelines = HashMap::<crate::ContentHash, book_enrichment::SubjectPipeline>::new();
        let scope_json = std::sync::Arc::new(serde_json::to_string(scope).map_err(BackendError::operation)?);
        let through = scope.iter().map(|hash| hash.as_str()).max();
        for phase in SubjectPhase::ALL {
            let mut after = String::new();
            loop {
                if through.is_none_or(|last| !after.is_empty() && after.as_str() >= last) {
                    break;
                }
                let cursor = after.clone();
                let mut jobs = self.db.subject_enrichment_candidates(&cursor, &scope_json).map_err(BackendError::operation)?;
                let Some(last) = jobs.last() else { break };
                after = last.hash.to_string();
                for job in &mut jobs {
                    if let Some(pipeline) = running_pipelines.get(&job.hash) {
                        job.pipeline = pipeline.clone();
                    }
                }
                // Finish this phase across every batch before starting any fallback.
                jobs.retain_mut(|job| scope.contains(&job.hash) && job.pipeline.next_step_in_phase(&job.book, phase).is_some());
                if !jobs.is_empty() {
                    let mut isbn_jobs = Vec::new();
                    let mut page_jobs = Vec::new();
                    let mut title_hashes = Vec::new();
                    let mut finished = HashSet::new();
                    let mut exhausted = HashSet::new();
                    let mut failed = HashSet::new();
                    let mut stage_providers = HashMap::new();
                    for job in &mut jobs {
                        match job.pipeline.next_step(&job.book) {
                            SubjectStep::Complete => {
                                finished.insert(job.hash);
                            }
                            SubjectStep::Exhausted => {
                                finished.insert(job.hash);
                                exhausted.insert(job.hash);
                            }
                            SubjectStep::ResolveCodes(_) => {
                                // Book writes already project LCC/BISAC through
                                // the same taxonomy. The shared policy checks whether
                                // the LCC actually resolves before treating it as done.
                                job.pipeline.finish_step();
                            }
                            SubjectStep::LookupIsbns(isbns) => {
                                let exact_isbns = job.book.identifiers.iter().filter(|id| id.scheme() == &book_model::Scheme::Isbn).filter_map(|id| book_model::from_metadata_value(id.value())).collect();
                                let reference_isbns = book_enrichment::related_isbn::lookup_candidates(&job.book.identifiers).into_iter().collect();
                                isbn_jobs.push(library_database::RichEnrichmentCandidate { content_hash: job.hash, isbns, exact_isbns, reference_isbns, classification_count: job.book.subjects.len() });
                                stage_providers.insert(job.hash, OPEN_LIBRARY_RICH_PROVIDER);
                            }
                            SubjectStep::SearchTitleAuthor => {
                                if job.title.trim().is_empty() {
                                    job.pipeline.finish_step();
                                    continue;
                                }
                                title_hashes.push(job.hash);
                                stage_providers.insert(job.hash, OPEN_LIBRARY_EDITION_IDENTITY_PROVIDER);
                            }
                            SubjectStep::InspectPages => {
                                let supported = self.cpu.capabilities().supports(job.format);
                                if !supported {
                                    job.pipeline.finish_step();
                                    continue;
                                }
                                page_jobs.push((job.hash, job.title.clone(), job.format));
                            }
                        }
                    }
                    let mut pages = stream::iter(page_jobs.into_iter().map(|(hash, title, format)| async move {
                        let outcome = async {
                            let _lease = self.assets.lease_book(&hash).await.map_err(BackendError::operation)?;
                            let Some(reader) = self.assets.prepare_reader(crate::BlobKind::Book, &hash).await.map_err(BackendError::operation)? else { return Ok(None) };
                            let result = cpu_host::submit_book(&*self.cpu, cpu_host::SubjectPages { source_name: title, format }, cpu_host::as_cpu_reader(reader)).await;
                            result.map(Some).map_err(BackendError::operation)
                        }
                        .await;
                        (hash, outcome)
                    }))
                    .buffer_unordered(enrichment_parallelism().0);
                    while let Some((hash, outcome)) = pages.next().await {
                        match outcome {
                            Ok(Some(evidence)) => {
                                // Persist each finished inspection without waiting for a slower sibling.
                                updated += usize::from(self.db.merge_evidence_pages(library_database::MergeEvidenceOutcome { content_hash: hash, evidence }).map_err(BackendError::operation)?);
                                jobs.iter_mut().find(|job| job.hash == hash).unwrap().pipeline.finish_step();
                            }
                            Ok(None) => jobs.iter_mut().find(|job| job.hash == hash).unwrap().pipeline.finish_step(),
                            Err(error) => {
                                log::warn!("subject page inspection {hash}: {error}");
                                failed.insert(hash);
                            }
                        }
                    }
                    if !isbn_jobs.is_empty() {
                        let hashes = isbn_jobs.iter().map(|c| c.content_hash).collect::<Vec<_>>();
                        match self.apply_rich_metadata_candidates(isbn_jobs).await {
                            Ok(count) => updated += count,
                            Err(error) => {
                                log::warn!("ISBN stage will retry: {error}");
                                failed.extend(hashes);
                            }
                        }
                    }
                    if !title_hashes.is_empty() {
                        let inputs = self.db.isbn_miss_identity_inputs(title_hashes.clone()).map_err(BackendError::operation)?;
                        match self.resolve_open_library_edition_identities(inputs).await {
                            Ok(count) => updated += count,
                            Err(error) => {
                                log::warn!("title stage will retry: {error}");
                                failed.extend(title_hashes);
                            }
                        }
                    }
                    let hashes = jobs.iter().map(|j| j.hash).collect::<Vec<_>>();
                    let attempted = stage_providers.keys().copied().collect::<HashSet<_>>();
                    let refreshed = self.db.refresh_subject_enrichment(hashes, stage_providers.clone()).map_err(BackendError::operation)?;
                    let mut checkpoints: Vec<library_database::SubjectCheckpointOutcome> = Vec::new();
                    for refreshed in refreshed {
                        let hash = refreshed.content_hash;
                        let job = jobs.iter_mut().find(|j| j.hash == hash).unwrap();
                        job.book = refreshed.book;
                        // Network failures retain the pending step. An omitted
                        // response is recorded as failed by the existing appliers.
                        if refreshed.provider_succeeded && !failed.contains(&hash) {
                            job.pipeline.finish_step();
                        } else if attempted.contains(&hash) {
                            failed.insert(hash);
                        }
                        let complete = finished.contains(&hash);
                        let delay = if failed.contains(&hash) {
                            60
                        } else if exhausted.contains(&hash) {
                            2_592_000
                        } else {
                            0
                        };
                        let expected = job.book.clone();
                        let title = job.title.clone();
                        checkpoints.push(library_database::SubjectCheckpointOutcome {
                            content_hash: hash,
                            phase: job.pipeline.phase(),
                            empty_metadata: expected == book_model::BookMetadata::default(),
                            expected: sync_common::wire::encode(&expected).map_err(BackendError::operation)?,
                            title,
                            complete,
                            retry_seconds: delay,
                        });
                        if complete {
                            running_pipelines.remove(&hash);
                        } else {
                            running_pipelines.insert(hash, job.pipeline.clone());
                        }
                    }
                    for checkpoint in checkpoints {
                        self.db.save_subject_checkpoint(checkpoint).map_err(BackendError::operation)?;
                    }
                    crate::executor::sleep(std::time::Duration::from_millis(1)).await;
                }
            }
        }
        Ok(updated)
    }
}
