use super::{
    attach_authors, attach_rich_fields, error, load_embedded_work_descriptions, load_work_bisac_classifications, load_work_descriptions_from_table, lookup_one, open_description_pool, open_library_id, open_read_pool, query_connection,
    resolve_edition_batch, Arc, AtomicU64, BTreeMap, BTreeSet, Classification, ClassificationRequest, ClassificationResponse, Connection, EditionIdentityRequest, EditionIdentityResponse, EndpointMetrics, Instant, IsbnMetadataResult,
    MetadataEnrichmentRequest, MetadataEnrichmentResponse, MetadataError, MetadataService, PathBuf, QueryConnection, RichIsbnMetadataResult, RichMetadataEnrichmentResponse, RichWorkMetadataMatch, Semaphore, ServiceState,
    SnapshotDescription, WorkMetadataMatch, DESCRIPTION_SCHEMA_VERSION, MAX_EDITION_QUERIES_PER_REQUEST, MAX_ISBNS_PER_REQUEST, QUERY_CONCURRENCY, SCHEMA_VERSION,
};
use crate::http::validate_isbn_count;
use crate::library_of_congress::{LibraryOfCongressClient, LibraryOfCongressLookup};

fn classification_to_metadata(classification: super::IsbnClassificationResult) -> IsbnMetadataResult {
    let matches = classification
        .matches
        .into_iter()
        .map(|matched| WorkMetadataMatch { open_library_work_id: matched.open_library_work_id, exact_edition_ids: matched.exact_edition_ids, classifications: matched.classifications, authors: Vec::new() })
        .collect();
    IsbnMetadataResult { requested_isbn: classification.requested_isbn, canonical_isbn13: classification.canonical_isbn13, status: classification.status, matches }
}

impl MetadataService {
    pub fn open(database: impl Into<PathBuf>) -> Result<Self, MetadataError> {
        Self::open_with_descriptions(database, None::<PathBuf>)
    }

    pub fn open_with_descriptions(database: impl Into<PathBuf>, description_database: Option<impl Into<PathBuf>>) -> Result<Self, MetadataError> {
        Self::open_with_supplements(database, description_database)
    }

    pub fn open_with_supplements(database: impl Into<PathBuf>, description_database: Option<impl Into<PathBuf>>) -> Result<Self, MetadataError> {
        let database = database.into();
        Self::open_with_stores(database.clone(), database.clone(), database, description_database)
    }

