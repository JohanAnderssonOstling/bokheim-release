//! Edition matching and automatic subject enrichment.
use super::application::record_enrichment_failures;
use super::{enrichment_parallelism, OPEN_LIBRARY_RICH_PROVIDER};
use super::{EDITION_IDENTITY_BATCH_SIZE, OPEN_LIBRARY_EDITION_IDENTITY_PROVIDER};
use crate::library::LibrarySession;
use crate::BackendError;
use book_enrichment::resolution::clean_fallback_authors;
use book_enrichment::resolution::{classification_only_fallback, rich_subjects};
use futures_util::{stream, StreamExt};
use library_database::EditionIdentityCandidate;
use metadata_contract::{authority_subjects, subject_consensus, EditionIdentityQuery, EditionIdentityRequest, EditionIdentityStatus, MAX_EDITION_QUERIES_PER_REQUEST};
use std::collections::HashMap;

fn edition_identity_query(candidate: &EditionIdentityCandidate, query_id: String) -> EditionIdentityQuery {
    EditionIdentityQuery { query_id, title: candidate.title.clone(), authors: clean_fallback_authors(&candidate.authors), publishers: candidate.publishers.clone(), book_year: candidate.book_year }
}

impl LibrarySession {
    /// Adds an ISBN only when dump evidence identifies one exact edition. An
    /// embedded/scanned ISBN always wins because books that already have one
    /// are excluded both while selecting and while applying candidates.
    pub(super) async fn resolve_open_library_edition_identities(&self, candidates: Vec<EditionIdentityCandidate>) -> Result<usize, BackendError> {
        let mut updated = 0;
        let mut unresolved = Vec::with_capacity(candidates.len());
        let mut filename_outcomes: Vec<library_database::EditionIdentityOutcome> = Vec::new();
        for candidate in candidates {
            if let Some(isbn) = book_enrichment::filename_isbn::from_filename_single(&candidate.title) {
                filename_outcomes.push(library_database::EditionIdentityOutcome {
                    content_hash: candidate.content_hash,
                    provider: OPEN_LIBRARY_EDITION_IDENTITY_PROVIDER.to_owned(),
                    identifier: isbn.clone(),
                    isbn: Some(isbn),
                    status: "updated".to_owned(),
                    detail: Some("checksum-valid ISBN extracted from filename".to_owned()),
                });
            } else {
                unresolved.push(candidate);
            }
        }
        updated += self.flush_edition_identity_outcomes(filename_outcomes).await?;
        let batches = unresolved.chunks((EDITION_IDENTITY_BATCH_SIZE.min(MAX_EDITION_QUERIES_PER_REQUEST) / (book_enrichment::filename_lookup::MAX_FILENAME_QUERIES + 1)).max(1)).map(<[_]>::to_vec).collect::<Vec<_>>();
        let mut requests = stream::iter(
            batches
                .into_iter()
                .map(|batch| async move {
                    let queries = batch
                        .iter()
                        .enumerate()
                        .flat_map(|(index, candidate)| {
                            let base = edition_identity_query(candidate, index.to_string());
                            let alternatives = book_enrichment::filename_lookup::queries(&base, &candidate.filenames);
                            std::iter::once(base).chain(alternatives)
                        })
                        .collect();
                    (batch, self.metadata().resolve_edition_identities(&EditionIdentityRequest { queries }).await.map_err(BackendError::operation))
                })
                .collect::<Vec<_>>(),
        )
        .buffer_unordered(enrichment_parallelism().1);
        while let Some((batch, outcome)) = requests.next().await {
            let response = match outcome {
                Ok(response) => response,
                Err(error) => {
                    let failures = batch.iter().map(|candidate| (candidate.content_hash, candidate.title.clone())).collect::<Vec<_>>();
                    record_enrichment_failures(self, OPEN_LIBRARY_EDITION_IDENTITY_PROVIDER, failures, error.to_string()).await?;
                    log::warn!("Open Library edition matching skipped a batch of {} books after an error: {error}", batch.len());
                    continue;
                }
            };
            let results = response.results.into_iter().map(|result| (result.query_id.clone(), result)).collect::<HashMap<_, _>>();
            let mut rich_outcomes: Vec<library_database::RichEnrichmentOutcome> = Vec::new();
            let mut edition_outcomes: Vec<library_database::EditionIdentityOutcome> = Vec::new();
            for (index, candidate) in batch.iter().enumerate() {
                let mut result = results.get(&index.to_string());
                let mut verification_query = edition_identity_query(candidate, index.to_string());
                let supported = |query: &EditionIdentityQuery, response: &metadata_contract::EditionIdentityResult| {
                    let codes = book_enrichment::filename_lookup::classifications(query, &candidate.resource_isbns, response);
                    !book_enrichment::resolution::classification_paths(&codes).is_empty()
                };
                let mut used_filename = false;
                if !result.is_some_and(|response| supported(&verification_query, response)) {
                    let alternatives = book_enrichment::filename_lookup::queries(&verification_query, &candidate.filenames);
                    let missing_alternative = alternatives.iter().any(|query| !results.contains_key(&query.query_id));
                    for alternative in alternatives {
                        if let Some(response) = results.get(&alternative.query_id).filter(|response| supported(&alternative, response)) {
                            verification_query = alternative;
                            result = Some(response);
                            used_filename = true;
                            break;
                        }
                    }
                    if !used_filename && missing_alternative {
                        result = None;
                    }
                }
                let verification_query = result.and_then(|r| metadata_contract::identity_evidence::verified_author_prefix_query(&verification_query, r)).unwrap_or(verification_query);
                if let Some(result) = result {
                    let verified = book_enrichment::related_isbn::verified_isbns(&verification_query, &candidate.resource_isbns, result);
                    let codes = book_enrichment::related_isbn::classifications(&verified, |isbn| {
                        result.matched.iter().chain(&result.candidates).filter(|record| record.canonical_isbn13 == isbn).map(|record| book_enrichment::related_isbn::classification_record(record.classifications.clone())).collect()
                    });
                    if !codes.is_empty() {
                        let record = book_enrichment::related_isbn::classification_record(codes);
                        let subjects = rich_subjects(&record).map_err(BackendError::operation)?;
                        rich_outcomes.push(library_database::RichEnrichmentOutcome {
                            content_hash: candidate.content_hash,
                            provider: OPEN_LIBRARY_RICH_PROVIDER.to_owned(),
                            isbn: String::new(),
                            matched: Some(record),
                            subjects,
                            status: "updated".to_owned(),
                            detail: Some("resource ISBN verified against title and authors".to_owned()),
                            replace_previous_work_classifications: false,
                        });
                        edition_outcomes.push(library_database::EditionIdentityOutcome {
                            content_hash: candidate.content_hash,
                            provider: OPEN_LIBRARY_EDITION_IDENTITY_PROVIDER.to_owned(),
                            identifier: "resource-isbn".to_owned(),
                            isbn: None,
                            status: "updated".to_owned(),
                            detail: Some("verified resource ISBN; book edition unchanged".to_owned()),
                        });
                        continue;
                    }
                }
                if let Some(record) = result.and_then(|r| r.authority_subjects.as_ref()).and_then(|m| authority_subjects::classification_record(&verification_query, m)) {
                    let subjects = rich_subjects(&record).map_err(BackendError::operation)?;
                    let detail = serde_json::to_string(&record.classifications).map_err(BackendError::operation)?;
                    rich_outcomes.push(library_database::RichEnrichmentOutcome {
                        content_hash: candidate.content_hash,
                        provider: OPEN_LIBRARY_RICH_PROVIDER.to_owned(),
                        isbn: String::new(),
                        matched: Some(record),
                        subjects,
                        status: "updated".to_owned(),
                        detail: Some(detail.clone()),
                        replace_previous_work_classifications: false,
                    });
                    edition_outcomes.push(library_database::EditionIdentityOutcome {
                        content_hash: candidate.content_hash,
                        provider: OPEN_LIBRARY_EDITION_IDENTITY_PROVIDER.to_owned(),
                        identifier: "lc-title-author".to_owned(),
                        isbn: None,
                        status: "updated".to_owned(),
                        detail: Some(detail),
                    });
                    continue;
                }
                let agreed_codes = result.map(|r| subject_consensus::resolved_classifications(&verification_query, r)).unwrap_or_default();
                if !agreed_codes.is_empty() {
                    let record = book_enrichment::related_isbn::classification_record(agreed_codes);
                    let subjects = rich_subjects(&record).map_err(BackendError::operation)?;
                    rich_outcomes.push(library_database::RichEnrichmentOutcome {
                        content_hash: candidate.content_hash,
                        provider: OPEN_LIBRARY_RICH_PROVIDER.to_owned(),
                        isbn: String::new(),
                        matched: Some(record),
                        subjects,
                        status: "updated".to_owned(),
                        detail: Some("classifications from matching editions; exact ISBN unresolved".to_owned()),
                        replace_previous_work_classifications: false,
                    });
                    edition_outcomes.push(library_database::EditionIdentityOutcome {
                        content_hash: candidate.content_hash,
                        provider: OPEN_LIBRARY_EDITION_IDENTITY_PROVIDER.to_owned(),
                        identifier: "subject-consensus".to_owned(),
                        isbn: None,
                        status: "updated".to_owned(),
                        detail: Some("subject resolved; exact edition remains unresolved".to_owned()),
                    });
                    continue;
                }
                if let Some(record) = result.filter(|r| r.status == EditionIdentityStatus::Matched).and_then(|r| r.matched.clone()).and_then(|matched| classification_only_fallback(&verification_query, matched)) {
                    let subjects = rich_subjects(&record).map_err(BackendError::operation)?;
                    rich_outcomes.push(library_database::RichEnrichmentOutcome {
                        content_hash: candidate.content_hash,
                        provider: OPEN_LIBRARY_RICH_PROVIDER.to_owned(),
                        isbn: "title-author".to_owned(),
                        matched: Some(record),
                        subjects,
                        status: "updated".to_owned(),
                        detail: Some("verified title/author classification fallback".to_owned()),
                        replace_previous_work_classifications: false,
                    });
                }
                let (status, isbn, identifier, detail) = match result {
                    None => ("failed", None, index.to_string(), Some("metadata response omitted the edition query".to_owned())),
                    Some(result) if result.status == EditionIdentityStatus::Matched => match &result.matched {
                        Some(matched) => {
                            let discriminator = match (matched.matched_publisher, matched.matched_book_year) {
                                (true, true) => "; publisher/year tie-break",
                                (true, false) => "; publisher tie-break",
                                (false, true) => "; year tie-break",
                                (false, false) => "",
                            };
                            ("updated", Some(matched.canonical_isbn13.as_str()), matched.open_library_edition_id.clone(), Some(format!("snapshot {}; title/author match{discriminator}", response.snapshot.dump_date)))
                        }
                        None => ("failed", None, index.to_string(), Some("matched response omitted edition evidence".to_owned())),
                    },
                    Some(result) if result.status == EditionIdentityStatus::Ambiguous => ("no_match", None, index.to_string(), None),
                    Some(result) if result.status == EditionIdentityStatus::NoMatch || result.status == EditionIdentityStatus::InsufficientMetadata => ("no_match", None, index.to_string(), None),
                    Some(_) => ("failed", None, index.to_string(), Some("unsupported edition identity result".to_owned())),
                };
                edition_outcomes.push(library_database::EditionIdentityOutcome {
                    content_hash: candidate.content_hash,
                    provider: OPEN_LIBRARY_EDITION_IDENTITY_PROVIDER.to_owned(),
                    identifier,
                    isbn: if used_filename { None } else { isbn.map(str::to_owned) },
                    status: status.to_owned(),
                    detail,
                });
            }
            updated += self.flush_rich_outcomes(rich_outcomes).await?;
            updated += self.flush_edition_identity_outcomes(edition_outcomes).await?;
        }
        Ok(updated)
    }

    /// Owner-committed edition outcomes: at most one row batch per response
    /// batch, persisted through Database methods, never connections.
    async fn flush_edition_identity_outcomes(&self, outcomes: Vec<library_database::EditionIdentityOutcome>) -> Result<usize, BackendError> {
        let mut updated = 0;
        for outcome in outcomes {
            if self.db.apply_edition_identity(outcome).map_err(BackendError::operation)? {
                updated += 1;
            }
        }
        Ok(updated)
    }

    /// Owner-committed rich outcomes, same batching as edition outcomes.
    async fn flush_rich_outcomes(&self, outcomes: Vec<library_database::RichEnrichmentOutcome>) -> Result<usize, BackendError> {
        let mut updated = 0;
        for outcome in outcomes {
            updated += self.db.apply_rich_enrichment(outcome).map_err(BackendError::operation)?;
        }
        Ok(updated)
    }
}
