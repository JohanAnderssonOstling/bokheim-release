//! Author identity and credit enrichment.
use super::application::record_enrichment_failures;
use super::OPEN_LIBRARY_AUTHOR_PROVIDER;
use crate::library::LibrarySession;
use crate::BackendError;
use book_enrichment::resolution::{authoritative_author_resolutions, match_author_resolutions, merge_rich_matches};
use metadata_contract::{LookupStatus, MetadataEnrichmentRequest};
use std::collections::{BTreeSet, HashMap};

impl LibrarySession {
    /// Resolves scanned author credits through the public authority metadata
    /// endpoint. Requests contain ISBNs only; library ids, titles, paths, and
    /// credited names remain on the device.
    pub(super) async fn apply_author_candidates(&self, candidates: Vec<library_database::AuthorEnrichmentCandidate>) -> Result<usize, BackendError> {
        let mut updated = 0_usize;
        for batch in candidates.chunks(metadata_contract::MAX_ISBNS_PER_REQUEST) {
            let mut isbns = batch.iter().flat_map(|candidate| candidate.isbns.iter().cloned()).collect::<Vec<_>>();
            isbns.sort_unstable();
            isbns.dedup();
            let mut results = HashMap::new();
            for isbns in isbns.chunks(metadata_contract::MAX_ISBNS_PER_REQUEST) {
                let response = match self.cached_metadata_enrichment(&MetadataEnrichmentRequest { isbns: isbns.to_vec() }).await {
                    Ok(response) => response,
                    Err(error) => {
                        let failures = batch.iter().map(|candidate| (candidate.content_hash, candidate.isbns.join(","))).collect();
                        record_enrichment_failures(self, OPEN_LIBRARY_AUTHOR_PROVIDER, failures, error.to_string()).await?;
                        return Err(error);
                    }
                };
                for result in response.results {
                    results.insert(result.requested_isbn.clone(), result);
                }
            }
            let mut author_outcomes: Vec<library_database::AuthorEnrichmentOutcome> = Vec::new();
            let mut authoritative_outcomes: Vec<library_database::AuthoritativeAuthorsOutcome> = Vec::new();
            for candidate in batch {
                let available = candidate.isbns.iter().filter_map(|isbn| results.get(isbn)).collect::<Vec<_>>();
                if available.len() != candidate.isbns.len() {
                    author_outcomes.push(library_database::AuthorEnrichmentOutcome {
                        content_hash: candidate.content_hash,
                        provider: OPEN_LIBRARY_AUTHOR_PROVIDER.to_owned(),
                        isbn: candidate.isbns.join(","),
                        resolutions: Vec::new(),
                        status: "failed".to_owned(),
                        detail: Some("metadata response omitted an ISBN".to_owned()),
                    });
                    continue;
                }
                let authoritative = available.iter().filter_map(|result| authoritative_author_resolutions(result).map(|authors| (*result, authors))).collect::<Vec<_>>();
                if let Some((_result, authors)) = authoritative.first() {
                    if authoritative.iter().any(|(_, other)| other != authors) {
                        author_outcomes.push(library_database::AuthorEnrichmentOutcome {
                            content_hash: candidate.content_hash,
                            provider: OPEN_LIBRARY_AUTHOR_PROVIDER.to_owned(),
                            isbn: candidate.isbns.join(","),
                            resolutions: Vec::new(),
                            status: "no_match".to_owned(),
                            detail: Some("inspected ISBN records disagree on author credits".to_owned()),
                        });
                    } else {
                        authoritative_outcomes.push(library_database::AuthoritativeAuthorsOutcome {
                            content_hash: candidate.content_hash,
                            provider: OPEN_LIBRARY_AUTHOR_PROVIDER.to_owned(),
                            isbn: candidate.isbns.join(","),
                            authors: author_resolutions(authors),
                        });
                    }
                    continue;
                }
                let result = available.iter().copied().find(|r| !r.matches.is_empty()).or_else(|| available.first().copied());
                let (status, resolutions, detail) = match result {
                    None => ("failed", Vec::new(), Some("metadata response omitted the requested ISBN".to_owned())),
                    Some(result) if result.status == LookupStatus::NoMatch || result.status == LookupStatus::InvalidIsbn => ("no_match", Vec::new(), None),
                    Some(result) if result.matches.is_empty() => ("no_match", Vec::new(), Some("ISBN returned no Open Library work metadata".to_owned())),
                    Some(result) => {
                        let work_count = result.matches.iter().filter_map(|matched| matched.open_library_work_id.as_deref()).collect::<BTreeSet<_>>().len();
                        let merged = merge_rich_matches(&result.matches);
                        let (status, resolutions, detail) = match_author_resolutions(&candidate.authors.iter().map(|credit| (credit.position as usize, credit.name.as_str())).collect::<Vec<_>>(), &merged.authors);
                        let detail = detail.or_else(|| (work_count > 1).then(|| format!("merged author evidence from {work_count} Open Library work records")));
                        (status, resolutions, detail)
                    }
                };
                let mut author_resolutions = Vec::with_capacity(resolutions.len());
                for (position, name, external_id) in resolutions {
                    let parsed = book_model::AuthorName::parse(&name).map_err(BackendError::operation)?;
                    author_resolutions.push(library_database::AuthorResolution { position, preferred_name: name, match_key: parsed.match_key().as_str().to_owned(), external_id });
                }
                author_outcomes.push(library_database::AuthorEnrichmentOutcome {
                    content_hash: candidate.content_hash,
                    provider: OPEN_LIBRARY_AUTHOR_PROVIDER.to_owned(),
                    isbn: candidate.isbns.join(","),
                    resolutions: author_resolutions,
                    status: status.to_owned(),
                    detail,
                });
                crate::executor::sleep(std::time::Duration::from_millis(1)).await;
            }
            for outcome in author_outcomes {
                updated += self.db.apply_author_enrichment(outcome).map_err(BackendError::operation)?;
            }
            for outcome in authoritative_outcomes {
                updated += self.db.apply_authoritative_authors(outcome).map_err(BackendError::operation)?;
            }
        }
        Ok(updated)
    }
}

/// Name parsing is pure producer work: unparseable credits never become
/// outcomes, so Database methods only insert.
pub(super) fn author_resolutions(authors: &[(usize, String, book_model::ExternalAuthorId)]) -> Vec<library_database::AuthorResolution> {
    authors
        .iter()
        .filter_map(|(position, name, external_id)| {
            let parsed = book_model::AuthorName::parse(name).ok()?;
            if parsed.match_key().as_str().is_empty() {
                return None;
            }
            Some(library_database::AuthorResolution { position: *position, preferred_name: name.clone(), match_key: parsed.match_key().as_str().to_owned(), external_id: external_id.clone() })
        })
        .collect()
}
