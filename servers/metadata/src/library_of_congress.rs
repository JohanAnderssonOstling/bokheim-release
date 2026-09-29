//! Server-owned exact-ISBN access to the Library of Congress SRU authority service.
//!
//! LCC, DDC, LCSH, and LCGFT values are cached with provider provenance.
//! This module neither bundles classification schedules nor matches embedded
//! book subjects against them.

use crate::{canonical_isbn13, Classification, ClassificationScheme, ClassificationSource, MetadataError, SubjectHeading};
use futures_util::StreamExt;
use reqwest::Url;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

const PROVIDER_ID: &str = "library_of_congress";
pub(crate) const DEFAULT_SRU_ENDPOINT: &str = "http://lx2.loc.gov:210/LCDB";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_BATCH_ISBNS: usize = 50;
const PAGE_SIZE: usize = 100;
const MAX_BATCH_RECORDS: usize = 1000;
const REQUEST_INTERVAL_MS: i64 = 6000;

#[derive(Clone)]
pub(crate) struct LibraryOfCongressClient {
    client: reqwest::Client,
    endpoint: Url,
    next_request_ms: Arc<std::sync::Mutex<i64>>,
    cache: Option<Arc<std::sync::Mutex<rusqlite::Connection>>>,
    request_lock: Arc<tokio::sync::Mutex<()>>,
    identity_pool: Option<crate::Pool<crate::SqliteConnectionManager>>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum LibraryOfCongressLookup {
    Matched { classifications: Vec<Classification>, subjects: Vec<SubjectHeading> },
    NoMatch,
    Ambiguous,
}

#[derive(Clone, Debug)]
struct LibraryOfCongressRecord {
    provider_record_id: Option<String>,
    isbns: Vec<String>,
    related_isbns: Vec<String>,
    title: String,
    authors: Vec<String>,
    classifications: Vec<Classification>,
    subjects: Vec<SubjectHeading>,
}

impl LibraryOfCongressClient {
    pub(crate) fn new() -> Result<Self, MetadataError> {
        Self::with_endpoint(DEFAULT_SRU_ENDPOINT)
    }

    pub(crate) fn with_endpoint(endpoint: &str) -> Result<Self, MetadataError> {
        let endpoint = Url::parse(endpoint).map_err(|error| MetadataError(format!("invalid Library of Congress SRU endpoint: {error}")))?;
        let official_http = endpoint.scheme() == "http" && endpoint.host_str() == Some("lx2.loc.gov") && endpoint.port() == Some(210);
        let test_http = cfg!(test) && endpoint.scheme() == "http" && endpoint.host_str() == Some("127.0.0.1");
        if (endpoint.scheme() != "https" && !official_http && !test_http) || !endpoint.username().is_empty() || endpoint.password().is_some() || endpoint.query().is_some() || endpoint.fragment().is_some() {
            return Err(MetadataError("Library of Congress SRU endpoint must use HTTPS or the official LC port-210 server, without credentials, query, or fragment".to_owned()));
        }
        let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).timeout(Duration::from_secs(15)).build().map_err(|error| MetadataError(format!("could not create Library of Congress HTTP client: {error}")))?;
        Ok(Self { client, endpoint, next_request_ms: Arc::new(std::sync::Mutex::new(0)), cache: None, request_lock: Arc::new(tokio::sync::Mutex::new(())), identity_pool: None })
    }

    pub(crate) fn with_identity_pool(mut self, pool: crate::Pool<crate::SqliteConnectionManager>) -> Self {
        self.identity_pool = Some(pool);
        self
    }