    pub fn open_with_stores(subject_database: impl Into<PathBuf>, rich_database: impl Into<PathBuf>, identity_database: impl Into<PathBuf>, description_database: Option<impl Into<PathBuf>>) -> Result<Self, MetadataError> {
        let database = subject_database.into();
        let pool = open_read_pool(&database)?;
        let (schema_version, dump_date, imported_at_ms) = {
            let connection = pool.get().map_err(error)?;
            let snapshot = connection.query_row("SELECT schema_version,dump_date,imported_at_ms FROM snapshot WHERE singleton=1", (), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))).map_err(error)?;
            (snapshot.0, snapshot.1, snapshot.2)
        };
        if schema_version != SCHEMA_VERSION {
            return Err(MetadataError(format!("unsupported metadata database schema {schema_version}; expected {SCHEMA_VERSION}")));
        }
        let rich_pool = open_read_pool(&rich_database.into())?;
        validate_store_snapshot(&rich_pool, &dump_date, "rich")?;
        let embedded_descriptions_available = {
            let connection = rich_pool.get().map_err(error)?;
            connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='work_description')", (), |row| row.get::<_, bool>(0)).map_err(error)?
        };
        let identity_pool = open_read_pool(&identity_database.into())?;
        validate_store_snapshot(&identity_pool, &dump_date, "identity")?;
        let description_pool = description_database
            .map(Into::into)
            .map(|description_database| {
                let description_pool = open_description_pool(&description_database)?;
                let connection = description_pool.get().map_err(error)?;
                let (description_schema, description_dump_date): (i64, String) =
                    connection.query_row("SELECT schema_version,dump_date FROM description_snapshot WHERE singleton=1", (), |row| Ok((row.get(0)?, row.get(1)?))).map_err(error)?;
                if description_schema != DESCRIPTION_SCHEMA_VERSION {
                    return Err(MetadataError(format!("unsupported description sidecar schema {description_schema}; expected {DESCRIPTION_SCHEMA_VERSION}")));
                }
                if description_dump_date != dump_date {
                    return Err(MetadataError(format!("description sidecar dump date {description_dump_date} does not match metadata snapshot {dump_date}")));
                }
                drop(connection);
                Ok(description_pool)
            })
            .transpose()?;
        let descriptions_available = description_pool.is_some() || embedded_descriptions_available;
        {
            let c = query_connection(&rich_pool)?;
            crate::authority_ingestion::available(&c)?;
        }
        let imported_at_ms = u64::try_from(imported_at_ms).map_err(|_| MetadataError("metadata snapshot has an invalid import timestamp".to_owned()))?;
        Ok(Self {
            state: Arc::new(ServiceState {
                database,
                pool: Some(pool),
                rich_pool,
                identity_pool,
                subject_index: None,
                description_pool,
                query_slots: Arc::new(Semaphore::new(QUERY_CONCURRENCY as usize)),
                snapshot: SnapshotDescription { dump_date, imported_at_ms },
                schema_version,
                descriptions_available,
                started_at: Instant::now(),
                requests: AtomicU64::new(0),
                errors: AtomicU64::new(0),
                total_duration_ms: AtomicU64::new(0),
                processed_items: AtomicU64::new(0),
                item_processing_duration_ms: AtomicU64::new(0),
                edition_identities: EndpointMetrics::default(),
                classifications: EndpointMetrics::default(),
                enrichment: EndpointMetrics::default(),
            }),
            library_of_congress: None,
            librarything: None,
        })
    }

    pub fn with_librarything(mut self, key_file: &std::path::Path, cache_file: &std::path::Path) -> Result<Self, MetadataError> {
        self.librarything = Some(crate::librarything::LibraryThingClient::open(key_file, cache_file)?);
        Ok(self)
    }

    pub fn start_librarything_background(&self, ratings: Option<PathBuf>) -> Option<tokio::task::JoinHandle<()>> {
        let client = self.librarything.clone()?;
        Some(tokio::spawn(crate::librarything_background::run(self.clone(), client, ratings)))
    }

    pub(crate) async fn apply_librarything_fallback(&self, response: &mut RichMetadataEnrichmentResponse) {
        if let Some(client) = &self.librarything {
            // Bound total batch latency, including time waiting for the API limiter.
            if tokio::time::timeout(std::time::Duration::from_secs(20), crate::librarything::apply(self, client, response)).await.is_err() {
                tracing::warn!("LibraryThing fallback batch deadline reached");
            }
        }
    }

    pub fn with_library_of_congress(mut self, endpoint: &str) -> Result<Self, MetadataError> {
        self.library_of_congress = Some(LibraryOfCongressClient::with_endpoint(endpoint)?.with_identity_pool(self.state.identity_pool.clone()));
        Ok(self)
    }

    pub fn with_default_library_of_congress(self) -> Result<Self, MetadataError> {
        let mut service = self;
        service.library_of_congress = Some(LibraryOfCongressClient::new()?.with_identity_pool(service.state.identity_pool.clone()));
        Ok(service)
    }

    pub fn with_subject_index(mut self, path: impl Into<PathBuf>) -> Result<Self, MetadataError> {
        let index = crate::subject_index::SubjectIndex::open(&path.into())?;
        index.validate_snapshot(&self.state.snapshot)?;
        let state = Arc::get_mut(&mut self.state).ok_or_else(|| MetadataError("cannot configure a subject index after cloning the metadata service".to_owned()))?;
        state.subject_index = Some(Arc::new(index));
        state.pool = None;
        Ok(self)
    }

    pub fn with_cached_library_of_congress(mut self, path: &std::path::Path) -> Result<Self, MetadataError> {
        self.library_of_congress = Some(LibraryOfCongressClient::new()?.with_identity_pool(self.state.identity_pool.clone()).with_cache(path)?);
        Ok(self)
    }

    pub fn start_library_of_congress_background(&self, path: PathBuf) -> Option<tokio::task::JoinHandle<()>> {
        let provider = self.library_of_congress.clone()?;
        Some(tokio::spawn(crate::loc_background::run(self.clone(), provider, path)))
    }

    // Shared foreground admission: a code anywhere on the work or its siblings
    // makes a new provider request unnecessary. Query failures never authorize I/O.
    fn provider_isbns(&self, isbn: &str) -> Result<Option<Vec<String>>, MetadataError> {
        let Some(id) = crate::canonical_isbn13(isbn) else { return Ok(None) };
        let c = query_connection(&self.state.identity_pool)?;
        if c.query_row("SELECT EXISTS(SELECT 1 FROM edition_isbn i JOIN edition_classification c USING(edition_id) WHERE i.isbn13=? AND trim(c.notation)!='')", [id], |r| r.get::<_, bool>(0)).map_err(crate::error)? {
            return Ok(None);
        }
        let mut q = c.prepare("SELECT DISTINCT e.work_id FROM edition_isbn i JOIN edition e USING(edition_id) WHERE i.isbn13=? AND e.work_id IS NOT NULL").map_err(crate::error)?;
        let works = q.query_map([id], |r| r.get::<_, i64>(0)).map_err(crate::error)?.collect::<Result<Vec<_>, _>>().map_err(crate::error)?;
        drop(q);
        drop(c);
        let mut all = BTreeSet::from([id.to_string()]);
        for work in works {
            match crate::librarything_background::eligible(self, work)? {
                crate::librarything_background::Eligibility::Isbns(isbns) => all.extend(isbns),
                _ => return Ok(None),
            }
        }
        // Also cover ISBNs known only to the LC supplement, without an OL work.
        if self.lookup(ClassificationRequest { isbns: vec![id.to_string()] })?.results.iter().any(|r| r.matches.iter().any(|m| !m.classifications.is_empty())) {
            return Ok(None);
        }
        if let Some(lt) = &self.librarything {
            for isbn in &all {
                if lt.cached(isbn)?.as_ref().is_some_and(crate::librarything_background::has_codes) {
                    return Ok(None);
                }
            }
        }
        Ok(Some(all.into_iter().collect()))
    }

    fn loc_network_eligible(&self, isbn: &str) -> Result<bool, MetadataError> {
        let Some(isbns) = self.provider_isbns(isbn)? else { return Ok(false) };
        Ok(crate::librarything_background::lc_gate(self, &isbns)? != crate::librarything_background::LcGate::HasCode)
    }

    pub(crate) fn librarything_network_eligible(&self, isbn: &str) -> Result<bool, MetadataError> {
        let Some(isbns) = self.provider_isbns(isbn)? else { return Ok(false) };
        Ok(crate::librarything_background::lc_gate(self, &isbns)? == crate::librarything_background::LcGate::Ready)
    }

    pub(crate) async fn apply_library_of_congress_fallback(&self, response: &mut RichMetadataEnrichmentResponse) {
        let Some(provider) = &self.library_of_congress else { return };
        let merge_loc = |result: &mut metadata_contract::RichIsbnMetadataResult, value: LibraryOfCongressLookup| {
            if let LibraryOfCongressLookup::Matched { classifications, subjects } = value {
                if classifications.is_empty() && subjects.is_empty() {
                    return;
                }
                if let Some(m) = result.matches.first_mut() {
                    m.classifications.extend(classifications);
                    m.subjects.extend(subjects);
                    m.subjects.sort();
                    m.subjects.dedup();
                } else {
                    result.matches.push(RichWorkMetadataMatch { open_library_work_id: None, exact_edition_ids: Vec::new(), classifications, authors: Vec::new(), description: None, subjects });
                }
                result.canonical_isbn13 = crate::canonical_isbn13(&result.requested_isbn).map(|i| i.to_string());
                result.status = metadata_contract::LookupStatus::Matched;
            }
        };
        let apply = async {
            let mut pending = Vec::new();
            for (index, result) in response.results.iter_mut().enumerate() {
                if result.status == metadata_contract::LookupStatus::InvalidIsbn || result.matches.len() > 1 || result.matches.iter().any(|m| !m.classifications.is_empty()) {
                    continue;
                }
                let isbn = result.requested_isbn.clone();
                match provider.cached(&isbn) {
                    Ok(Some(value)) => merge_loc(result, value),
                    Ok(None) => {
                        let query = isbn.clone();
                        if matches!(crate::query::run_database(self.clone(), move |s| s.loc_network_eligible(&query)).await, Ok(Ok(true))) && provider.retry_ready(&isbn).unwrap_or(false) {
                            pending.push((index, isbn));
                        }
                    }
                    Err(e) => tracing::warn!(%e,"LC cache unavailable"),
                }
            }
            for chunk in pending.chunks(crate::library_of_congress::MAX_BATCH_ISBNS) {
                let isbns = chunk.iter().map(|(_, isbn)| isbn.clone()).collect::<Vec<_>>();
                match provider.lookup_batch(&isbns).await {
                    Ok(values) => {
                        for ((index, _), value) in chunk.iter().zip(values) {
                            merge_loc(&mut response.results[*index], value)
                        }
                    }
                    Err(e) => tracing::warn!(%e,"Library of Congress fallback batch unavailable"),
                }
            }
        };
        if tokio::time::timeout(std::time::Duration::from_secs(35), apply).await.is_err() {
            tracing::warn!("LC fallback batch deadline reached");
        }
    }

    pub fn lookup(&self, request: ClassificationRequest) -> Result<ClassificationResponse, MetadataError> {
        if request.isbns.is_empty() {
            return Err(MetadataError("at least one ISBN is required".to_owned()));
        }
        if request.isbns.len() > MAX_ISBNS_PER_REQUEST {
            return Err(MetadataError(format!("at most {MAX_ISBNS_PER_REQUEST} ISBNs may be requested")));
        }
        let mut results = Vec::with_capacity(request.isbns.len());
        if let Some(index) = &self.state.subject_index {
            for requested_isbn in request.isbns {
                results.push(index.lookup(requested_isbn)?);
            }
        } else {
            let connection = self.connection()?;
            for requested_isbn in request.isbns {
                results.push(lookup_one(&connection, requested_isbn)?);
            }
        }
        if results.iter().any(|r| r.matches.len() == 1 && !r.matches[0].classifications.iter().any(|c| c.scheme == metadata_contract::ClassificationScheme::LibraryOfCongress)) {
            let identity = query_connection(&self.state.identity_pool)?;
            for result in &mut results {
                crate::duplicate_work::recover(&identity, result)?;
            }
        }
        Ok(ClassificationResponse { snapshot: self.state.snapshot.clone(), results })
    }

    pub fn enrich(&self, request: MetadataEnrichmentRequest) -> Result<MetadataEnrichmentResponse, MetadataError> {
        validate_isbn_count(request.isbns.len())?;
        let mut results = self.lookup_metadata(request.isbns)?;
        let rich = query_connection(&self.state.rich_pool)?;
        attach_authors(&rich, &mut results)?;
        Ok(MetadataEnrichmentResponse { snapshot: self.state.snapshot.clone(), results })
    }

    pub fn enrich_rich(&self, request: MetadataEnrichmentRequest) -> Result<RichMetadataEnrichmentResponse, MetadataError> {
        validate_isbn_count(request.isbns.len())?;
        let mut metadata = self.lookup_metadata(request.isbns)?;
        let connection = query_connection(&self.state.rich_pool)?;
        attach_authors(&connection, &mut metadata)?;
        let work_ids = metadata.iter().flat_map(|result| &result.matches).filter_map(|matched| matched.open_library_work_id.as_deref().and_then(|value| open_library_id(value, 'W'))).collect::<BTreeSet<_>>();
        let (descriptions, bisac) = self.load_work_supplements(&connection, &work_ids)?;
        let results = attach_rich_fields(&connection, metadata, &descriptions, &bisac)?;
        let mut response = RichMetadataEnrichmentResponse { snapshot: self.state.snapshot.clone(), results };
        crate::loc_dump::enrich(&connection, &mut response)?;
        Ok(response)
    }

    pub fn classify_rich(&self, request: MetadataEnrichmentRequest) -> Result<RichMetadataEnrichmentResponse, MetadataError> {
        validate_isbn_count(request.isbns.len())?;
        let classifications = self.lookup(ClassificationRequest { isbns: request.isbns })?.results;
        let work_ids = classifications.iter().flat_map(|classification| &classification.matches).filter_map(|matched| matched.open_library_work_id.as_deref().and_then(|value| open_library_id(value, 'W'))).collect::<BTreeSet<_>>();
        let connection = query_connection(&self.state.rich_pool)?;
        let (descriptions, bisac) = self.load_work_supplements(&connection, &work_ids)?;
        let mut results = Vec::with_capacity(classifications.len());
        for classification in classifications {
            let matches = classification
                .matches
                .into_iter()
                .map(|matched| {
                    let work_id = matched.open_library_work_id.as_deref().and_then(|value| open_library_id(value, 'W'));
                    let mut classifications = matched.classifications;
                    classifications.extend(work_id.and_then(|work_id| bisac.get(&work_id)).into_iter().flatten().cloned());
                    RichWorkMetadataMatch {
                        open_library_work_id: matched.open_library_work_id,
                        exact_edition_ids: matched.exact_edition_ids,
                        classifications,
                        authors: Vec::new(),
                        description: work_id.and_then(|work_id| descriptions.get(&work_id).cloned()),
                        subjects: Vec::new(),
                    }
                })
                .collect();
            results.push(RichIsbnMetadataResult { wikidata_books: Vec::new(), requested_isbn: classification.requested_isbn, canonical_isbn13: classification.canonical_isbn13, status: classification.status, matches });
        }
        let mut response = RichMetadataEnrichmentResponse { snapshot: self.state.snapshot.clone(), results };
        crate::loc_dump::enrich(&connection, &mut response)?;
        Ok(response)
    }

    pub fn resolve_editions(&self, request: EditionIdentityRequest) -> Result<EditionIdentityResponse, MetadataError> {
        if request.queries.is_empty() || request.queries.len() > MAX_EDITION_QUERIES_PER_REQUEST {
            return Err(MetadataError(format!("between 1 and {MAX_EDITION_QUERIES_PER_REQUEST} edition queries are required")));
        }
        let connection = query_connection(&self.state.identity_pool)?;
        let mut results = resolve_edition_batch(&connection, request.queries.clone())?;
        for (query, result) in request.queries.iter().zip(&mut results) {
            if result.status == crate::EditionIdentityStatus::NoMatch {
                result.authority_subjects = crate::loc_dump::lookup_title_author(&connection, query)?;
                if result.authority_subjects.is_none() {
                    result.authority_subjects = crate::authority_ingestion::lookup_title_author(&connection, query)?;
                }
            }
        }
        Ok(EditionIdentityResponse { snapshot: self.state.snapshot.clone(), results })
    }

    pub(crate) fn connection(&self) -> Result<QueryConnection, MetadataError> {
        query_connection(self.state.pool.as_ref().ok_or_else(|| MetadataError("subject SQLite store is disabled while the mmap index is active".to_owned()))?)
    }

    fn lookup_metadata(&self, isbns: Vec<String>) -> Result<Vec<IsbnMetadataResult>, MetadataError> {
        Ok(self.lookup(ClassificationRequest { isbns })?.results.into_iter().map(classification_to_metadata).collect())
    }

    fn load_work_supplements(&self, metadata_connection: &Connection, ids: &BTreeSet<i64>) -> Result<(BTreeMap<i64, String>, BTreeMap<i64, Vec<Classification>>), MetadataError> {
        if let Some(pool) = &self.state.description_pool {
            let connection = query_connection(pool)?;
            let descriptions = load_work_descriptions_from_table(&connection, ids)?;
            let classifications = load_work_bisac_classifications(&connection, ids)?;
            return Ok((descriptions, classifications));
        }
        Ok((load_embedded_work_descriptions(metadata_connection, ids)?, BTreeMap::new()))
    }
}

fn validate_store_snapshot(pool: &super::Pool<super::SqliteConnectionManager>, expected_dump_date: &str, name: &str) -> Result<(), MetadataError> {
    let connection = pool.get().map_err(error)?;
    let dump_date: String = connection.query_row("SELECT dump_date FROM snapshot WHERE singleton=1", (), |row| row.get(0)).map_err(error)?;
    if dump_date != expected_dump_date {
        return Err(MetadataError(format!("{name} store dump date {dump_date} does not match subject store {expected_dump_date}")));
    }
    Ok(())
}
