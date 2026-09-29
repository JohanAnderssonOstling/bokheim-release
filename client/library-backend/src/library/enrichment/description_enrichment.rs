use super::application::record_enrichment_failures;
use super::DESCRIPTION_PROVIDER;

use crate::{library::LibrarySession, BackendError, ContentHash};
use library_database::DescriptionEnrichmentResult;
use metadata_contract::MetadataEnrichmentRequest;
use std::collections::{BTreeSet, HashMap};

impl LibrarySession {
    pub(super) async fn enrich_description_candidates(&self, candidates: Vec<(ContentHash, Vec<String>)>) -> Result<usize, BackendError> {
        let isbns = candidates.iter().flat_map(|(_, ids)| ids.iter().cloned()).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
        let mut results = HashMap::new();
        for batch in isbns.chunks(metadata_contract::MAX_ISBNS_PER_REQUEST) {
            match self.cached_metadata_enrichment(&MetadataEnrichmentRequest { isbns: batch.to_vec() }).await {
                Ok(response) => results.extend(response.results.into_iter().map(|r| (r.requested_isbn.clone(), r))),
                Err(error) => {
                    record_enrichment_failures(self, DESCRIPTION_PROVIDER, candidates.iter().map(|(hash, ids)| (*hash, ids.join(","))).collect(), error.to_string()).await?;
                    return Err(error);
                }
            }
        }
        let results = candidates
            .into_iter()
            .map(|(hash, isbns)| {
                let missing = isbns.iter().any(|id| !results.contains_key(id));
                let descriptions = isbns.iter().filter_map(|id| results.get(id)).flat_map(|r| &r.matches).filter_map(|m| m.description.as_deref()).map(str::trim).filter(|text| !text.is_empty()).collect::<BTreeSet<_>>();
                let (status, description) = if missing {
                    ("failed", None)
                } else if descriptions.len() == 1 {
                    ("updated", descriptions.into_iter().next().map(str::to_owned))
                } else if descriptions.is_empty() {
                    ("no_match", None)
                } else {
                    ("no_match", None)
                };
                DescriptionEnrichmentResult { content_hash: hash, provider: DESCRIPTION_PROVIDER.to_owned(), identifier: isbns.join(","), description, status: status.to_owned() }
            })
            .collect::<Vec<_>>();
        let mut updated = 0;
        for result in results {
            updated += self.db.apply_description_enrichment(result).map_err(BackendError::operation)?;
        }
        Ok(updated)
    }
}