    pub(crate) fn with_cache(mut self, path: &std::path::Path) -> Result<Self, MetadataError> {
        let mut c = rusqlite::Connection::open(path).map_err(crate::error)?;
        c.busy_timeout(Duration::from_secs(2)).map_err(crate::error)?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS loc_api_result(isbn13 TEXT PRIMARY KEY,result TEXT NOT NULL,checked_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS loc_api_failure(isbn13 TEXT PRIMARY KEY,retry_at INTEGER NOT NULL,error TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS loc_api_rate(id INTEGER PRIMARY KEY CHECK(id=1),next_ms INTEGER NOT NULL,requests INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS loc_api_rejected(isbn13 TEXT PRIMARY KEY,original_result TEXT NOT NULL,rejected_at INTEGER NOT NULL);",
        )
        .map_err(crate::error)?;
        // Repair earlier MARC shelf-control values mislabelled as LCC, retaining an audit copy.
        let rows = {
            let mut q = c.prepare("SELECT isbn13,result FROM loc_api_result").map_err(crate::error)?;
            let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(crate::error)?.collect::<Result<Vec<_>, _>>().map_err(crate::error)?;
            rows
        };
        let tx = c.transaction().map_err(crate::error)?;
        let mut repaired = false;
        for (isbn, raw) in rows {
            let mut value: LibraryOfCongressLookup = serde_json::from_str(&raw).map_err(crate::error)?;
            if let LibraryOfCongressLookup::Matched { classifications, .. } = &mut value {
                let before = classifications.len();
                classifications.retain(valid_classification);
                if before != classifications.len() {
                    tx.execute("INSERT OR IGNORE INTO loc_api_rejected VALUES(?,?,?)", rusqlite::params![isbn, raw, epoch()]).map_err(crate::error)?;
                    tx.execute("UPDATE loc_api_result SET result=? WHERE isbn13=?", rusqlite::params![serde_json::to_string(&value).map_err(crate::error)?, isbn]).map_err(crate::error)?;
                    repaired = true;
                }
            }
        }
        if repaired && tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='loc_background_work')", [], |r| r.get::<_, bool>(0)).map_err(crate::error)? {
            tx.execute("UPDATE loc_background_work SET status='pending',retry_at=0 WHERE status IN ('subjects_found','cached_subjects')", []).map_err(crate::error)?;
        }
        tx.execute_batch("CREATE TABLE IF NOT EXISTS loc_parser_version(version INTEGER PRIMARY KEY);").map_err(crate::error)?;
        let upgraded: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM loc_parser_version WHERE version=2)", [], |r| r.get(0)).map_err(crate::error)?;
        if !upgraded {
            tx.execute("DELETE FROM loc_api_result WHERE result=?", [serde_json::to_string(&LibraryOfCongressLookup::NoMatch).map_err(crate::error)?]).map_err(crate::error)?;
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='loc_background_work')", [], |r| r.get::<_, bool>(0)).map_err(crate::error)? {
                tx.execute("UPDATE loc_background_work SET status='pending',retry_at=0 WHERE status='no_usable_subjects'", []).map_err(crate::error)?;
            }
            tx.execute("INSERT INTO loc_parser_version VALUES(2)", []).map_err(crate::error)?;
        }
        tx.commit().map_err(crate::error)?;
        self.cache = Some(Arc::new(std::sync::Mutex::new(c)));
        Ok(self)
    }

    pub(crate) fn cached(&self, isbn: &str) -> Result<Option<LibraryOfCongressLookup>, MetadataError> {
        use rusqlite::OptionalExtension;
        let Some(cache) = &self.cache else { return Ok(None) };
        let isbn = canonical_isbn(isbn).ok_or_else(|| MetadataError("invalid LC ISBN".into()))?;
        let c = cache.lock().map_err(crate::error)?;
        let value: Option<String> = c.query_row("SELECT result FROM loc_api_result WHERE isbn13=?", [isbn], |r| r.get(0)).optional().map_err(crate::error)?;
        value.map(|s| serde_json::from_str(&s).map_err(crate::error)).transpose()
    }

    pub(crate) fn retry_ready(&self, isbn: &str) -> Result<bool, MetadataError> {
        use rusqlite::OptionalExtension;
        let Some(cache) = &self.cache else { return Ok(true) };
        let isbn = canonical_isbn(isbn).ok_or_else(|| MetadataError("invalid LC ISBN".into()))?;
        let c = cache.lock().map_err(crate::error)?;
        let retry: Option<i64> = c.query_row("SELECT retry_at FROM loc_api_failure WHERE isbn13=?", [isbn], |r| r.get(0)).optional().map_err(crate::error)?;
        Ok(!retry.is_some_and(|t| t > epoch()))
    }

    /// Cache a batch only after every result page has been retrieved and validated.
    pub(crate) async fn lookup_batch(&self, isbns: &[String]) -> Result<Vec<LibraryOfCongressLookup>, MetadataError> {
        if isbns.len() > MAX_BATCH_ISBNS {
            return Err(MetadataError("LC batch exceeds 50 ISBNs".into()));
        }
        let keys = isbns.iter().map(|i| canonical_isbn(i).ok_or_else(|| MetadataError("invalid LC ISBN".into()))).collect::<Result<Vec<_>, _>>()?;
        let mut found = std::collections::HashMap::new();
        for key in &keys {
            if let Some(v) = self.cached(key)? {
                found.insert(key.clone(), v);
            }
        }
        if keys.iter().all(|k| found.contains_key(k)) {
            return Ok(keys.iter().map(|k| found[k].clone()).collect());
        }
        let _guard = self.request_lock.lock().await;
        let mut pending = Vec::new();
        for key in &keys {
            if let Some(v) = self.cached(key)? {
                found.insert(key.clone(), v);
            } else if !pending.contains(key) {
                if !self.retry_ready(key)? {
                    return Err(MetadataError("Library of Congress retry backoff".into()));
                }
                pending.push(key.clone());
            }
        }
        if !pending.is_empty() {
            let lookup = self.request_batch(&pending).await;
            if let Some(cache) = &self.cache {
                let mut c = cache.lock().map_err(crate::error)?;
                let tx = c.transaction().map_err(crate::error)?;
                for (i, key) in pending.iter().enumerate() {
                    match &lookup {
                        Ok(values) => {
                            tx.execute("INSERT OR REPLACE INTO loc_api_result VALUES(?,?,?)", rusqlite::params![key, serde_json::to_string(&values[i]).map_err(crate::error)?, epoch()]).map_err(crate::error)?;
                            tx.execute("DELETE FROM loc_api_failure WHERE isbn13=?", [key]).map_err(crate::error)?;
                        }
                        Err(e) => {
                            tx.execute("INSERT OR REPLACE INTO loc_api_failure VALUES(?,?,?)", rusqlite::params![key, epoch() + 3600, e.to_string()]).map_err(crate::error)?;
                        }
                    }
                }
                tx.commit().map_err(crate::error)?;
            }
            for (key, value) in pending.into_iter().zip(lookup?) {
                found.insert(key, value);
            }
        }
        Ok(keys.iter().map(|k| found[k].clone()).collect())
    }

    // Persistent reservation is shared by foreground, background and restarts.
    fn reserve_request(&self, now: i64) -> Result<i64, MetadataError> {
        use rusqlite::OptionalExtension;
        if let Some(cache) = &self.cache {
            let mut c = cache.lock().map_err(crate::error)?;
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(crate::error)?;
            let next = tx.query_row("SELECT next_ms FROM loc_api_rate WHERE id=1", [], |r| r.get::<_, i64>(0)).optional().map_err(crate::error)?.unwrap_or(0);
            if next > now {
                return Ok(next - now);
            }
            tx.execute("INSERT INTO loc_api_rate VALUES(1,?,1) ON CONFLICT(id) DO UPDATE SET next_ms=excluded.next_ms,requests=requests+1", [now + REQUEST_INTERVAL_MS]).map_err(crate::error)?;
            tx.commit().map_err(crate::error)?;
        } else {
            let mut next = self.next_request_ms.lock().map_err(crate::error)?;
            if *next > now {
                return Ok(*next - now);
            }
            *next = now + REQUEST_INTERVAL_MS;
        }
        Ok(0)
    }

    async fn request_batch(&self, isbns: &[String]) -> Result<Vec<LibraryOfCongressLookup>, MetadataError> {
        let terms = isbns.iter().flat_map(|i| equivalent_isbns(i).unwrap_or_default()).map(|i| format!("bath.isbn=\"{i}\"")).collect::<Vec<_>>();
        let query = terms.join(" or ");
        let mut records = Vec::new();
        let mut expected_total = None;
        let mut seen = HashSet::new();
        loop {
            loop {
                let delay = self.reserve_request(epoch_ms())?;
                if delay == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(delay as u64)).await;
            }
            let start = records.len() + 1;
            let response = self
                .client
                .get(self.endpoint.clone())
                .query(&[("version", "1.1"), ("operation", "searchRetrieve"), ("query", query.as_str()), ("maximumRecords", &PAGE_SIZE.to_string()), ("startRecord", &start.to_string()), ("recordSchema", "marcxml")])
                .send()
                .await
                .map_err(crate::error)?;
            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS || response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                let seconds = response.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i64>().ok()).unwrap_or(60).clamp(6, 86400);
                let until = epoch_ms() + seconds * 1000;
                if let Some(cache) = &self.cache {
                    cache.lock().map_err(crate::error)?.execute("UPDATE loc_api_rate SET next_ms=MAX(next_ms,?) WHERE id=1", [until]).map_err(crate::error)?;
                } else {
                    *self.next_request_ms.lock().map_err(crate::error)? = until;
                }
            }
            let response = response.error_for_status().map_err(crate::error)?;
            if response.content_length().is_some_and(|n| n > MAX_RESPONSE_BYTES as u64) {
                return Err(MetadataError("LC response size limit".into()));
            }
            let mut stream = response.bytes_stream();
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(crate::error)?;
                if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                    return Err(MetadataError("LC response size limit".into()));
                }
                bytes.extend_from_slice(&chunk);
            }
            let (total, page) = parse_sru_page(&bytes)?;
            if total > MAX_BATCH_RECORDS {
                return Err(MetadataError("LC result set exceeds batch limit".into()));
            }
            if expected_total.is_some_and(|n| n != total) {
                return Err(MetadataError("LC result count changed during pagination".into()));
            }
            expected_total = Some(total);
            if records.len() + page.len() > total || (page.is_empty() && records.len() < total) {
                return Err(MetadataError("LC incomplete result page".into()));
            }
            for record in page {
                let id = record.provider_record_id.as_ref().ok_or_else(|| MetadataError("LC record lacks a stable identifier".into()))?;
                if !seen.insert(id.clone()) {
                    return Err(MetadataError("LC repeated record during pagination".into()));
                }
                records.push(record);
            }
            if records.len() == total {
                break;
            }
        }
        tracing::info!(isbns = isbns.len(), records = records.len(), "LC batch completed");
        let pool = self.identity_pool.clone();
        let keys = isbns.to_vec();
        tokio::task::spawn_blocking(move || {
            let connection = pool.as_ref().map(crate::query::query_connection).transpose()?;
            keys.iter().map(|isbn| match_isbn_verified(isbn, &records, connection.as_deref())).collect()
        })
        .await
        .map_err(crate::error)?
    }
}

