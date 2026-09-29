//! Applies prepared enrichment results in bounded, conditional transactions.
use crate::{library::LibrarySession, BackendError, ContentHash};

// Match the existing enrichment page size; network requests never own storage.
pub(super) const ENRICHMENT_WRITE_BATCH_SIZE: usize = 32;

pub(super) async fn record_enrichment_failures(library: &LibrarySession, provider: &'static str, failures: Vec<(ContentHash, String)>, detail: String) -> Result<(), BackendError> {
    let failures = failures.into_iter().map(|(content_hash, identifier)| library_database::EnrichmentFailure { content_hash, provider: provider.to_owned(), identifier, detail: detail.clone() }).collect::<Vec<_>>();
    library.db.record_enrichment_failures(failures).map_err(BackendError::operation)
}
