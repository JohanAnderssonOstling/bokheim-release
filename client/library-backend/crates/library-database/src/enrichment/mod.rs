//! Enrichment work queues, results, and thumbnail jobs.

use crate::shared_sql::SharedSql;
// ---- enrichment ----
// Typed thumbnail-work queue operations.

use crate::{Database, DatabaseError};
use include_sqlite_sql::include_sql;
use sync_common::ContentHash;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbnailSyncAction { Put, Delete }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingThumbnailSync {
    pub content_hash: ContentHash,
    pub action: ThumbnailSyncAction,
    pub generation: i64,
}

impl Database {
    pub fn pending_thumbnail_job_count(&self) -> Result<u64, DatabaseError> {
        let connection = &self.connection;
        Ok(connection.thumbnail_pending_job_count(|row| row.get::<_, i64>(0))?.max(0) as u64)
    }

    pub fn pending_thumbnail_jobs_page(&self, after: &str) -> Result<Vec<(ContentHash, i64)>, DatabaseError> {
        let connection = &self.connection;
        let mut jobs = Vec::new();
        connection.thumbnail_pending_jobs(after, |row| {
            jobs.push((ContentHash::new(&row.get::<_, String>(0)?), row.get(1)?));
            Ok(())
        })?;
        Ok(jobs)
    }

    pub fn thumbnail_book_format(&self, hash: ContentHash) -> Result<Option<book_model::BookFormat>, DatabaseError> {
        let connection = &self.connection;
        let mut format = None;
        connection.thumbnail_local_format(hash.as_str(), |row| {
            format = Some(row.get::<_, String>(0)?);
            Ok(())
        })?;
        format.map(|format| book_model::BookFormat::from_extension(&format).ok_or_else(|| DatabaseError::message("book format is missing"))).transpose()
    }

    pub fn confirm_thumbnail_available(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        let connection = &self.connection;
        connection.thumbnail_set_result(hash.as_str(), "ready")?;
        Ok(())
    }

    pub fn has_remote_thumbnail(&self, hash: &ContentHash) -> Result<bool, DatabaseError> {
        let connection = &self.connection;
        connection.thumbnail_has_remote(hash.as_str(), |row| row.get(0)).map_err(Into::into)
    }

    pub fn thumbnail_download_coverage(&self) -> Result<(u64, u64), DatabaseError> {
        let mut coverage = (0, 0);
        self.connection.thumbnail_download_coverage(|row| {
            coverage = (row.get::<_, i64>(0)?.max(0) as u64, row.get::<_, i64>(1)?.max(0) as u64);
            Ok(())
        })?;
        Ok(coverage)
    }