fn epoch() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

fn epoch_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}
fn valid_classification(c: &Classification) -> bool {
    metadata_contract::classification_consensus_notation(c.scheme, &c.notation).is_some()
}
#[cfg(test)]
fn match_isbn(isbn: &str, records: &[LibraryOfCongressRecord]) -> LibraryOfCongressLookup {
    match_isbn_verified(isbn, records, None).unwrap()
}

fn match_isbn_verified(isbn: &str, records: &[LibraryOfCongressRecord], connection: Option<&rusqlite::Connection>) -> Result<LibraryOfCongressLookup, MetadataError> {
    let exact = records.iter().filter(|r| r.isbns.iter().any(|i| i == isbn)).collect::<Vec<_>>();
    let mut related = Vec::new();
    if exact.is_empty() {
        if let Some(connection) = connection {
            for record in records.iter().filter(|r| r.related_isbns.iter().any(|i| i == isbn)) {
                if verified_related_record(connection, isbn, record)? {
                    related.push(record);
                }
            }
        }
    }
    let is_related = exact.is_empty();
    let candidates = if is_related { related } else { exact };
    let Some(record) = candidates.first() else { return Ok(LibraryOfCongressLookup::NoMatch) };
    if candidates.len() > 1 {
        return Ok(LibraryOfCongressLookup::Ambiguous);
    }
    let mut classifications = record.classifications.clone();
    for c in &mut classifications {
        if is_related {
            c.source = ClassificationSource::Work;
        }
        c.evidence.push(metadata_contract::ClassificationEvidence {
            method: format!("{}:{}", if is_related { "loc_api_related_isbn_verified_title_author" } else { "loc_api_isbn" }, record.provider_record_id.as_deref().unwrap_or("unknown")),
            isbn13: isbn.to_owned(),
            open_library_work_id: String::new(),
            open_library_edition_id: String::new(),
        });
    }
    Ok(LibraryOfCongressLookup::Matched { classifications, subjects: record.subjects.clone() })
}

