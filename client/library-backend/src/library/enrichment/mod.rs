//! Coordinates feature-specific metadata enrichment.
fn enrichment_parallelism() -> (usize, usize) {
    let cpus = crate::executor::available_parallelism();
    (book_enrichment::background_worker_limit(cpus), cpus.clamp(1, 4))
}

mod description_enrichment;
mod subject_pipeline;
const DESCRIPTION_PROVIDER: &str = "openlibrary_description_v1";

const OPEN_LIBRARY_AUTHOR_PROVIDER: &str = "openlibrary_authors_v3";
// v19 ignores legacy Dewey evidence; local print ISBN inspection remains enabled.
const OPEN_LIBRARY_RICH_PROVIDER: &str = "openlibrary_rich_v19";
const OPEN_LIBRARY_EDITION_IDENTITY_PROVIDER: &str = "openlibrary_edition_identity_v17";
const OPEN_LIBRARY_COVER_PROVIDER: &str = "openlibrary_covers_v2";

// Edition matching can involve large candidate intersections in the dump.
// Smaller requests keep one pathological classic from timing out an entire
// 128-book retry batch and leave less work to repeat after interruption.
const EDITION_IDENTITY_BATCH_SIZE: usize = 8;
const ISBN_LOOKUP_BATCH_SIZE: usize = 8;
const MAX_DIRECT_CLASSIFICATIONS_WITHOUT_CONSENSUS: usize = 12;

mod application;
mod scheduling;

mod authors;
mod covers;
mod editions;
mod rich_metadata;
