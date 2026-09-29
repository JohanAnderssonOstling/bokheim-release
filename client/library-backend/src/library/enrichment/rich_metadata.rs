//! Applies classification metadata.
use super::application::record_enrichment_failures;
use super::authors::author_resolutions;
use super::{enrichment_parallelism, DESCRIPTION_PROVIDER, MAX_DIRECT_CLASSIFICATIONS_WITHOUT_CONSENSUS, OPEN_LIBRARY_AUTHOR_PROVIDER};
use super::{ISBN_LOOKUP_BATCH_SIZE, OPEN_LIBRARY_RICH_PROVIDER};
use crate::library::LibrarySession;
use crate::BackendError;
use book_enrichment::resolution::authoritative_author_resolutions;
use book_enrichment::resolution::{merge_rich_matches, rich_subjects};
use futures_util::{stream, StreamExt};
use metadata_contract::EditionClassificationConsensus;
use metadata_contract::{LookupStatus, MetadataEnrichmentRequest};
use std::collections::{BTreeSet, HashMap};

impl LibrarySession {
    /// Batch-enriches ISBN books with classifications. Canonical subject
    /// references are synchronized with the book.
    pub(super) async fn apply_rich_metadata_candidates(&self, candidates: Vec<library_database::RichEnrichmentCandidate>) -> Result<usize, BackendError> {
        let mut requested_isbns = candidates.iter().flat_map(|candidate| candidate.isbns.iter().cloned()).collect::<Vec<_>>();
        requested_isbns.sort_unstable();
        requested_isbns.dedup();
        let mut results = HashMap::new();
        let batches = requested_isbns.chunks(ISBN_LOOKUP_BATCH_SIZE.min(metadata_contract::MAX_ISBNS_PER_REQUEST)).map(<[_]>::to_vec).collect::<Vec<_>>();
        let mut requests =
            stream::iter(batches.into_iter().map(|isbns| async move { self.cached_metadata_enrichment(&MetadataEnrichmentRequest { isbns: isbns.to_vec() }).await }).collect::<Vec<_>>()).buffer_unordered(enrichment_parallelism().1);
        while let Some(outcome) = requests.next().await {
            let response = match outcome {
                Ok(response) => response,
                Err(error) => {
                    let failures = candidates.iter().map(|candidate| (candidate.content_hash, candidate.isbns.join(","))).collect::<Vec<_>>();
                    record_enrichment_failures(self, OPEN_LIBRARY_RICH_PROVIDER, failures, error.to_string()).await?;
                    return Err(error);
                }
            };
            for result in response.results {
                let requested = book_model::canonical_isbn(&result.requested_isbn).unwrap_or_else(|| result.requested_isbn.clone());
                if let Some(isbn13) = result.canonical_isbn13.as_ref().filter(|isbn13| isbn13.as_str() != requested.as_str()) {
                    results.insert(isbn13.clone(), result.clone());
                }
                results.insert(requested, result);
            }
        }
        let mut updated = 0_usize;
        let mut attempts: Vec<library_database::EnrichmentAttempt> = Vec::new();
        let mut descriptions: Vec<library_database::DescriptionEnrichmentResult> = Vec::new();
        let mut author_outcomes: Vec<library_database::AuthorEnrichmentOutcome> = Vec::new();
        let mut authoritative_outcomes: Vec<library_database::AuthoritativeAuthorsOutcome> = Vec::new();
        let mut rich_outcomes: Vec<library_database::RichEnrichmentOutcome> = Vec::new();
        for candidate in candidates {
            let mut matched_records = Vec::new();
            let mut alternative_classification_consensus = EditionClassificationConsensus::default();
            let mut matched_isbn_count = 0_usize;
            let mut omitted = false;
            for isbn in &candidate.isbns {
                match results.get(isbn) {
                    None => omitted = true,
                    Some(result) if !result.matches.is_empty() && (result.status == LookupStatus::Matched || result.status == LookupStatus::Ambiguous) => {
                        matched_isbn_count += 1;
                        let mut matches = result.matches.clone();
                        if candidate.reference_isbns.contains(isbn) && !candidate.exact_isbns.contains(isbn) {
                            matches = vec![book_enrichment::related_isbn::classification_record(book_enrichment::related_isbn::reference_classifications(isbn, matches))];
                        } else if !candidate.exact_isbns.contains(isbn) {
                            alternative_classification_consensus.record_alternative_matches(&mut matches, isbn);
                        }
                        matched_records.extend(matches);
                    }
                    Some(_) => {}
                }
            }
            if omitted {
                attempts.push(library_database::EnrichmentAttempt {
                    content_hash: candidate.content_hash,
                    provider: OPEN_LIBRARY_RICH_PROVIDER.to_owned(),
                    identifier: candidate.isbns.join(","),
                    status: "failed".to_owned(),
                    detail: Some("metadata response omitted one or more requested ISBNs".to_owned()),
                });
                continue;
            }
            if candidate.isbns.is_empty() {
                attempts.push(library_database::EnrichmentAttempt {
                    content_hash: candidate.content_hash,
                    provider: OPEN_LIBRARY_RICH_PROVIDER.to_owned(),
                    identifier: String::new(),
                    status: "no_match".to_owned(),
                    detail: Some("no verified print ISBN classification evidence".to_owned()),
                });
                continue;
            }
            let available = candidate.isbns.iter().filter_map(|isbn| results.get(isbn)).collect::<Vec<_>>();
            let description_candidates = available.iter().flat_map(|r| &r.matches).filter_map(|m| m.description.as_deref()).map(str::trim).filter(|d| !d.is_empty()).collect::<BTreeSet<_>>();
            let (description_status, description) = match description_candidates.len() {
                0 => ("no_match", None),
                1 => ("updated", description_candidates.into_iter().next().map(str::to_owned)),
                _ => ("no_match", None),
            };
            descriptions.push(library_database::DescriptionEnrichmentResult {
                content_hash: candidate.content_hash,
                provider: DESCRIPTION_PROVIDER.to_owned(),
                identifier: candidate.isbns.join(","),
                description,
                status: description_status.to_owned(),
            });
            let authoritative = available.iter().filter_map(|result| authoritative_author_resolutions(result)).collect::<Vec<_>>();
            if let Some(authors) = authoritative.first() {
                let ids = candidate.isbns.join(",");
                if authoritative.iter().any(|other| other != authors) {
                    author_outcomes.push(library_database::AuthorEnrichmentOutcome {
                        content_hash: candidate.content_hash,
                        provider: OPEN_LIBRARY_AUTHOR_PROVIDER.to_owned(),
                        isbn: ids,
                        resolutions: Vec::new(),
                        status: "no_match".to_owned(),
                        detail: Some("inspected ISBN records disagree on author credits".to_owned()),
                    });
                } else if !self.db.author_rich_already_applied(&candidate.content_hash, OPEN_LIBRARY_AUTHOR_PROVIDER, &ids).map_err(BackendError::operation)? {
                    authoritative_outcomes.push(library_database::AuthoritativeAuthorsOutcome { content_hash: candidate.content_hash, provider: OPEN_LIBRARY_AUTHOR_PROVIDER.to_owned(), isbn: ids, authors: author_resolutions(authors) });
                }
            }
            let global_alternative_classifications = alternative_classification_consensus.classifications();
            let original_has_classifications = !global_alternative_classifications.is_empty() || matched_records.iter().any(|matched| !matched.classifications.is_empty());
            let identifier = candidate.isbns.join(",");
            let (status, matched, subjects, detail, replace_previous_work_classifications) = if matched_records.is_empty() {
                if omitted {
                    ("failed", None, Vec::new(), Some("metadata response omitted one or more requested ISBNs".to_owned()), false)
                } else {
                    ("no_match", None, Vec::new(), Some(format!("checked {} ISBNs", candidate.isbns.len())), false)
                }
            } else {
                let work_count = matched_records.iter().filter_map(|matched| matched.open_library_work_id.as_deref()).collect::<BTreeSet<_>>().len();
                let mut matched = merge_rich_matches(&matched_records);
                matched.classifications.extend(global_alternative_classifications);
                matched.classifications.sort();
                matched.classifications.dedup();
                let merged_direct_classification_count = matched.classifications.len();
                let direct_consensus_replacement = original_has_classifications && candidate.classification_count > MAX_DIRECT_CLASSIFICATIONS_WITHOUT_CONSENSUS && merged_direct_classification_count < candidate.classification_count;
                let subjects = rich_subjects(&matched).map_err(BackendError::operation)?;
                let fallback_detail =
                    if direct_consensus_replacement { format!("; replaced the previous oversized work union with {merged_direct_classification_count} dump-native edition-consensus classifications") } else { String::new() };
                let detail = Some(format!("checked {} ISBNs; {matched_isbn_count} matched across {work_count} Open Library work records; merged all matches{fallback_detail}", candidate.isbns.len()));
                ("updated", Some(matched), subjects, detail, direct_consensus_replacement)
            };
            rich_outcomes.push(library_database::RichEnrichmentOutcome {
                content_hash: candidate.content_hash,
                provider: OPEN_LIBRARY_RICH_PROVIDER.to_owned(),
                isbn: identifier,
                matched,
                subjects,
                status: status.to_owned(),
                detail,
                replace_previous_work_classifications,
            });
            crate::executor::sleep(std::time::Duration::from_millis(1)).await;
        }
        if !attempts.is_empty() {
            self.db.record_enrichment_attempts(attempts).map_err(BackendError::operation)?;
        }
        for result in descriptions {
            updated += self.db.apply_description_enrichment(result).map_err(BackendError::operation)?;
        }
        for outcome in author_outcomes {
            updated += self.db.apply_author_enrichment(outcome).map_err(BackendError::operation)?;
        }
        for outcome in authoritative_outcomes {
            updated += self.db.apply_authoritative_authors(outcome).map_err(BackendError::operation)?;
        }
        for outcome in rich_outcomes {
            updated += self.db.apply_rich_enrichment(outcome).map_err(BackendError::operation)?;
        }
        Ok(updated)
    }
}