// 020$z is not an edition identifier. Use it only to discover subject evidence
// after the ISBN's own bibliographic title and author independently agree.
fn verified_related_record(connection: &rusqlite::Connection, isbn: &str, record: &LibraryOfCongressRecord) -> Result<bool, MetadataError> {
    use metadata_contract::matching::{matching_author_count, NormalizedTitle};
    let Some(isbn) = crate::canonical_isbn13(isbn) else { return Ok(false) };
    let mut query = connection
        .prepare_cached("SELECT e.edition_id,e.work_id,b.title FROM edition_isbn i JOIN edition e USING(edition_id) JOIN edition_bibliography b USING(edition_id) WHERE i.isbn13=? ORDER BY e.edition_id LIMIT 129")
        .map_err(crate::error)?;
    let rows = query.query_map([isbn], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, String>(2)?))).map_err(crate::error)?.collect::<Result<Vec<_>, _>>().map_err(crate::error)?;
    if rows.is_empty() || rows.len() > 128 {
        return Ok(false);
    }
    let remote = NormalizedTitle::new(&record.title);
    let mut edition_authors = connection.prepare_cached("SELECT a.name FROM edition_author r JOIN author a USING(author_id) WHERE r.edition_id=? ORDER BY r.position").map_err(crate::error)?;
    let mut work_authors = connection.prepare_cached("SELECT a.name FROM work_author r JOIN author a USING(author_id) WHERE r.work_id=? ORDER BY r.position").map_err(crate::error)?;
    for (edition, work, title) in rows {
        let local = NormalizedTitle::new(&title);
        if !local.matches(&remote) {
            return Ok(false);
        }
        let mut authors = edition_authors.query_map([edition], |r| r.get::<_, Option<String>>(0)).map_err(crate::error)?.collect::<Result<Vec<_>, _>>().map_err(crate::error)?.into_iter().flatten().collect::<Vec<_>>();
        if authors.is_empty() {
            if let Some(work) = work {
                authors = work_authors.query_map([work], |r| r.get::<_, Option<String>>(0)).map_err(crate::error)?.collect::<Result<Vec<_>, _>>().map_err(crate::error)?.into_iter().flatten().collect();
            }
        }
        if matching_author_count(&authors, &record.authors) == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}
fn parse_sru_page(bytes: &[u8]) -> Result<(usize, Vec<LibraryOfCongressRecord>), MetadataError> {
    let xml = std::str::from_utf8(bytes).map_err(crate::error)?;
    let document = roxmltree::Document::parse(xml).map_err(crate::error)?;
    if document.root_element().tag_name().name() != "searchRetrieveResponse" {
        return Err(MetadataError("unexpected LC response envelope".into()));
    }
    if let Some(d) = document.descendants().find(|n| n.tag_name().name() == "diagnostic") {
        return Err(MetadataError(format!("LC SRU diagnostic: {}", d.descendants().filter_map(|n| n.text()).collect::<Vec<_>>().join(" "))));
    }
    let total = document
        .root_element()
        .children()
        .find(|n| n.tag_name().name() == "numberOfRecords")
        .and_then(|n| n.text())
        .and_then(|v| v.trim().parse::<usize>().ok())
        .ok_or_else(|| MetadataError("LC response lacks a valid record count".into()))?;
    let mut records = document.descendants().filter(|n| n.tag_name().name() == "record" && n.tag_name().namespace() == Some("http://www.loc.gov/MARC21/slim")).map(parse_marc_record).collect::<Vec<_>>();
    if records.len() > PAGE_SIZE || records.len() > total || (total > 0 && records.is_empty()) {
        return Err(MetadataError("invalid LC page record count".into()));
    }
    for r in &mut records {
        r.isbns.sort();
        r.isbns.dedup();
        r.classifications.sort();
        r.classifications.dedup();
        r.subjects.sort();
        r.subjects.dedup();
    }
    Ok((total, records))
}
#[cfg(test)]
fn parse_sru_response(bytes: &[u8]) -> Result<Vec<LibraryOfCongressRecord>, MetadataError> {
    let (total, records) = parse_sru_page(bytes)?;
    if total != records.len() {
        return Err(MetadataError("incomplete LC response".into()));
    }
    Ok(records)
}