    pub fn defer_thumbnail_to_download(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        let connection = &self.connection;
        let transaction = self.connection.unchecked_transaction()?;
        transaction.thumbnail_discard(hash.as_str())?;
        transaction.thumbnail_enqueue_asset_work(hash.as_str())?;
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    pub fn discard_thumbnail_work(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        let connection = &self.connection;
        connection.thumbnail_discard(hash.as_str())?;
        Ok(())
    }

    pub fn has_book_placement(&self, hash: ContentHash) -> Result<bool, DatabaseError> {
        Ok(self.thumbnail_book_format(hash)?.is_some())
    }

    pub fn complete_thumbnail_work(&self, hash: &ContentHash, has_cover: bool) -> Result<(), DatabaseError> {
        let connection = &self.connection;
        let transaction = self.connection.unchecked_transaction()?;
        transaction.thumbnail_confirm_source(hash.as_str(), "local")?;
        transaction.thumbnail_sidecar_clear(hash.as_str())?;
        transaction.thumbnail_set_result(hash.as_str(), if has_cover { "ready" } else { "no_cover" })?;
        if has_cover { transaction.thumbnail_sync_pending_set(hash.as_str(), "put")?; }
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    pub fn has_sidecar_thumbnail(&self, hash: &ContentHash) -> Result<bool, DatabaseError> {
        let mut exists = false;
        self.connection.thumbnail_sidecar_exists(hash.as_str(), |row| { exists = row.get(0)?; Ok(()) })?;
        Ok(exists)
    }

    pub fn complete_sidecar_thumbnail_work(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.thumbnail_confirm_source(hash.as_str(), "local")?;
        transaction.thumbnail_sidecar_mark(hash.as_str())?;
        transaction.thumbnail_set_result(hash.as_str(), "ready")?;
        transaction.thumbnail_sync_pending_set(hash.as_str(), "put")?;
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    pub fn complete_replaced_sidecar_thumbnail_work(&self, hash: &ContentHash, has_cover: bool) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.thumbnail_confirm_source(hash.as_str(), "local")?;
        transaction.thumbnail_sidecar_clear(hash.as_str())?;
        transaction.thumbnail_set_result(hash.as_str(), if has_cover { "ready" } else { "no_cover" })?;
        transaction.thumbnail_sync_pending_set(hash.as_str(), if has_cover { "put" } else { "delete" })?;
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    pub fn pending_thumbnail_sync_page(&self, after: &str) -> Result<Vec<PendingThumbnailSync>, DatabaseError> {
        let mut pending = Vec::new();
        self.connection.thumbnail_sync_pending_page(after, |row| {
            let action: String = row.get(1)?;
            let action = match action.as_str() {
                "put" => ThumbnailSyncAction::Put,
                "delete" => ThumbnailSyncAction::Delete,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            pending.push(PendingThumbnailSync { content_hash: ContentHash::new(&row.get::<_, String>(0)?), action, generation: row.get(2)? });
            Ok(())
        })?;
        Ok(pending)
    }

    pub fn complete_thumbnail_sync(&self, pending: PendingThumbnailSync) -> Result<(), DatabaseError> {
        self.connection.thumbnail_sync_pending_clear(pending.content_hash.as_str(), pending.generation)?;
        Ok(())
    }

    pub fn remote_thumbnail_revision(&self, hash: &ContentHash) -> Result<Option<ContentHash>, DatabaseError> {
        let mut revision = None;
        self.connection.thumbnail_remote_revision(hash.as_str(), |row| { revision = Some(ContentHash::new(&row.get::<_, String>(0)?)); Ok(()) })?;
        Ok(revision)
    }

    pub fn record_remote_thumbnail_revision(&self, hash: &ContentHash, revision: &ContentHash) -> Result<bool, DatabaseError> {
        let changed = self.remote_thumbnail_revision(hash)? != Some(*revision);
        if changed { self.connection.thumbnail_remote_revision_set(hash.as_str(), revision.as_str())?; }
        Ok(changed)
    }

    pub fn forget_remote_thumbnail_revision(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        self.connection.thumbnail_remote_revision_clear(hash.as_str())?;
        Ok(())
    }

    pub fn thumbnail_source_origin(&self, hash: &ContentHash) -> Result<Option<String>, DatabaseError> {
        let mut origin = None;
        self.connection.thumbnail_source_origin(hash.as_str(), |row| { origin = Some(row.get(0)?); Ok(()) })?;
        Ok(origin)
    }

    pub fn remote_thumbnail_presence_candidates_page(&self, after: &str) -> Result<Vec<ContentHash>, DatabaseError> {
        let mut hashes = Vec::new();
        self.connection.thumbnail_remote_presence_candidates(after, |row| {
            hashes.push(ContentHash::new(&row.get::<_, String>(0)?));
            Ok(())
        })?;
        Ok(hashes)
    }

    pub fn retry_thumbnail_work(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        let connection = &self.connection;
        let retry_after = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_err(DatabaseError::operation)?.as_secs().saturating_add(30) as i64;
        connection.thumbnail_retry(hash.as_str(), retry_after)?;
        Ok(())
    }

    pub fn thumbnail_upload_allowed(&self, hash: &ContentHash) -> Result<bool, DatabaseError> {
        let connection = &self.connection;
        let mut allowed = false;
        connection.thumbnail_upload_allowed(hash.as_str(), |row| {
            allowed = row.get(0)?;
            Ok(())
        })?;
        Ok(allowed)
    }

    pub fn record_uploaded_thumbnail(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        let connection = &self.connection;
        connection.thumbnail_record_remote(hash.as_str())?;
        Ok(())
    }

    pub fn complete_downloaded_thumbnail(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        Self::complete_downloaded_thumbnail_on(&transaction, hash)?;
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    pub(crate) fn complete_downloaded_thumbnail_on(connection: &rusqlite::Connection, hash: &ContentHash) -> Result<(), DatabaseError> {
        connection.thumbnail_record_remote(hash.as_str())?;
        connection.thumbnail_confirm_source(hash.as_str(), "remote")?;
        connection.thumbnail_set_result(hash.as_str(), "ready")?;
        Ok(())
    }

    pub fn forget_remote_thumbnail(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        let connection = &self.connection;
        connection.thumbnail_forget_remote(hash.as_str())?;
        Ok(())
    }

    pub fn request_missing_thumbnail(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        let connection = &self.connection;
        connection.thumbnail_request_missing(hash.as_str())?;
        Ok(())
    }

    /// Queues thumbnail recovery unconditionally, resetting any backoff.
    /// Unlike [`Database::request_missing_thumbnail`], which only revives
    /// finished work, this also covers books that never had a work row.
    pub fn shared_request_thumbnail(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        self.connection.shared_request_thumbnail(hash.as_str())?;
        Ok(())
    }

    /// Current thumbnail work state and retry deadline, if any row exists.
    pub fn thumbnail_work_state(&self, hash: &ContentHash) -> Result<Option<(String, i64)>, DatabaseError> {
        let mut state = None;
        self.connection.thumbnail_work_state(hash.as_str(), |row| {
            state = Some((row.get::<_, String>(0)?, row.get::<_, i64>(1)?));
            Ok(())
        })?;
        Ok(state)
    }

    /// Records remote thumbnail availability and returns missing hashes still
    /// eligible for local generation/upload.
    pub fn settle_thumbnail_availability(&self, available: &[ContentHash], missing: &[ContentHash]) -> Result<Vec<ContentHash>, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let uploads = Self::settle_thumbnail_availability_on(&transaction, available, missing)?;
        crate::sync::apply::commit(transaction)?;
        Ok(uploads)
    }

    pub(crate) fn settle_thumbnail_availability_on(connection: &rusqlite::Connection, available: &[ContentHash], missing: &[ContentHash]) -> Result<Vec<ContentHash>, DatabaseError> {
        for hash in available {
            connection.thumbnail_record_remote(hash.as_str())?;
        }
        let mut uploads = Vec::new();
        for hash in missing {
            use rusqlite::OptionalExtension as _;
            let mut allowed = false;
            connection
                .thumbnail_upload_allowed(hash.as_str(), |row| {
                    allowed = row.get(0)?;
                    Ok(())
                })
                .optional()?;
            if allowed {
                uploads.push(*hash);
            }
        }
        Ok(uploads)
    }
}

include_sql!("src/enrichment/sql/schema.sql");
include_sql!("src/enrichment/sql/enrichment.sql");

// ---- work ----
// Upload preparation snapshots and Audible batch claims.

#[derive(Clone, Debug)]
pub struct UploadPreparationBatch {
    pub members: Vec<(ContentHash, Option<i64>)>,
}

impl UploadPreparationBatch {
    pub fn hashes(&self) -> std::collections::HashSet<ContentHash> {
        self.members.iter().map(|(hash, _)| *hash).collect()
    }
}

#[derive(Debug, serde::Serialize)]
pub struct AudiobookEnrichmentStatus {
    pub title: Option<String>,
    pub status: String,
    pub response_json: Option<String>,
    pub retry_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct AudibleBatchJob {
    pub content_hash: ContentHash,
    pub request: metadata_contract::audible::LookupRequest,
    pub token: String,
}

impl Database {
    pub fn upload_preparation_snapshot(&self) -> Result<UploadPreparationBatch, DatabaseError> {
        let mut members = Vec::new();
        self.connection.upload_preparation_snapshot(|row| {
            members.push((ContentHash::new(&row.get::<_, String>(0)?), row.get::<_, Option<i64>>(1)?));
            Ok(())
        })?;
        Ok(UploadPreparationBatch { members })
    }

    pub fn prepared_upload_batch(&self, scope: &std::collections::HashSet<ContentHash>) -> Result<UploadPreparationBatch, DatabaseError> {
        let scope_json = serde_json::to_string(scope).map_err(DatabaseError::operation)?;
        let mut members = Vec::new();
        self.connection.prepared_upload_batch(scope_json.as_str(), |row| {
            members.push((ContentHash::new(&row.get::<_, String>(0)?), row.get::<_, Option<i64>>(1)?));
            Ok(())
        })?;
        Ok(UploadPreparationBatch { members })
    }

    pub fn finish_upload_preparation(&self, batch: &UploadPreparationBatch) -> Result<(), DatabaseError> {
        let members: Vec<(String, Option<i64>)> = batch.members.iter().map(|(hash, id)| (hash.to_string(), *id)).collect();
        let json = serde_json::to_string(&members).map_err(DatabaseError::operation)?;
        self.connection.finish_upload_preparation(json.as_str())?;
        Ok(())
    }

    pub fn upload_thumbnail_candidates(&self, members: &std::collections::HashSet<ContentHash>, now: i64) -> Result<Vec<ContentHash>, DatabaseError> {
        let scope_json = serde_json::to_string(members).map_err(DatabaseError::operation)?;
        let mut hashes = Vec::new();
        self.connection.upload_thumbnail_candidates(scope_json.as_str(), now, |row| {
            hashes.push(ContentHash::new(&row.get::<_, String>(0)?));
            Ok(())
        })?;
        Ok(hashes)
    }

    /// Includes future deadlines so the library actor keeps a retry wake scheduled.
    pub fn pending_audible_job_count(&self) -> Result<u64, DatabaseError> {
        let mut count = 0_i64;
        self.connection.audible_retry_job_count(metadata_contract::audible::POLICY_VERSION, |row| {
            count = row.get(0)?;
            Ok(())
        })?;
        Ok(count.max(0) as u64)
    }

    pub fn audiobook_enrichment_status(&self) -> Result<Vec<AudiobookEnrichmentStatus>, DatabaseError> {
        let mut rows = Vec::new();
        self.connection.audiobook_enrichment_status(|row| {
            rows.push(AudiobookEnrichmentStatus { title: row.get(0)?, status: row.get(1)?, response_json: row.get(2)?, retry_at: row.get(3)?, updated_at: row.get(4)? });
            Ok(())
        })?;
        Ok(rows)
    }

    pub fn audible_batch_candidates(&self, now: i64, members: &[ContentHash]) -> Result<Vec<ContentHash>, DatabaseError> {
        let json = serde_json::to_string(members).map_err(DatabaseError::operation)?;
        let mut hashes = Vec::new();
        self.connection.audible_batch_candidates(json.as_str(), now, metadata_contract::audible::POLICY_VERSION, |row| {
            hashes.push(ContentHash::new(&row.get::<_, String>(0)?));
            Ok(())
        })?;
        Ok(hashes)
    }

    pub fn claim_audible_batch(&self, now: i64, hash: ContentHash) -> Result<Option<AudibleBatchJob>, DatabaseError> {
        self.claim_audiobook(now, hash, false)
    }

    /// Explicit refresh claims each recording atomically without releasing a
    /// globally pending batch for another running client to race over.
    pub fn claim_audiobook_refresh(&self, now: i64, hash: ContentHash) -> Result<Option<AudibleBatchJob>, DatabaseError> {
        self.claim_audiobook(now, hash, true)
    }

    fn claim_audiobook(&self, now: i64, hash: ContentHash, refresh: bool) -> Result<Option<AudibleBatchJob>, DatabaseError> {
        use crate::transactions::WriteOutcome;
        self.with_write_transaction(|transaction| {
            if refresh {
                transaction.refresh_audiobook_enrichment(hash.as_str(), now, metadata_contract::audible::POLICY_VERSION)?;
            }
            let mut request_json = None;
            transaction.audible_claim_candidate(hash.as_str(), now, metadata_contract::audible::POLICY_VERSION, |row| {
                request_json = Some(row.get::<_, String>(0)?);
                Ok(())
            })?;
            let Some(request_json) = request_json else { return Ok(WriteOutcome::Rollback(None)) };
            let request: metadata_contract::audible::LookupRequest = serde_json::from_str(&request_json).map_err(DatabaseError::operation)?;
            let token = uuid::Uuid::new_v4().to_string();
            transaction.audible_claim(hash.as_str(), token.as_str(), now + 300, now)?;
            Ok(WriteOutcome::Commit(Some(AudibleBatchJob { content_hash: hash, request, token })))
        })
    }

    pub fn resolve_audible_batch(&self, job: &AudibleBatchJob, response: &metadata_contract::audible::LookupResponse, retry_at: i64, now: i64) -> Result<bool, DatabaseError> {
        use metadata_contract::audible::LookupStatus;
        let status = match response.status {
            LookupStatus::Supported => "applied",
            LookupStatus::NoMatch => "no_match",
            LookupStatus::Ambiguous => "ambiguous",
            LookupStatus::Unavailable => "unavailable",
        };
        let response_json = serde_json::to_string(response).map_err(DatabaseError::operation)?;
        let transaction = self.connection.unchecked_transaction()?;
        let committed = transaction.audible_resolve(job.content_hash.as_str(), job.token.as_str(), status, response_json.as_str(), retry_at, now)?;
        if committed == 0 {
            return Ok(false);
        }
        if status == "applied" {
            let candidate = response
                .candidates
                .iter()
                .find(|candidate| Some(&candidate.asin) == response.selected_asin.as_ref() && Some(&candidate.region) == response.selected_region.as_ref())
                .ok_or_else(|| DatabaseError::message("supported audiobook response has no selected recording"))?;
            let mut raw = None;
            transaction.enrichment_book(job.content_hash.as_str(), |row| {
                raw = Some(row.get::<_, Vec<u8>>(0)?);
                Ok(())
            })?;
            if let Some(raw) = raw {
                let mut book = crate::sync::decode_book_metadata(&raw)?;
                let mut changed = false;
                for (scheme, value) in std::iter::once((book_model::Scheme::Asin, &candidate.asin)).chain(candidate.isbns.iter().map(|isbn| (book_model::Scheme::Isbn, isbn))) {
                    // Preserve explicit local edition identifiers rather than replacing them.
                    if book.identifiers.iter().any(|identifier| identifier.scheme() == &scheme) {
                        continue;
                    }
                    if let Ok(identifier) = book_model::Identifier::new(value.clone(), scheme, book_model::Scope::Edition) {
                        book.identifiers.push(identifier);
                        changed = true;
                    }
                }
                if book.publishers.is_empty() {
                    if let Some(publisher) = candidate.publisher.as_deref().filter(|name| !name.trim().is_empty()) {
                        if let Ok(publisher) = book_model::PublisherCredit::new(publisher) {
                            book.publishers.push(publisher);
                            changed = true;
                        }
                    }
                }
                if changed {
                    let encoded = sync_common::wire::encode(&book).map_err(DatabaseError::operation)?;
                    transaction.audible_update_book_metadata(job.content_hash.as_str(), &encoded)?;
                    crate::sync::apply::project_prepared_book(&transaction, job.content_hash.as_str(), &book, encoded)?;
                }
                if let Some(description) = candidate.description.as_deref() {
                    let normalized = crate::sync::normalized_description(description);
                    transaction.enrichment_set_missing_description(job.content_hash.as_str(), &normalized)?;
                }
            }
            if let Some(plan) = response.chapter_plan.as_ref() {
                let entries = plan.chapters.iter().map(|chapter| book_model::BookTocEntry { title: chapter.title.clone(), target: book_model::audiobook_toc_target(chapter.start_ms), children: Vec::new() }).collect::<Vec<_>>();
                let toc_json = serde_json::to_string(&entries).map_err(DatabaseError::operation)?;
                transaction.audible_store_toc(job.content_hash.as_str(), entries.len() as i64, &toc_json)?;
            }
        }
        crate::sync::apply::commit(transaction)?;
        Ok(true)
    }
}

// ---- results ----
// Enrichment candidates, result application, and status reads.

#[derive(Clone, Debug)]
pub struct AuthorCredit {
    pub position: i64,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct AuthorEnrichmentCandidate {
    pub content_hash: ContentHash,
    pub isbns: Vec<String>,
    pub authors: Vec<AuthorCredit>,
}

#[derive(Clone, Debug)]
pub struct EditionIdentityCandidate {
    pub filenames: Vec<String>,
    pub content_hash: ContentHash,
    pub title: String,
    pub authors: Vec<String>,
    pub publishers: Vec<String>,
    pub book_year: Option<i32>,
    pub resource_isbns: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct DescriptionEnrichmentResult {
    pub content_hash: ContentHash,
    pub provider: String,
    pub identifier: String,
    pub description: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug)]
pub struct EnrichmentFailure {
    pub content_hash: ContentHash,
    pub provider: String,
    pub identifier: String,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct EnrichmentAttempt {
    pub content_hash: ContentHash,
    pub provider: String,
    pub identifier: String,
    pub status: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AuthorResolution {
    pub position: usize,
    pub preferred_name: String,
    pub match_key: String,
    pub external_id: book_model::ExternalAuthorId,
}

#[derive(Clone, Debug)]
pub struct RichEnrichmentCandidate {
    pub content_hash: ContentHash,
    pub isbns: Vec<String>,
    /// ISBNs which identify the local book itself. ISBNs attached from
    /// every ambiguous Open Library edition remain useful for locating work
    /// records, but their edition metadata must not be treated as exact
    /// evidence for the local file.
    pub exact_isbns: std::collections::BTreeSet<String>,
    pub reference_isbns: std::collections::BTreeSet<String>,
    pub classification_count: usize,
}

#[derive(Clone, Debug)]
pub struct IsbnCacheOutcome {
    pub isbn: String,
    pub json: String,
}

#[derive(Clone, Debug)]
pub struct MergeEvidenceOutcome {
    pub content_hash: ContentHash,
    pub evidence: book_model::BookMetadata,
}

#[derive(Clone, Debug)]
pub struct SubjectCheckpointOutcome {
    pub content_hash: ContentHash,
    pub phase: book_enrichment::SubjectPhase,
    pub expected: Vec<u8>,
    pub title: String,
    pub complete: bool,
    pub retry_seconds: i64,
    pub empty_metadata: bool,
}

#[derive(Clone, Debug)]
pub struct AuthoritativeAuthorsOutcome {
    pub content_hash: ContentHash,
    pub provider: String,
    pub isbn: String,
    pub authors: Vec<AuthorResolution>,
}

#[derive(Clone, Debug)]
pub struct AuthorEnrichmentOutcome {
    pub content_hash: ContentHash,
    pub provider: String,
    pub isbn: String,
    pub resolutions: Vec<AuthorResolution>,
    pub status: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RichEnrichmentOutcome {
    pub content_hash: ContentHash,
    pub provider: String,
    pub isbn: String,
    pub matched: Option<metadata_contract::RichWorkMetadataMatch>,
    pub subjects: Vec<book_model::BookSubject>,
    pub status: String,
    pub detail: Option<String>,
    pub replace_previous_work_classifications: bool,
}

#[derive(Clone, Debug)]
pub struct EditionIdentityOutcome {
    pub content_hash: ContentHash,
    pub provider: String,
    pub identifier: String,
    pub isbn: Option<String>,
    pub status: String,
    pub detail: Option<String>,
}

pub struct SubjectEnrichmentJob {
    pub hash: ContentHash,
    pub title: String,
    pub format: book_model::BookFormat,
    pub book: book_model::BookMetadata,
    pub pipeline: book_enrichment::SubjectPipeline,
}

#[derive(Clone, Debug)]
pub struct RefreshedSubject {
    pub content_hash: ContentHash,
    pub book: book_model::BookMetadata,
    pub provider_succeeded: bool,
}

fn split_isbns(value: &str) -> Vec<String> {
    value.split(',').map(str::trim).filter(|part| !part.is_empty()).map(str::to_owned).collect()
}

impl Database {
    pub fn description_enrichment_candidates(&self, provider: &str, cursor: &str, scope: &str) -> Result<Vec<(ContentHash, Vec<String>)>, DatabaseError> {
        let mut candidates = Vec::new();
        self.connection.enrichment_description_candidates(provider, cursor, scope, |row| {
            candidates.push((ContentHash::new(&row.get::<_, String>(0)?), split_isbns(&row.get::<_, String>(1)?)));
            Ok(())
        })?;
        Ok(candidates)
    }

    pub fn author_enrichment_candidates(&self, provider: &str, cursor: &str, scope: &str) -> Result<Vec<AuthorEnrichmentCandidate>, DatabaseError> {
        let mut candidates = Vec::new();
        self.connection.enrichment_author_candidates(provider, cursor, scope, |row| {
            let content_hash = ContentHash::new(&row.get::<_, String>(0)?);
            let isbns = split_isbns(&row.get::<_, String>(1)?);
            let mut authors = Vec::new();
            self.connection.enrichment_author_credits(content_hash.as_str(), |credit| {
                authors.push(AuthorCredit { position: credit.get(0)?, name: credit.get(1)? });
                Ok(())
            })?;
            candidates.push(AuthorEnrichmentCandidate { content_hash, isbns, authors });
            Ok(())
        })?;
        Ok(candidates)
    }

    pub fn audible_cover_identity(&self, hash: &ContentHash) -> Result<Option<(String, String)>, DatabaseError> {
        let mut identity = None;
        self.connection.enrichment_audible_cover_identity(hash.as_str(), |row| {
            identity = Some((row.get(0)?, row.get(1)?));
            Ok(())
        })?;
        Ok(identity)
    }

    pub fn audible_cover_candidates(&self, provider: &str, cursor: &str, scope: &str) -> Result<Vec<(ContentHash, String, String)>, DatabaseError> {
        let mut candidates = Vec::new();
        self.connection.enrichment_audible_cover_candidates(provider, cursor, scope, |row| {
            candidates.push((ContentHash::new(&row.get::<_, String>(0)?), row.get(1)?, row.get(2)?));
            Ok(())
        })?;
        Ok(candidates)
    }

    pub fn cover_enrichment_candidates(&self, provider: &str, cursor: &str, scope: &str) -> Result<Vec<(ContentHash, Vec<String>)>, DatabaseError> {
        let mut candidates = Vec::new();
        self.connection.enrichment_cover_candidates(provider, cursor, scope, |row| {
            candidates.push((ContentHash::new(&row.get::<_, String>(0)?), split_isbns(&row.get::<_, String>(1)?)));
            Ok(())
        })?;
        Ok(candidates)
    }

    pub fn isbn_miss_identity_inputs(&self, title_hashes: Vec<ContentHash>) -> Result<Vec<EditionIdentityCandidate>, DatabaseError> {
        let mut candidates = Vec::new();
        for hash in title_hashes {
            let mut input = None;
            self.connection.enrichment_identity_input(hash.as_str(), |row| {
                input = Some((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?));
                Ok(())
            })?;
            let Some((title, metadata)) = input else { continue };
            let metadata = crate::sync::decode_book_metadata(&metadata)?;
            let mut authors = Vec::new();
            self.connection.enrichment_identity_authors(hash.as_str(), |row| {
                authors.push(row.get::<_, String>(0)?);
                Ok(())
            })?;
            let publishers = metadata.publishers.iter().map(|credit| credit.name().as_str().to_owned()).collect();
            let book_year = metadata.dates.iter().filter_map(|date| date.value().get(..4).and_then(|year| year.parse::<i32>().ok())).next();
            let resource_isbns = metadata.identifiers.iter().filter(|identifier| identifier.scheme() == &book_model::Scheme::Isbn).filter_map(|identifier| identifier.canonical_value()).collect();
            let mut filenames = Vec::new();
            self.connection.enrichment_identity_filenames(hash.as_str(), |row| {
                filenames.push(row.get::<_, String>(0)?);
                Ok(())
            })?;
            candidates.push(EditionIdentityCandidate { filenames, content_hash: hash, title, authors, publishers, book_year, resource_isbns });
        }
        Ok(candidates)
    }

    pub fn subject_enrichment_candidates(&self, cursor: &str, scope: &str) -> Result<Vec<SubjectEnrichmentJob>, DatabaseError> {
        let mut jobs = Vec::new();
        self.connection.enrichment_subject_candidates(cursor, scope, book_enrichment::SUBJECT_PIPELINE_REVISION, |row| {
            let hash = ContentHash::new(&row.get::<_, String>(0)?);
            let title: String = row.get(1)?;
            let Some(format) = row.get::<_, String>(2).ok().and_then(|format| book_model::BookFormat::from_extension(&format)) else {
                return Ok(());
            };
            let Ok(book) = crate::sync::decode_book_metadata(&row.get::<_, Vec<u8>>(3)?) else {
                return Ok(());
            };
            let pipeline = row
                .get::<_, Option<i64>>(4)
                .ok()
                .flatten()
                .and_then(|phase| u8::try_from(phase).ok())
                .and_then(book_enrichment::SubjectPhase::from_checkpoint_id)
                .map(book_enrichment::SubjectPipeline::resume_at_phase)
                .unwrap_or_default();
            jobs.push(SubjectEnrichmentJob { hash, title, format, book, pipeline });
            Ok(())
        })?;
        Ok(jobs)
    }

    pub fn refresh_subject_enrichment(&self, hashes: Vec<ContentHash>, providers: std::collections::HashMap<ContentHash, &str>) -> Result<Vec<RefreshedSubject>, DatabaseError> {
        let mut refreshed = Vec::new();
        for hash in hashes {
            let mut raw = None;
            self.connection.enrichment_book(hash.as_str(), |row| {
                raw = Some(row.get::<_, Vec<u8>>(0)?);
                Ok(())
            })?;
            let Some(raw) = raw else { continue };
            let book = crate::sync::decode_book_metadata(&raw)?;
            let provider_succeeded = match providers.get(&hash) {
                Some(provider) => {
                    let mut succeeded = false;
                    self.connection.enrichment_step_succeeded(hash.as_str(), provider, |row| {
                        succeeded = row.get(0)?;
                        Ok(())
                    })?;
                    succeeded
                }
                None => false,
            };
            refreshed.push(RefreshedSubject { content_hash: hash, book, provider_succeeded });
        }
        Ok(refreshed)
    }

    pub fn cached_isbn_enrichment(&self, isbns: Vec<String>) -> Result<Vec<(String, Option<String>)>, DatabaseError> {
        let mut cached = Vec::new();
        for isbn in isbns {
            let mut response = None;
            self.connection.enrichment_cached_isbn(isbn.as_str(), |row| {
                response = Some(row.get::<_, String>(0)?);
                Ok(())
            })?;
            cached.push((isbn, response));
        }
        Ok(cached)
    }

    pub fn apply_description_enrichment(&self, result: DescriptionEnrichmentResult) -> Result<usize, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let updated = transaction.enrichment_set_missing_description(result.content_hash.as_str(), result.description.as_deref().unwrap_or(""))?;
        transaction.shared_enrichment_mark_description_scanned(result.content_hash.as_str())?;
        transaction.enrichment_record_attempt(result.content_hash.as_str(), result.provider.as_str(), result.identifier.as_str(), result.status.as_str(), result.description.as_deref())?;
        crate::sync::apply::commit(transaction)?;
        Ok(updated)
    }

    pub fn record_enrichment_failures(&self, failures: Vec<EnrichmentFailure>) -> Result<(), DatabaseError> {
        for failure in failures {
            self.connection.enrichment_record_attempt(failure.content_hash.as_str(), failure.provider.as_str(), failure.identifier.as_str(), "failed", Some(failure.detail.as_str()))?;
        }
        Ok(())
    }
    pub fn record_enrichment_attempts(&self, attempts: Vec<EnrichmentAttempt>) -> Result<(), DatabaseError> {
        for attempt in attempts {
            self.connection.enrichment_record_attempt(attempt.content_hash.as_str(), attempt.provider.as_str(), attempt.identifier.as_str(), attempt.status.as_str(), attempt.detail.as_deref())?;
        }
        Ok(())
    }
    /// Applies one edition-identity outcome: stores the ISBN when the book has
    /// none (matching derived identifiers, including subject-embedded ones),
    /// rewrites the identifier rows, and records the attempt atomically.
    pub fn apply_edition_identity(&self, outcome: EditionIdentityOutcome) -> Result<bool, DatabaseError> {
        let mut raw: Option<Vec<u8>> = None;
        self.connection.enrichment_book(outcome.content_hash.as_str(), |row| {
            raw = Some(row.get::<_, Vec<u8>>(0)?);
            Ok(())
        })?;
        let Some(raw) = raw else { return Ok(false) };
        let mut book = crate::sync::decode_book_metadata(&raw)?;
        book.infer_embedded_subject_isbns().map_err(|error| DatabaseError::operation(error.to_string()))?;
        let changed = outcome.isbn.is_some() && !book.identifiers.iter().any(|identifier| identifier.scheme() == &book_model::Scheme::Isbn && identifier.canonical_value().is_some());
        let transaction = self.connection.unchecked_transaction()?;
        if changed {
            let identifier = book_model::Identifier::new(outcome.isbn.clone().unwrap_or_default(), book_model::Scheme::Isbn, book_model::Scope::Edition).map_err(|error| DatabaseError::operation(error.to_string()))?;
            book.identifiers.push(identifier);
            let encoded = sync_common::wire::encode(&book).map_err(DatabaseError::operation)?;
            transaction.audible_update_book_metadata(outcome.content_hash.as_str(), &encoded)?;
            transaction.audible_clear_identifiers(outcome.content_hash.as_str())?;
            for (position, identifier) in book.identifiers.iter().enumerate() {
                transaction.audible_insert_identifier(
                    outcome.content_hash.as_str(),
                    position as i64,
                    &crate::sync::identifier_scheme_label(identifier.scheme()),
                    identifier.value(),
                    identifier.canonical_value().as_deref(),
                    crate::sync::identifier_scope_label(identifier.scope()),
                )?;
            }
        }
        transaction.enrichment_record_attempt(outcome.content_hash.as_str(), outcome.provider.as_str(), outcome.identifier.as_str(), outcome.status.as_str(), outcome.detail.as_deref())?;
        crate::sync::apply::commit(transaction)?;
        Ok(changed)
    }

    /// Applies one rich-enrichment outcome: description, subjects, and
    /// attempt record, atomically.
    pub fn apply_rich_enrichment(&self, outcome: RichEnrichmentOutcome) -> Result<usize, DatabaseError> {
        let mut raw: Option<Vec<u8>> = None;
        self.connection.enrichment_book(outcome.content_hash.as_str(), |row| {
            raw = Some(row.get::<_, Vec<u8>>(0)?);
            Ok(())
        })?;
        let Some(raw) = raw else { return Ok(0) };
        let transaction = self.connection.unchecked_transaction()?;
        let mut imported = 0;
        if let Some(description) = outcome.matched.as_ref().and_then(|matched| matched.description.as_deref()) {
            let normalized = crate::sync::normalized_description(description);
            if !normalized.trim().is_empty() && transaction.enrichment_set_missing_description(outcome.content_hash.as_str(), &normalized)? > 0 {
                imported += 1;
            }
        }
        if !outcome.subjects.is_empty() || outcome.replace_previous_work_classifications {
            let previous = crate::sync::decode_book_metadata(&raw)?;
            let mut book = previous.clone();
            if outcome.replace_previous_work_classifications {
                let mut fallback_codes = std::collections::BTreeSet::new();
                // The source allow-list lives here rather than in SQL: the
                // statement parser mistakes ':subject' inside string literals
                // for bind parameters.
                transaction.rich_fallback_codes(outcome.content_hash.as_str(), |row| {
                    let source: String = row.get(2)?;
                    if matches!(source.as_str(), "dc:subject" | "openlibrary:metadata:sibling-edition" | "openlibrary:metadata:work") {
                        fallback_codes.insert((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
                    }
                    Ok(())
                })?;
                book.subjects.retain(|subject| {
                    let authority = subject.authority().unwrap_or_default().to_ascii_lowercase();
                    let notation = subject.code().unwrap_or_else(|| subject.name());
                    !fallback_codes.contains(&(authority, notation.to_owned()))
                });
                imported += previous.subjects.len() - book.subjects.len();
            }
            for subject in &outcome.subjects {
                if !book.subjects.contains(subject) {
                    book.subjects.push(subject.clone());
                    imported += 1;
                }
            }
            if book != previous {
                let encoded = sync_common::wire::encode(&book).map_err(DatabaseError::operation)?;
                crate::sync::apply::project_prepared_book(&transaction, outcome.content_hash.as_str(), &book, encoded)?;
            }
        }
        let bounded_detail = outcome.detail.map(|detail| detail.chars().take(1_024).collect::<String>());
        transaction.enrichment_record_attempt(outcome.content_hash.as_str(), outcome.provider.as_str(), outcome.isbn.as_str(), outcome.status.as_str(), bounded_detail.as_deref())?;
        crate::sync::apply::commit(transaction)?;
        Ok(imported)
    }
    /// Applies one author-enrichment outcome: resolves each credit against its
    /// authority identifier and records the attempt atomically.
    pub fn apply_author_enrichment(&self, outcome: AuthorEnrichmentOutcome) -> Result<usize, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let mut updated = 0_usize;
        for resolution in &outcome.resolutions {
            let position = i64::try_from(resolution.position).map_err(|error| DatabaseError::operation(error.to_string()))?;
            let mut current: Option<(i64, String, bool)> = None;
            transaction.author_resolve_credit(outcome.content_hash.as_str(), position, |row| {
                current = Some((row.get(0)?, row.get(1)?, row.get(2)?));
                Ok(())
            })?;
            let (current_local_id, _, current_is_provisional) = current.ok_or_else(|| DatabaseError::message("book credit is missing"))?;
            let mut existing: Option<(i64, String)> = None;
            transaction.author_find_external_identity(resolution.external_id.authority().as_str(), resolution.external_id.value(), |row| {
                existing = Some((row.get(0)?, row.get(1)?));
                Ok(())
            })?;
            let authority_stable_id = book_model::agent_id_from_migration_seed(format!("external-author:{}:{}", resolution.external_id.authority().as_str(), resolution.external_id.value()).as_bytes());
            let mut stable: Option<(i64, String)> = None;
            transaction.author_find_stable_identity(authority_stable_id.to_string(), |row| {
                stable = Some((row.get(0)?, row.get(1)?));
                Ok(())
            })?;
            let mut credit_count: i64 = 0;
            transaction.author_credit_count(current_local_id, |row| {
                credit_count = row.get(0)?;
                Ok(())
            })?;
            let (target_local_id, target_stable_id) = match existing {
                Some(identity) => identity,
                None if stable.is_some() => stable.expect("checked above"),
                None if current_is_provisional && credit_count == 1 => {
                    transaction.author_update_stable_id(authority_stable_id.to_string(), current_local_id)?;
                    (current_local_id, authority_stable_id.to_string())
                }
                None => {
                    transaction.author_insert_identity(authority_stable_id.to_string(), resolution.preferred_name.as_str(), resolution.match_key.as_str(), crate::sync::unix_millis()?)?;
                    (transaction.last_insert_rowid(), authority_stable_id.to_string())
                }
            };
            let target_author_id = book_model::AgentId::parse_str(&target_stable_id).map_err(|error| DatabaseError::operation(error.to_string()))?;
            transaction.author_update_identity_names(resolution.preferred_name.as_str(), resolution.match_key.as_str(), target_local_id)?;
            transaction.author_attach_external_id(target_local_id, resolution.external_id.authority().as_str(), resolution.external_id.value(), crate::sync::unix_millis()?)?;
            // Update even when the local row is unchanged. The contributor trigger
            // refreshes the already-queued metadata mutation with the authority-derived
            // stable id before the first upload.
            transaction.author_reidentify_credit(target_local_id, outcome.content_hash.as_str(), position)?;
            if target_local_id != current_local_id {
                transaction.author_delete_identity(current_local_id)?;
            }
            let _ = target_author_id;
            updated += 1;
        }
        let bounded_detail = outcome.detail.map(|detail| detail.chars().take(1_024).collect::<String>());
        transaction.enrichment_record_attempt(outcome.content_hash.as_str(), outcome.provider.as_str(), outcome.isbn.as_str(), outcome.status.as_str(), bounded_detail.as_deref())?;
        crate::sync::apply::commit(transaction)?;
        Ok(updated)
    }
    /// Applies one authoritative-authors outcome: replaces the author credits
    /// with authority-resolved identities and records the attempt atomically.
    pub fn apply_authoritative_authors(&self, outcome: AuthoritativeAuthorsOutcome) -> Result<usize, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let mut credits: Vec<book_model::Contributor> =
            outcome.authors.iter().map(|author| book_model::Contributor::new(&author.preferred_name, book_model::MarcRelatorCode(*b"aut")).map_err(|error| DatabaseError::operation(error.to_string()))).collect::<Result<Vec<_>, _>>()?;
        let mut existing = Vec::new();
        transaction.author_non_author_credits(outcome.content_hash.as_str(), |row| {
            existing.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Vec<u8>>(2)?));
            Ok(())
        })?;
        for (id, name, role) in existing {
            let id = book_model::AgentId::parse_str(&id).map_err(|error| DatabaseError::operation(error.to_string()))?;
            let role = book_model::MarcRelatorCode(role.try_into().map_err(|_| DatabaseError::message("invalid contributor role"))?);
            credits.push(book_model::Contributor::with_id(id, &name, role).map_err(|error| DatabaseError::operation(error.to_string()))?);
        }
        let prepared = crate::contributors::resolve_credits(&transaction, &credits)?;
        crate::transactions::with_metadata_batch(&transaction, &self.metadata_batch_flag, || crate::contributors::write_credits(&transaction, outcome.content_hash.as_str(), &prepared))?;
        transaction.enrichment_record_attempt(outcome.content_hash.as_str(), outcome.provider.as_str(), outcome.isbn.as_str(), "updated", Some("authoritative Open Library author credits from unambiguous ISBN match"))?;
        crate::sync::apply::commit(transaction)?;
        Ok(outcome.authors.len())
    }
    pub fn cache_isbn_json(&self, outcome: IsbnCacheOutcome) -> Result<(), DatabaseError> {
        let key = book_enrichment::isbn_key(&outcome.isbn).unwrap_or_else(|| outcome.isbn.clone());
        let transaction = self.connection.unchecked_transaction()?;
        transaction.cache_isbn_response(&key, &outcome.json)?;
        transaction.trim_isbn_response_cache()?;
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }
    /// Merges inspection evidence into the stored book, preserving concurrent
    /// edits, and converges the projections. Returns whether anything changed.
    pub fn merge_evidence_pages(&self, outcome: MergeEvidenceOutcome) -> Result<bool, DatabaseError> {
        let mut raw: Option<Vec<u8>> = None;
        self.connection.enrichment_book(outcome.content_hash.as_str(), |row| {
            raw = Some(row.get::<_, Vec<u8>>(0)?);
            Ok(())
        })?;
        let Some(raw) = raw else { return Ok(false) };
        let previous = crate::sync::decode_book_metadata(&raw)?;
        let mut current = previous.clone();
        for identifier in &outcome.evidence.identifiers {
            if !current.identifiers.contains(identifier) {
                current.identifiers.push(identifier.clone());
            }
        }
        for subject in &outcome.evidence.subjects {
            if !current.subjects.contains(subject) {
                current.subjects.push(subject.clone());
            }
        }
        if current == previous {
            return Ok(false);
        }
        let encoded = sync_common::wire::encode(&current).map_err(DatabaseError::operation)?;
        let transaction = self.connection.unchecked_transaction()?;
        crate::sync::apply::project_prepared_book(&transaction, outcome.content_hash.as_str(), &current, encoded)?;
        crate::sync::apply::commit(transaction)?;
        Ok(true)
    }
    /// Persists a coarse subject-pipeline phase checkpoint prepared by the producer.
    pub fn save_subject_checkpoint(&self, outcome: SubjectCheckpointOutcome) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.save_subject_pipeline_checkpoint(
            outcome.content_hash.as_str(),
            i64::from(outcome.phase.checkpoint_id()),
            book_enrichment::SUBJECT_PIPELINE_REVISION,
            outcome.complete,
            outcome.retry_seconds,
            &outcome.expected,
            &outcome.title,
            outcome.empty_metadata,
        )?;
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }
    pub fn author_rich_already_applied(&self, hash: &ContentHash, provider: &str, identifier: &str) -> Result<bool, DatabaseError> {
        let mut applied = false;
        self.connection.author_rich_already_applied(hash.as_str(), provider, identifier, |row| {
            applied = row.get(0)?;
            Ok(())
        })?;
        Ok(applied)
    }
    pub fn cover_enrichment_state(&self, hash: &ContentHash) -> Result<(Option<(String, i64)>, Option<book_model::BookFormat>), DatabaseError> {
        let mut state = None;
        self.connection.enrichment_thumbnail_state(hash.as_str(), |row| {
            state = Some((row.get::<_, String>(0)?, row.get::<_, i64>(1)?));
            Ok(())
        })?;
        let mut format = None;
        self.connection.enrichment_book_format(hash.as_str(), |row| {
            format = Some(row.get::<_, String>(0)?);
            Ok(())
        })?;
        Ok((state, format.and_then(|format| book_model::BookFormat::from_extension(&format))))
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    fn open_test_database() -> (Database, std::path::PathBuf) {
        // Process IDs can repeat across test runs while old fixture files remain.
        let path = std::env::temp_dir().join(format!("library-database-enrichment-schema-{}", uuid::Uuid::new_v4()));
        let database = Database::open(&path).expect("open test database");
        database.initialize_library().expect("initialize test library");
        (database, path)
    }

    #[test]
    fn sidecar_cover_provenance_is_cleared_when_local_cover_changes_source() {
        let (db, _path) = open_test_database();
        let hash = ContentHash::new(&"d".repeat(64));
        db.connection.execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'Audio','m4b')", [hash.as_str()]).unwrap();
        db.complete_sidecar_thumbnail_work(&hash).unwrap();
        assert!(db.has_sidecar_thumbnail(&hash).unwrap());
        let first = db.pending_thumbnail_sync_page("").unwrap().pop().unwrap();
        assert_eq!(first.action, ThumbnailSyncAction::Put);
        db.complete_replaced_sidecar_thumbnail_work(&hash, false).unwrap();
        assert!(!db.has_sidecar_thumbnail(&hash).unwrap());
        let second = db.pending_thumbnail_sync_page("").unwrap().pop().unwrap();
        assert_eq!(second.action, ThumbnailSyncAction::Delete);
        assert!(second.generation > first.generation);
        db.complete_thumbnail_sync(first).unwrap();
        assert_eq!(db.pending_thumbnail_sync_page("").unwrap(), vec![second]);
        db.complete_thumbnail_sync(second).unwrap();
        assert!(db.pending_thumbnail_sync_page("").unwrap().is_empty());
        db.complete_thumbnail_work(&hash, true).unwrap();
        let embedded = db.pending_thumbnail_sync_page("").unwrap().pop().unwrap();
        assert_eq!(embedded.action, ThumbnailSyncAction::Put);
        db.complete_thumbnail_sync(embedded).unwrap();
        let revision = ContentHash::new(&"e".repeat(64));
        assert!(db.record_remote_thumbnail_revision(&hash, &revision).unwrap());
        assert!(!db.record_remote_thumbnail_revision(&hash, &revision).unwrap());
        assert_eq!(db.remote_thumbnail_revision(&hash).unwrap(), Some(revision));
        db.forget_remote_thumbnail_revision(&hash).unwrap();
        assert!(db.remote_thumbnail_revision(&hash).unwrap().is_none());
    }


    #[test]
    fn remote_thumbnail_presence_does_not_strand_pending_recovery() {
        let (db, _path) = open_test_database();
        let hash = ContentHash::new(&"a".repeat(64));
        db.connection.execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'A book','epub')", [hash.as_str()]).unwrap();
        db.connection.execute("INSERT INTO local_thumbnail_work(content_hash,state) VALUES(?1,'pending')", [hash.as_str()]).unwrap();
        db.connection.execute("INSERT INTO remote_asset(kind,hash) VALUES('thumbnail',?1)", [hash.as_str()]).unwrap();

        assert_eq!(db.upload_thumbnail_candidates(&std::collections::HashSet::from([hash]), 0).unwrap(), [hash]);
    }

    #[test]
    fn thumbnail_download_coverage_survives_reopening_and_tracks_recovery() {
        let (db, path) = open_test_database();
        let first = ContentHash::new(&"a".repeat(64));
        let second = ContentHash::new(&"b".repeat(64));
        for hash in [first, second] {
            db.connection.execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'A book','epub')", [hash.as_str()]).unwrap();
            db.connection.execute("INSERT INTO remote_asset(kind,hash) VALUES('thumbnail',?1)", [hash.as_str()]).unwrap();
        }
        assert_eq!(db.thumbnail_download_coverage().unwrap(), (0, 2));
        db.complete_downloaded_thumbnail(&first).unwrap();
        assert_eq!(db.thumbnail_download_coverage().unwrap(), (1, 2));
        drop(db);

        let db = Database::open(&path).unwrap();
        assert_eq!(db.thumbnail_download_coverage().unwrap(), (1, 2));
        db.complete_downloaded_thumbnail(&second).unwrap();
        assert_eq!(db.thumbnail_download_coverage().unwrap(), (2, 2));
        db.request_missing_thumbnail(&first).unwrap();
        assert_eq!(db.thumbnail_download_coverage().unwrap(), (1, 2));
    }

    #[test]
    fn audiobook_failures_remain_retryable_after_upload_preparation_and_success_is_atomic() {
        use metadata_contract::audible::*;
        let (db, path) = open_test_database();
        let hash = ContentHash::new(&"a".repeat(64));
        db.connection.execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'A book','m4b')", [hash.as_str()]).unwrap();
        db.connection.execute("INSERT INTO audiobook_metadata VALUES(?1,600000)", [hash.as_str()]).unwrap();
        db.connection.execute("INSERT INTO book_toc VALUES(?1,0,'[]')",[hash.as_str()]).unwrap();
        let request = LookupRequest { title: "A book".into(), authors: vec![], duration_ms: 600000, recording: RecordingEvidence::default(), chapters: vec![] };
        db.connection
            .execute("INSERT INTO audible_enrichment_jobs(content_hash,policy,request_json,status,updated_at) VALUES(?1,?2,?3,'pending',0)", rusqlite::params![hash.as_str(), POLICY_VERSION, serde_json::to_string(&request).unwrap()])
            .unwrap();
        db.finish_upload_preparation(&db.upload_preparation_snapshot().unwrap()).unwrap();
        assert_eq!(db.connection.query_row("SELECT COUNT(*) FROM local_upload_preparation WHERE content_hash=?1", [hash.as_str()], |row| row.get::<_, i64>(0)).unwrap(), 1, "optional Audible lookup must not hold upload admission");
        let job = db.claim_audible_batch(100, hash).unwrap().unwrap();
        let mut response = LookupResponse { status: LookupStatus::Unavailable, candidates: vec![], selected_asin: None, selected_region: None, chapter_plan: None, used_website_fallback: false, detail: "HTTP 503".into() };
        assert!(db.resolve_audible_batch(&job, &response, 400, 100).unwrap());
        db.finish_upload_preparation(&db.upload_preparation_snapshot().unwrap()).unwrap();
        assert_eq!(db.pending_audible_job_count().unwrap(), 1);
        assert!(db.audible_batch_candidates(399, &[hash]).unwrap().is_empty());
        assert_eq!(db.audible_batch_candidates(400, &[hash]).unwrap(), [hash]);
        let retried = db.claim_audible_batch(400, hash).unwrap().unwrap();
        assert!(db.claim_audiobook_refresh(401, hash).unwrap().is_none(), "refresh must not steal an active claim");
        assert!(db.claim_audible_batch(401, hash).unwrap().is_none(), "refresh must not steal active claims");
        assert!(!db.resolve_audible_batch(&job, &response, 700, 400).unwrap(), "stale claim cannot overwrite retry");
        response.status = LookupStatus::Supported;
        response.selected_asin = Some("B000000001".into());
        response.selected_region = Some("us".into());
        response.candidates.push(Candidate {
            asin: "B000000001".into(),
            region: "us".into(),
            title: "A book".into(),
            subtitle: None,
            authors: vec![],
            narrators: vec![],
            publisher: Some("Test Publisher".into()),
            description: Some("An audiobook description.".into()),
            release_date: None,
            isbns: vec!["9780306406157".into()],
            abridged: None,
            provider_duration_ms: None,
            duration_ms: Some(600000),
            chapters: vec![],
            source: DiscoverySource::Api,
            metadata_provider: "audnexus".into(),
            chapter_provider: Some("audnexus".into()),
        });
        response.chapter_plan = Some(ChapterPlan { method: AlignmentMethod::ImportedBoundaries, chapters: vec![Chapter { title: "Opening".into(), start_ms: 0, end_ms: 600000 }], renamed: 1, detail: "aligned".into() });
        assert!(db.resolve_audible_batch(&retried, &response, 700, 400).unwrap());
        assert_eq!(db.pending_audible_job_count().unwrap(), 0);
        let identifiers: i64 = db.connection.query_row("SELECT COUNT(*) FROM book_identifier", [], |r| r.get(0)).unwrap();
        assert_eq!(identifiers, 2);
        let raw: Vec<u8> = db.connection.query_row("SELECT book_metadata FROM book", [], |r| r.get(0)).unwrap();
        assert_eq!(crate::sync::decode_book_metadata(&raw).unwrap().publishers[0].name().as_str(), "Test Publisher");
        let description: String = db.connection.query_row("SELECT description FROM book_description", [], |r| r.get(0)).unwrap();
        assert_eq!(description, "An audiobook description.");
        assert_eq!(db.audiobook_chapters(&hash).unwrap()[0].title, "Opening");
        let status = db.audiobook_enrichment_status().unwrap();
        assert_eq!(status[0].status, "applied");
        assert!(status[0].response_json.as_ref().unwrap().contains("audnexus"));
        assert!(db.claim_audible_batch(401, hash).unwrap().is_none());
        let refreshed = db.claim_audiobook_refresh(401, hash).unwrap().unwrap();
        response.candidates[0].publisher = Some("Different Publisher".into());
        response.candidates[0].description = Some("Different description".into());
        response.candidates[0].isbns = vec!["9780140328721".into()];
        assert!(db.resolve_audible_batch(&refreshed, &response, 701, 401).unwrap());
        let raw: Vec<u8> = db.connection.query_row("SELECT book_metadata FROM book", [], |r| r.get(0)).unwrap();
        let book = crate::sync::decode_book_metadata(&raw).unwrap();
        assert_eq!(book.publishers[0].name().as_str(), "Test Publisher");
        assert!(book.identifiers.iter().any(|id| id.value() == "9780306406157"));
        let description: String = db.connection.query_row("SELECT description FROM book_description", [], |r| r.get(0)).unwrap();
        assert_eq!(description, "An audiobook description.");
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn fresh_schema_has_attempts_but_no_resolved_entity_tables() {
        let (database, path) = open_test_database();
        let entity_tables: i64 = database
            .connection
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('external_entity', 'external_entity_alias', 'external_place_edge', 'book_external_entity')", [], |row| row.get(0))
            .expect("count entity tables");
        assert_eq!(entity_tables, 0, "person/place entity resolution was removed; the schema must not recreate its tables");
        let attempt_tables: i64 = database.connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='external_metadata_attempt'", [], |row| row.get(0)).expect("count attempt table");
        assert_eq!(attempt_tables, 1, "external metadata attempts are still recorded per book and provider");
        drop(database);
        let _ = std::fs::remove_file(&path);
    }
}