fn parse_marc_record(record: roxmltree::Node<'_, '_>) -> LibraryOfCongressRecord {
    let fields = record.children().filter(|node| node.tag_name().name() == "datafield").collect::<Vec<_>>();
    let first = |tag: &str, code: &str| fields.iter().find(|field| field.attribute("tag") == Some(tag)).and_then(|field| subfield(*field, code));
    let provider_record_id =
        record.children().find(|n| n.tag_name().name() == "controlfield" && n.attribute("tag") == Some("001")).and_then(|n| n.text()).map(compact_whitespace).filter(|s| !s.is_empty()).or_else(|| first("010", "a").map(compact_whitespace));
    let isbns = fields.iter().filter(|field| field.attribute("tag") == Some("020")).filter_map(|field| subfield(*field, "a")).filter_map(|value| value.split_whitespace().next()).filter_map(canonical_isbn).collect();
    let related_isbns = fields
        .iter()
        .filter(|f| f.attribute("tag") == Some("020"))
        .flat_map(|f| f.children().filter(|n| n.tag_name().name() == "subfield" && n.attribute("code") == Some("z")))
        .filter_map(|n| n.text())
        .filter_map(|s| s.split_whitespace().next())
        .filter_map(canonical_isbn)
        .collect();
    let title = [first("245", "a"), first("245", "b"), first("245", "n"), first("245", "p")].into_iter().flatten().map(|s| s.trim().trim_end_matches(['/', ':', ';', ',', ' '])).collect::<Vec<_>>().join(" ");
    let authors = fields.iter().filter(|f| matches!(f.attribute("tag"), Some("100" | "700"))).filter_map(|f| subfield(*f, "a")).map(compact_whitespace).collect();
    let mut classifications = marc_classifications(&fields, "050", ClassificationScheme::LibraryOfCongress);
    for field in fields.iter().filter(|f| f.attribute("tag") == Some("082")) {
        if let Some(notation) = subfield(*field, "a").and_then(|v| metadata_contract::classification_consensus_notation(ClassificationScheme::DeweyDecimal, v)) {
            classifications.push(Classification { scheme: ClassificationScheme::DeweyDecimal, notation, source: ClassificationSource::ExactEdition, evidence: Vec::new() });
        }
    }
    let subjects = fields.iter().filter(|field| matches!(field.attribute("tag"), Some("600" | "610" | "611" | "630" | "648" | "650" | "651" | "655"))).filter_map(|field| marc_subject(*field)).collect();
    LibraryOfCongressRecord { provider_record_id, isbns, related_isbns, title, authors, classifications, subjects }
}

fn marc_classifications(fields: &[roxmltree::Node<'_, '_>], tag: &str, scheme: ClassificationScheme) -> Vec<Classification> {
    fields
        .iter()
        .filter(|field| field.attribute("tag") == Some(tag))
        .filter_map(|field| {
            let code = [subfield(*field, "a"), subfield(*field, "b")].into_iter().flatten().map(compact_whitespace).collect::<Vec<_>>().join(" ");
            metadata_contract::classification_consensus_notation(scheme, &code).map(|_| Classification { evidence: Vec::new(), scheme, notation: code, source: ClassificationSource::ExactEdition })
        })
        .collect()
}

fn marc_subject(field: roxmltree::Node<'_, '_>) -> Option<SubjectHeading> {
    let components = field
        .children()
        .filter(|node| node.tag_name().name() == "subfield" && matches!(node.attribute("code"), Some("a" | "v" | "x" | "y" | "z")))
        .filter_map(|node| node.text())
        .map(compact_whitespace)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if components.is_empty() {
        return None;
    }
    let authority = subfield(field, "2").map(compact_whitespace).or_else(|| (field.attribute("ind2") == Some("0")).then(|| "lcsh".to_owned())).unwrap_or_else(|| "unspecified".to_owned());
    let authority = authority.to_ascii_lowercase();
    let authority_uri = subfield(field, "0").map(compact_whitespace).filter(|value| value.starts_with("https://id.loc.gov/") || value.starts_with("http://id.loc.gov/"));
    Some(SubjectHeading { source: PROVIDER_ID.to_owned(), authority, components, authority_uri })
}

fn subfield<'input>(field: roxmltree::Node<'input, 'input>, code: &str) -> Option<&'input str> {
    field.children().find(|node| node.tag_name().name() == "subfield" && node.attribute("code") == Some(code)).and_then(|node| node.text())
}

fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ").trim_end_matches(['/', ':', ';', ',', '.']).to_owned()
}

fn equivalent_isbns(value: &str) -> Option<Vec<String>> {
    let canonical = canonical_isbn(value)?;
    let mut values = vec![canonical.clone()];
    if let Some(equivalent) = isbn13_to_isbn10(&canonical) {
        values.push(equivalent);
    }
    Some(values)
}

fn canonical_isbn(value: &str) -> Option<String> {
    canonical_isbn13(value).map(|isbn| format!("{isbn:013}"))
}

fn isbn13_to_isbn10(value: &str) -> Option<String> {
    let canonical = canonical_isbn(value)?;
    if canonical.len() != 13 || !canonical.starts_with("978") {
        return None;
    }
    let mut digits = canonical[3..12].to_owned();
    let sum = digits.bytes().enumerate().map(|(index, byte)| u32::from(byte - b'0') * (10 - index as u32)).sum::<u32>();
    let check = (11 - sum % 11) % 11;
    digits.push(if check == 10 { 'X' } else { char::from(b'0' + check as u8) });
    Some(digits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARC_RESPONSE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<searchRetrieveResponse xmlns="http://www.loc.gov/zing/srw/">
  <numberOfRecords>1</numberOfRecords><records><record><recordData>
    <marc:record xmlns:marc="http://www.loc.gov/MARC21/slim">
      <marc:datafield tag="010"><marc:subfield code="a">  2019000001 </marc:subfield></marc:datafield>
      <marc:datafield tag="020"><marc:subfield code="a">978-0-306-40615-7 (hardcover)</marc:subfield></marc:datafield>
      <marc:datafield tag="245"><marc:subfield code="a">A test book :</marc:subfield><marc:subfield code="b">metadata.</marc:subfield></marc:datafield>
      <marc:datafield tag="050"><marc:subfield code="a">QA76.9</marc:subfield><marc:subfield code="b">.D3 2019</marc:subfield></marc:datafield>
      <marc:datafield tag="082"><marc:subfield code="a">005.1</marc:subfield></marc:datafield>
      <marc:datafield tag="650" ind2="0"><marc:subfield code="a">Computer software</marc:subfield><marc:subfield code="x">Development</marc:subfield><marc:subfield code="0">https://id.loc.gov/authorities/subjects/sh00000001</marc:subfield></marc:datafield>
      <marc:datafield tag="655"><marc:subfield code="a">Handbooks and manuals</marc:subfield><marc:subfield code="2">lcgft</marc:subfield></marc:datafield>
    </marc:record>
  </recordData></record></records>
</searchRetrieveResponse>"#;

    #[test]
    fn parses_lcc_ddc_and_authority_subjects() {
        let records = parse_sru_response(MARC_RESPONSE.as_bytes()).expect("valid MARC XML");
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.isbns, ["9780306406157"]);
        assert!(record.classifications.contains(&Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation: "QA76.9 .D3 2019".to_owned(), source: ClassificationSource::ExactEdition }));
        assert!(record.classifications.iter().any(|c| c.scheme == ClassificationScheme::DeweyDecimal && c.notation == "005.1"));
        assert!(record.subjects.contains(&SubjectHeading {
            source: PROVIDER_ID.to_owned(),
            authority: "lcsh".to_owned(),
            components: vec!["Computer software".to_owned(), "Development".to_owned()],
            authority_uri: Some("https://id.loc.gov/authorities/subjects/sh00000001".to_owned()),
        }));
        assert!(record.subjects.iter().any(|subject| subject.authority == "lcgft" && subject.components == ["Handbooks and manuals"]));
    }

    #[test]
    fn related_ebook_isbn_requires_independent_title_and_author() {
        let xml = include_str!("../tests/fixtures/loc-2023945417.xml");
        let doc = roxmltree::Document::parse(xml).unwrap();
        let record = parse_marc_record(doc.root_element());
        assert_eq!(record.related_isbns, ["9781943003914"]);
        assert!(!record.isbns.contains(&"9781943003914".to_owned()));
        assert_eq!(match_isbn("9781943003914", std::slice::from_ref(&record)), LibraryOfCongressLookup::NoMatch);
        let dir = tempfile::tempdir().unwrap();
        let path = crate::tests::snapshot_from(
            dir.path(),
            &[serde_json::json!({"key":"/books/OL1M", "works":[{"key":"/works/OL1W"}], "title":"The Case for Colonialism", "isbn_13":["9781943003914"]})],
            &[serde_json::json!({"key":"/works/OL1W", "title":"The Case for Colonialism", "authors":[{"author":{"key":"/authors/OL1A"}}]})],
            &[serde_json::json!({"key":"/authors/OL1A", "name":"Bruce Gilley"})],
        );
        let connection = rusqlite::Connection::open(path).unwrap();
        let records = vec![record];
        let matched = match_isbn_verified("9781943003914", &records, Some(&connection)).unwrap();
        let LibraryOfCongressLookup::Matched { classifications, .. } = matched else { panic!("verified ebook should recover subjects") };
        assert!(classifications.iter().any(|c| c.notation == "JV105 .G55 2023" && c.source == ClassificationSource::Work));
        assert!(classifications.iter().all(|c| c.evidence[0].method.starts_with("loc_api_related_isbn_verified_title_author:")));
        let mut wrong = records.clone();
        wrong[0].title = "A Different Book".into();
        assert_eq!(match_isbn_verified("9781943003914", &wrong, Some(&connection)).unwrap(), LibraryOfCongressLookup::NoMatch);
        wrong = records.clone();
        wrong[0].authors = vec!["Another Author".into()];
        assert_eq!(match_isbn_verified("9781943003914", &wrong, Some(&connection)).unwrap(), LibraryOfCongressLookup::NoMatch);
        wrong[0].authors.clear();
        assert_eq!(match_isbn_verified("9781943003914", &wrong, Some(&connection)).unwrap(), LibraryOfCongressLookup::NoMatch);
        let mut duplicate = records.clone();
        duplicate.extend(records.clone());
        assert_eq!(match_isbn_verified("9781943003914", &duplicate, Some(&connection)).unwrap(), LibraryOfCongressLookup::Ambiguous);
        // A genuine 020$a match continues to work without bibliographic context.
        assert!(matches!(match_isbn("9781943003891", &records), LibraryOfCongressLookup::Matched { .. }));
    }

    #[test]
    fn parser_upgrade_retries_old_misses_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE loc_api_result(isbn13 TEXT PRIMARY KEY,result TEXT NOT NULL,checked_at INTEGER NOT NULL);
            CREATE TABLE loc_background_work(work_id INTEGER PRIMARY KEY,status TEXT,retry_at INTEGER);
            INSERT INTO loc_background_work VALUES(1,'no_usable_subjects',123);",
        )
        .unwrap();
        let miss = serde_json::to_string(&LibraryOfCongressLookup::NoMatch).unwrap();
        c.execute("INSERT INTO loc_api_result VALUES('9781943003914',?,0)", [&miss]).unwrap();
        let client = LibraryOfCongressClient::new().unwrap().with_cache(&path).unwrap();
        assert_eq!(client.cached("9781943003914").unwrap(), None);
        assert_eq!(c.query_row("SELECT status FROM loc_background_work", [], |r| r.get::<_, String>(0)).unwrap(), "pending");
        c.execute("INSERT INTO loc_api_result VALUES('9781943003914',?,0)", [&miss]).unwrap();
        let client = LibraryOfCongressClient::new().unwrap().with_cache(&path).unwrap();
        assert_eq!(client.cached("9781943003914").unwrap(), Some(LibraryOfCongressLookup::NoMatch));
    }

    #[test]
    fn derives_the_equivalent_isbn10_for_a_978_isbn() {
        assert_eq!(equivalent_isbns("978-0-306-40615-7"), Some(vec!["9780306406157".to_owned(), "0306406152".to_owned()]));
    }
    async fn lookup_one(client: &LibraryOfCongressClient, isbn: &str) -> Result<LibraryOfCongressLookup, MetadataError> {
        client.lookup_batch(&[isbn.to_owned()]).await?.pop().ok_or_else(|| MetadataError("empty LC result".into()))
    }
    #[tokio::test]
    async fn cache_survives_reopen_and_http_errors_are_retryable() {
        use axum::{routing::get, Router};
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, Router::new().route("/", get(|| async { MARC_RESPONSE }))).await.unwrap() });
        let path = dir.path().join("cache.sqlite");
        let client = LibraryOfCongressClient::with_endpoint(&endpoint).unwrap().with_cache(&path).unwrap();
        let found = lookup_one(&client, "9780306406157").await.unwrap();
        assert!(matches!(&found,LibraryOfCongressLookup::Matched{classifications,..} if classifications.len()==2 && classifications[0].evidence[0].method.starts_with("loc_api_isbn:")));
        assert_eq!(lookup_one(&client, "9781691706631").await.unwrap(), LibraryOfCongressLookup::NoMatch);
        task.abort();
        drop(client);
        let client = LibraryOfCongressClient::with_endpoint(&endpoint).unwrap().with_cache(&path).unwrap();
        assert_eq!(lookup_one(&client, "0306406152").await.unwrap(), found);
        assert_eq!(lookup_one(&client, "9781691706631").await.unwrap(), LibraryOfCongressLookup::NoMatch);
        assert!(lookup_one(&client, "9780374610326").await.is_err());
        assert_eq!(client.cached("9780374610326").unwrap(), None);
        let c = rusqlite::Connection::open(path).unwrap();
        assert!(c.query_row("SELECT retry_at FROM loc_api_failure", [], |r| r.get::<_, i64>(0)).unwrap() > epoch());
    }

    #[test]
    fn malformed_or_truncated_success_is_not_a_negative_match() {
        assert!(parse_sru_response(b"<searchRetrieveResponse/>").is_err());
        assert!(parse_sru_response(b"<searchRetrieveResponse><numberOfRecords>1</numberOfRecords></searchRetrieveResponse>").is_err());
        assert!(parse_sru_response(b"<html/>").is_err());
        assert!(parse_sru_response(b"<searchRetrieveResponse><numberOfRecords>0</numberOfRecords></searchRetrieveResponse>").unwrap().is_empty());
    }

    #[tokio::test]
    async fn cached_enrichment_preserves_existing_work_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::tests::snapshot_from(dir.path(), &[serde_json::json!({"key":"/books/OL1M","works":[{"key":"/works/OL10W"}],"title":"Recent book","isbn_13":["9781691706631"],"publish_date":"2016"})], &[], &[]);
        let cache = dir.path().join("loc.sqlite");
        let service = crate::MetadataService::open(path).unwrap().with_cached_library_of_congress(&cache).unwrap();
        let found = LibraryOfCongressLookup::Matched {
            classifications: vec![Classification { scheme: ClassificationScheme::LibraryOfCongress, notation: "QA76".into(), source: ClassificationSource::ExactEdition, evidence: vec![] }],
            subjects: vec![],
        };
        rusqlite::Connection::open(cache).unwrap().execute("INSERT INTO loc_api_result VALUES(?,?,0)", rusqlite::params!["9781691706631", serde_json::to_string(&found).unwrap()]).unwrap();
        let mut response = service.enrich_rich(crate::MetadataEnrichmentRequest { isbns: vec!["9781691706631".into()] }).unwrap();
        let identity = response.results[0].matches[0].open_library_work_id.clone();
        service.apply_library_of_congress_fallback(&mut response).await;
        assert_eq!(response.results[0].matches.len(), 1);
        assert_eq!(response.results[0].matches[0].open_library_work_id, identity);
        assert_eq!(response.results[0].matches[0].classifications[0].notation, "QA76");
    }

    #[test]
    fn rate_reservation_is_shared_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        let first = LibraryOfCongressClient::new().unwrap().with_cache(&path).unwrap();
        let second = LibraryOfCongressClient::new().unwrap().with_cache(&path).unwrap();
        assert_eq!(first.reserve_request(10000).unwrap(), 0);
        assert_eq!(second.reserve_request(10001).unwrap(), 5999);
        assert_eq!(first.clone().reserve_request(15999).unwrap(), 1);
        drop(first);
        drop(second);
        let third = LibraryOfCongressClient::new().unwrap().with_cache(&path).unwrap();
        assert_eq!(third.reserve_request(16000).unwrap(), 0);
        assert_eq!(third.reserve_request(16000).unwrap(), 6000);
    }

    #[test]
    fn shelf_control_numbers_are_removed_and_original_cache_is_archived() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        drop(LibraryOfCongressClient::new().unwrap().with_cache(&path).unwrap());
        let c = rusqlite::Connection::open(&path).unwrap();
        let bad = LibraryOfCongressLookup::Matched {
            classifications: vec![Classification { scheme: ClassificationScheme::LibraryOfCongress, notation: "MLCSE 2017/70941 (P)".into(), source: ClassificationSource::ExactEdition, evidence: Vec::new() }],
            subjects: Vec::new(),
        };
        c.execute("INSERT INTO loc_api_result VALUES(?,?,0)", rusqlite::params!["9780306406157", serde_json::to_string(&bad).unwrap()]).unwrap();
        let client = LibraryOfCongressClient::new().unwrap().with_cache(&path).unwrap();
        assert!(matches!(client.cached("9780306406157").unwrap(),Some(LibraryOfCongressLookup::Matched{classifications,..}) if classifications.is_empty()));
        assert_eq!(c.query_row("SELECT count(*) FROM loc_api_rejected", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        let xml = MARC_RESPONSE.replace("QA76.9", "MLCM 2025/80464");
        assert!(!parse_sru_response(xml.as_bytes()).unwrap()[0].classifications.iter().any(|c| c.scheme == ClassificationScheme::LibraryOfCongress));
    }

    #[tokio::test]
    async fn batch_pages_map_exact_isbns_and_keep_other_books_separate() {
        use axum::{extract::Query, routing::get, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/",
                    get(|Query(q): Query<std::collections::HashMap<String, String>>| async move {
                        assert!(q["query"].contains("0306406152"));
                        assert!(q["query"].contains(" or "));
                        let xml = MARC_RESPONSE.replace("<numberOfRecords>1", "<numberOfRecords>2");
                        if q["startRecord"] == "1" {
                            xml
                        } else {
                            assert_eq!(q["startRecord"], "2");
                            xml.replace("2019000001", "2019000002").replace("978-0-306-40615-7", "9781691706631").replace("QA76.9", "BF637")
                        }
                    }),
                ),
            )
            .await
            .unwrap()
        });
        let dir = tempfile::tempdir().unwrap();
        let client = LibraryOfCongressClient::with_endpoint(&endpoint).unwrap().with_cache(&dir.path().join("cache")).unwrap();
        let isbns = vec!["9780306406157".into(), "9781691706631".into(), "9780374610326".into()];
        let results = client.lookup_batch(&isbns).await.unwrap();
        assert!(matches!(&results[0],LibraryOfCongressLookup::Matched{classifications,..} if classifications.iter().any(|c|c.notation.starts_with("QA76"))));
        assert!(matches!(&results[1],LibraryOfCongressLookup::Matched{classifications,..} if classifications.iter().any(|c|c.notation.starts_with("BF637"))));
        assert_eq!(results[2], LibraryOfCongressLookup::NoMatch);
        task.abort();
        assert_eq!(client.lookup_batch(&isbns).await.unwrap(), results);
    }

    #[tokio::test]
    async fn failed_later_page_caches_neither_positive_nor_negative_results() {
        use axum::{extract::Query, http::StatusCode, routing::get, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/",
                    get(|Query(q): Query<std::collections::HashMap<String, String>>| async move {
                        if q["startRecord"] == "1" {
                            (StatusCode::OK, MARC_RESPONSE.replace("<numberOfRecords>1", "<numberOfRecords>2"))
                        } else {
                            (StatusCode::INTERNAL_SERVER_ERROR, "error".into())
                        }
                    }),
                ),
            )
            .await
            .unwrap()
        });
        let dir = tempfile::tempdir().unwrap();
        let client = LibraryOfCongressClient::with_endpoint(&endpoint).unwrap().with_cache(&dir.path().join("cache")).unwrap();
        let isbns = vec!["9780306406157".into(), "9781691706631".into()];
        assert!(client.lookup_batch(&isbns).await.is_err());
        for isbn in isbns {
            assert_eq!(client.cached(&isbn).unwrap(), None);
            assert!(!client.retry_ready(&isbn).unwrap());
        }
        task.abort();
    }

    #[test]
    fn two_authority_records_for_one_isbn_are_ambiguous() {
        let records = parse_sru_response(MARC_RESPONSE.as_bytes()).unwrap();
        let mut duplicate = records[0].clone();
        duplicate.provider_record_id = Some("other".into());
        assert_eq!(match_isbn("9780306406157", &[records[0].clone(), duplicate]), LibraryOfCongressLookup::Ambiguous);
    }
}
