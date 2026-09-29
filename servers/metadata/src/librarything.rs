//! LibraryThing's documented Common Knowledge API, with a persistent ISBN cache.
//! Active canonical LCC, DDC and BISAC facts are retained when supplied by the API.
use crate::{error, MetadataError};
use futures_util::StreamExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ENDPOINT: &str = "https://www.librarything.com/services/rest/1.1/";
const MAX_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct LibraryThingClient {
    client: reqwest::Client,
    key: Arc<String>,
    endpoint: String,
    pub(super) cache: Arc<Mutex<Connection>>,
    pub(super) cache_path: Arc<std::path::PathBuf>,
    requests: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Lookup {
    pub work_id: String,
    /// Legacy field name: these are LCC codes only.
    pub codes: Vec<String>,
    #[serde(default)]
    pub ddc: Vec<String>,
    #[serde(default)]
    pub bisac: Vec<String>,
    #[serde(default)]
    pub all_schemes_checked: bool,
}

impl LibraryThingClient {
    pub(crate) fn open(key_file: &Path, cache_file: &Path) -> Result<Self, MetadataError> {
        let key = std::fs::read_to_string(key_file).map_err(|_| MetadataError("could not read LibraryThing key file".into()))?;
        if key.trim().is_empty() {
            return Err(MetadataError("LibraryThing key is empty".into()));
        }
        let db = Connection::open(cache_file).map_err(error)?;
        db.busy_timeout(Duration::from_secs(1)).map_err(error)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS librarything_isbn_lcc (isbn13 TEXT PRIMARY KEY, result TEXT NOT NULL, checked_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS librarything_requests (id INTEGER PRIMARY KEY CHECK(id=1), day INTEGER NOT NULL, count INTEGER NOT NULL, last_at INTEGER NOT NULL);",
        )
        .map_err(error)?;
        let client = reqwest::Client::builder().user_agent("BokheimMetadata/1.0").redirect(reqwest::redirect::Policy::none()).connect_timeout(Duration::from_secs(5)).timeout(Duration::from_secs(15)).build().map_err(error)?;
        Ok(Self { client, key: Arc::new(key.trim().into()), endpoint: ENDPOINT.into(), cache: Arc::new(Mutex::new(db)), cache_path: Arc::new(cache_file.to_owned()), requests: Arc::new(tokio::sync::Mutex::new(())) })
    }

    pub(super) fn cached(&self, isbn: &str) -> Result<Option<Lookup>, MetadataError> {
        let db = self.cache.lock().map_err(|_| MetadataError("LibraryThing cache lock failed".into()))?;
        let row: Option<String> = db.query_row("SELECT result FROM librarything_isbn_lcc WHERE isbn13=?1", [isbn], |r| r.get(0)).optional().map_err(error)?;
        let Some(json) = row else { return Ok(None) };
        let result: Lookup = serde_json::from_str(&json).map_err(error)?;
        // A stored result marks this ISBN as tried, including a work without LCC or no work.
        // checked_at is audit information only; completed lookups never expire.
        Ok(Some(result))
    }

    fn save(&self, isbn: &str, result: &Lookup, now: i64) -> Result<(), MetadataError> {
        self.cache
            .lock()
            .map_err(|_| MetadataError("LibraryThing cache lock failed".into()))?
            .execute("INSERT OR REPLACE INTO librarything_isbn_lcc VALUES (?1,?2,?3)", params![isbn, serde_json::to_string(result).map_err(error)?, now])
            .map_err(error)?;
        Ok(())
    }

    // Persistent quota and spacing survive restarts. Reserve before sending, including failures.
    #[cfg(test)]
    fn reserve(&self, now: i64) -> Result<(), MetadataError> {
        self.reserve_for(now, false)
    }

    fn reserve_for(&self, now: i64, background: bool) -> Result<(), MetadataError> {
        let mut db = self.cache.lock().map_err(|_| MetadataError("LibraryThing cache lock failed".into()))?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(error)?;
        let (day, count, last): (i64, i64, i64) = tx.query_row("SELECT day,count,last_at FROM librarything_requests WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(error)?.unwrap_or((-1, 0, 0));
        if now - last < 2 {
            return Err(MetadataError("LibraryThing request spacing limit".into()));
        }
        let count = if day == now / 86400 { count } else { 0 };
        if background && count >= 950 {
            return Err(MetadataError("LibraryThing background daily budget exhausted; 50 requests reserved for fallback".into()));
        }
        if count >= 1000 {
            return Err(MetadataError("LibraryThing daily request limit reached".into()));
        }
        tx.execute("INSERT OR REPLACE INTO librarything_requests VALUES (1,?1,?2,?3)", params![now / 86400, count + 1, now]).map_err(error)?;
        tx.commit().map_err(error)
    }

    pub(crate) async fn lookup_isbn(&self, isbn: &str) -> Result<Lookup, MetadataError> {
        self.lookup(isbn, false, false).await
    }

    // Explicit one-time upgrade only; ordinary lookups still never expire.
    pub(crate) async fn refresh_schemes(&self, isbn: &str) -> Result<Lookup, MetadataError> {
        self.lookup(isbn, true, false).await
    }

    pub(super) async fn lookup_background(&self, isbn: &str) -> Result<Lookup, MetadataError> {
        self.lookup(isbn, true, true).await
    }

    async fn lookup(&self, isbn: &str, refresh: bool, background: bool) -> Result<Lookup, MetadataError> {
        let isbn = crate::canonical_isbn13(isbn).ok_or_else(|| MetadataError("LibraryThing requires a valid ISBN".into()))?;
        let isbn = format!("{isbn:013}");
        if let Some(result) = self.cached(&isbn)? {
            if !refresh || result.all_schemes_checked || result.work_id.is_empty() || !result.codes.is_empty() {
                return Ok(result);
            }
        }
        let _guard = self.requests.lock().await;
        if let Some(result) = self.cached(&isbn)? {
            if !refresh || result.all_schemes_checked || result.work_id.is_empty() || !result.codes.is_empty() {
                return Ok(result);
            }
        }
        // Two integer seconds guarantee at least one wall-clock second between requests.
        tokio::time::sleep(Duration::from_secs(2)).await;
        self.reserve_for(now(), background)?;
        let response = self.client.get(&self.endpoint).query(&[("method", "librarything.ck.getwork"), ("isbn", isbn.as_str()), ("apikey", self.key.as_str())]).send().await.map_err(|_| MetadataError("LibraryThing request failed".into()))?;
        if !response.status().is_success() {
            return Err(MetadataError(format!("LibraryThing HTTP {}", response.status().as_u16())));
        }
        if response.content_length().is_some_and(|size| size > MAX_BYTES as u64) {
            return Err(MetadataError("LibraryThing response too large".into()));
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| MetadataError("LibraryThing response failed".into()))?;
            if bytes.len() + chunk.len() > MAX_BYTES {
                return Err(MetadataError("LibraryThing response too large".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        let result = parse(&bytes)?;
        self.save(&isbn, &result, now())?;
        Ok(result)
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

fn parse(bytes: &[u8]) -> Result<Lookup, MetadataError> {
    let xml = std::str::from_utf8(bytes).map_err(|_| MetadataError("LibraryThing response is not UTF-8".into()))?;
    let doc = roxmltree::Document::parse(xml).map_err(|_| MetadataError("invalid LibraryThing XML".into()))?;
    let root = doc.root_element();
    if root.tag_name().name() == "response"
        && root.attribute("stat") == Some("fail")
        && root.children().any(|n| n.tag_name().name() == "err" && n.attribute("code") == Some("105") && n.text() == Some("Could not determine data ID to retrieve"))
    {
        // Verified response to a valid ISBN with no LibraryThing work.
        return Ok(Lookup { work_id: String::new(), codes: vec![], ddc: vec![], bisac: vec![], all_schemes_checked: true });
    }
    if root.tag_name().name() != "response" || root.attribute("stat") != Some("ok") {
        return Err(MetadataError("LibraryThing API did not report success".into()));
    }
    let items: Vec<_> = doc.descendants().filter(|n| n.has_tag_name(("http://www.librarything.com/", "item"))).collect();
    // Unexpected success envelopes are not authoritative empty lookups.
    if items.len() != 1 || items[0].attribute("type") != Some("work") {
        return Err(MetadataError("LibraryThing response has no unique work".into()));
    }
    let item = items[0];
    let id = item.attribute("id").filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_digit())).ok_or_else(|| MetadataError("invalid LibraryThing work ID".into()))?;
    let mut codes = BTreeSet::new();
    let mut ddc = BTreeSet::new();
    let mut bisac = BTreeSet::new();
    for field in item.descendants().filter(|n| n.tag_name().name() == "field") {
        let name = field.attribute("name").unwrap_or_default();
        if !matches!(name, "canonicallcc" | "canonicalddc" | "canonicaldewey" | "canonicalbisac") {
            continue;
        }
        for version in field.descendants().filter(|n| n.tag_name().name() == "version" && n.attribute("archived") == Some("0")) {
            for fact in version.descendants().filter(|n| n.tag_name().name() == "fact") {
                let raw = fact.text().unwrap_or_default().trim();
                if raw.is_empty() {
                    continue;
                }
                use metadata_contract::{classification_consensus_notation, ClassificationScheme};
                match name {
                    "canonicallcc" => {
                        codes.insert(subject_projection::canonical_lcc_notation(raw).ok_or_else(|| MetadataError("LibraryThing returned invalid LCC".into()))?);
                    }
                    "canonicalddc" | "canonicaldewey" => {
                        ddc.insert(classification_consensus_notation(ClassificationScheme::DeweyDecimal, raw).ok_or_else(|| MetadataError("LibraryThing returned invalid DDC".into()))?);
                    }
                    "canonicalbisac" => {
                        bisac.insert(classification_consensus_notation(ClassificationScheme::Bisac, raw).ok_or_else(|| MetadataError("LibraryThing returned invalid BISAC".into()))?);
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
    Ok(Lookup { work_id: id.into(), codes: codes.into_iter().collect(), ddc: ddc.into_iter().collect(), bisac: bisac.into_iter().collect(), all_schemes_checked: true })
}

pub(crate) async fn apply(service: &crate::MetadataService, client: &LibraryThingClient, response: &mut crate::RichMetadataEnrichmentResponse) {
    use metadata_contract::{Classification, ClassificationEvidence, ClassificationScheme, ClassificationSource, LookupStatus, RichWorkMetadataMatch};
    for result in &mut response.results {
        if result.status == LookupStatus::InvalidIsbn || result.matches.len() > 1 || result.matches.iter().any(|m| !m.classifications.is_empty()) {
            continue;
        }
        let Some(id) = crate::canonical_isbn13(&result.requested_isbn) else { continue };
        let isbn = id.to_string();
        match client.cached(&isbn) {
            Ok(Some(_)) => {} // Permanent results may be reused without a new LC request.
            Ok(None) => {
                let query = isbn.clone();
                if !matches!(crate::query::run_database(service.clone(), move |s| s.librarything_network_eligible(&query)).await, Ok(Ok(true))) {
                    continue;
                }
            }
            Err(e) => {
                tracing::warn!(%e,"LibraryThing cache unavailable");
                continue;
            }
        }
        let found = match client.lookup_isbn(&isbn).await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(isbn = %result.requested_isbn, %error, "LibraryThing fallback unavailable");
                continue;
            }
        };
        if found.codes.is_empty() && found.ddc.is_empty() && found.bisac.is_empty() {
            continue;
        }
        let isbn = format!("{:013}", crate::canonical_isbn13(&result.requested_isbn).expect("validated ISBN"));
        if result.matches.is_empty() {
            result.matches.push(RichWorkMetadataMatch { open_library_work_id: None, exact_edition_ids: vec![], classifications: vec![], authors: vec![], description: None, subjects: vec![] });
        }
        let classifications = found
            .codes
            .into_iter()
            .map(|n| (ClassificationScheme::LibraryOfCongress, n))
            .chain(found.ddc.into_iter().map(|n| (ClassificationScheme::DeweyDecimal, n)))
            .chain(found.bisac.into_iter().map(|n| (ClassificationScheme::Bisac, n)));
        result.matches[0].classifications.extend(classifications.map(|(scheme, notation)| Classification {
            scheme,
            notation,
            source: ClassificationSource::Work,
            evidence: vec![ClassificationEvidence { method: format!("librarything_isbn:{}", found.work_id), isbn13: isbn.clone(), open_library_work_id: String::new(), open_library_edition_id: String::new() }],
        }));
        result.canonical_isbn13 = Some(isbn);
        if result.status == LookupStatus::NoMatch {
            result.status = LookupStatus::Matched;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response(fields: &str) -> String {
        format!(r#"<response stat="ok"><ltml xmlns="http://www.librarything.com/"><item id="22550208" type="work"><commonknowledge><fieldList>{fields}</fieldList></commonknowledge></item></ltml></response>"#)
    }
    #[test]
    fn active_lcc_only_and_failures_are_not_misses() {
        let xml = response(
            r#"<field name="canonicallcc"><version archived="1"><fact>DC161</fact></version><version archived="0"><fact>PS3608.O623</fact></version></field><field name="canonicalddc"><version archived="0"><fact>813.6</fact></version></field>"#,
        );
        let result = parse(xml.as_bytes()).unwrap();
        assert_eq!(result.codes, vec!["PS3608.O623"]);
        assert_eq!(result.ddc, vec!["813.6"]);
        assert!(parse(response("").as_bytes()).unwrap().codes.is_empty());
        for xml in ["<html/>", "<response stat='fail'/>", "<response stat='ok'/>", "invalid"] {
            assert!(parse(xml.as_bytes()).is_err());
        }
        assert!(parse(response(r#"<field name="canonicallcc"><version archived="0"><fact>garbage</fact></version></field>"#).as_bytes()).is_err());
    }
    #[test]
    fn schemes_are_validated_and_legacy_results_are_not_mislabelled() {
        let old: Lookup = serde_json::from_str(r#"{"work_id":"1","codes":[]}"#).unwrap();
        assert!(!old.all_schemes_checked);
        let xml = response(
            r#"<field name="canonicalddc"><version archived="0"><fact>005/.133</fact><fact>005.133</fact></version></field><field name="canonicalbisac"><version archived="1"><fact>BAD</fact></version><version archived="0"><fact>com051010</fact></version></field>"#,
        );
        let found = parse(xml.as_bytes()).unwrap();
        assert_eq!(found.ddc, ["005.133"]);
        assert_eq!(found.bisac, ["COM051010"]);
        assert!(found.codes.is_empty());
        assert!(found.all_schemes_checked);
        for (field, value) in [("canonicalddc", "813 fiction"), ("canonicalbisac", "Fiction"), ("canonicalbisac", "ABé12345"), ("canonicalddc", "12")] {
            assert!(parse(response(&format!("<field name='{field}'><version archived='0'><fact>{value}</fact></version></field>")).as_bytes()).is_err());
        }
    }

    #[tokio::test]
    async fn refresh_is_one_time_and_failure_preserves_legacy_cache() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("key");
        std::fs::write(&key, "test-key").unwrap();
        let client = LibraryThingClient::open(&key, &dir.path().join("cache.sqlite")).unwrap();
        let old: Lookup = serde_json::from_str(r#"{"work_id":"1","codes":[]}"#).unwrap();
        client.save("9781691706631", &old, 0).unwrap();
        client.reserve(now()).unwrap();
        client.cache.lock().unwrap().execute("UPDATE librarything_requests SET count=1000", []).unwrap();
        assert_eq!(client.lookup_isbn("9781691706631").await.unwrap(), old);
        assert!(client.refresh_schemes("9781691706631").await.is_err());
        assert_eq!(client.cached("9781691706631").unwrap(), Some(old));
        let upgraded = parse(response(r#"<field name="canonicalddc"><version archived="0"><fact>813.6</fact></version></field>"#).as_bytes()).unwrap();
        client.save("9781691706631", &upgraded, now()).unwrap();
        // Even with exhausted quota, a completed upgrade never requests again.
        assert_eq!(client.refresh_schemes("9781691706631").await.unwrap(), upgraded);
    }

    #[test]
    fn background_reserve_survives_reopen_and_leaves_fifty_foreground_requests() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("key");
        std::fs::write(&key, "test").unwrap();
        let path = dir.path().join("cache.sqlite");
        let client = LibraryThingClient::open(&key, &path).unwrap();
        client.reserve_for(100, true).unwrap();
        client.cache.lock().unwrap().execute("UPDATE librarything_requests SET count=949", []).unwrap();
        client.reserve_for(102, true).unwrap();
        drop(client);
        let client = LibraryThingClient::open(&key, &path).unwrap();
        assert!(client.reserve_for(104, true).unwrap_err().to_string().contains("50 requests reserved"));
        for i in 0..50 {
            client.reserve_for(104 + i * 2, false).unwrap();
        }
        assert!(client.reserve_for(204, false).is_err());
        assert_eq!(client.cache.lock().unwrap().query_row("SELECT count FROM librarything_requests", [], |r| r.get::<_, i64>(0)).unwrap(), 1000);
        client.reserve_for(86405, true).unwrap();
    }

    #[test]
    fn persistent_positive_negative_cache_and_quota() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("key");
        std::fs::write(&key, "test-key").unwrap();
        let db = dir.path().join("cache.sqlite");
        let client = LibraryThingClient::open(&key, &db).unwrap();
        let positive = Lookup { work_id: "1".into(), codes: vec!["DC161".into()], ddc: vec![], bisac: vec![], all_schemes_checked: true };
        let negative = Lookup { work_id: "2".into(), codes: vec![], ddc: vec![], bisac: vec![], all_schemes_checked: true };
        client.save("9781538724736", &positive, 10).unwrap();
        client.save("9781691706631", &negative, 10).unwrap();
        client.reserve(100).unwrap();
        drop(client);
        let client = LibraryThingClient::open(&key, &db).unwrap();
        assert_eq!(client.cached("9781538724736").unwrap(), Some(positive));
        assert_eq!(client.cached("9781691706631").unwrap(), Some(negative));
        assert!(client.reserve(101).is_err());
        client.reserve(102).unwrap();
        client.cache.lock().unwrap().execute("UPDATE librarything_requests SET count=1000", []).unwrap();
        assert!(client.reserve(104).is_err());
        client.reserve(86400).unwrap();
    }
    #[tokio::test]
    async fn network_cache_survives_reopen_and_errors_remain_retryable() {
        use axum::{extract::Query, routing::get, Router};
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let app = Router::new().route(
            "/",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(q.get("apikey").unwrap(), "test-key");
                    assert_eq!(q.get("method").unwrap(), "librarything.ck.getwork");
                    match q.get("isbn").unwrap().as_str() {
                        "9781538724736" => response(r#"<field name="canonicallcc"><version archived="0"><fact>PS3608.O623</fact></version></field>"#),
                        "9781691706631" => response(""),
                        "9798897248889" => r#"<response stat="fail"><err code="105">Could not determine data ID to retrieve</err></response>"#.into(),
                        _ => "<response stat='fail'><err code='1'>Invalid key</err></response>".into(),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("key");
        std::fs::write(&key, "test-key").unwrap();
        let db = dir.path().join("cache.sqlite");
        let mut client = LibraryThingClient::open(&key, &db).unwrap();
        client.endpoint = endpoint.clone();
        assert!(!client.lookup_isbn("1538724731").await.unwrap().codes.is_empty());
        assert!(client.lookup_isbn("9781691706631").await.unwrap().codes.is_empty());
        assert!(client.lookup_isbn("9798897248889").await.unwrap().work_id.is_empty());
        // Age all completed results well past the old expiry before reopening.
        client.cache.lock().unwrap().execute("UPDATE librarything_isbn_lcc SET checked_at=0", []).unwrap();
        drop(client);
        let mut client = LibraryThingClient::open(&key, &db).unwrap();
        client.endpoint = endpoint;
        for isbn in ["9781538724736", "9781691706631", "9798897248889"] {
            client.lookup_isbn(isbn).await.unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        for _ in 0..2 {
            assert!(client.lookup_isbn("9780131103627").await.is_err());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        assert!(client.cached("9780131103627").unwrap().is_none());
        task.abort();
    }

    #[tokio::test]
    async fn applies_cached_codes_preserves_existing_metadata_and_skips_ambiguity() {
        use metadata_contract::*;
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("key");
        std::fs::write(&key, "test-key").unwrap();
        let client = LibraryThingClient::open(&key, &dir.path().join("cache.sqlite")).unwrap();
        client.save("9781538724736", &Lookup { work_id: "22550208".into(), codes: vec!["PS3608.O623".into()], ddc: vec!["813.6".into()], bisac: vec!["FIC000000".into()], all_schemes_checked: true }, now()).unwrap();
        let matched = RichWorkMetadataMatch { open_library_work_id: Some("OL20068530W".into()), exact_edition_ids: vec![], classifications: vec![], authors: vec![], description: Some("existing description".into()), subjects: vec![] };
        let mut result = RichIsbnMetadataResult { wikidata_books: Vec::new(), requested_isbn: "9781538724736".into(), canonical_isbn13: None, status: LookupStatus::Matched, matches: vec![matched.clone()] };
        let mut existing = result.clone();
        existing.matches[0].classifications.push(Classification { scheme: ClassificationScheme::LibraryOfCongress, notation: "PS3608".into(), source: ClassificationSource::ExactEdition, evidence: vec![] });
        let mut ambiguous = result.clone();
        ambiguous.matches.push(matched);
        result.status = LookupStatus::NoMatch;
        result.matches.clear();
        let mut response = RichMetadataEnrichmentResponse {
            snapshot: SnapshotDescription { dump_date: "test".into(), imported_at_ms: 0 },
            results: vec![RichIsbnMetadataResult { wikidata_books: Vec::new(), requested_isbn: "9781538724736".into(), canonical_isbn13: None, status: LookupStatus::Matched, matches: vec![existing.matches[0].clone()] }, ambiguous, result],
        };
        let service = crate::MetadataService::open(crate::tests::snapshot_from(dir.path(), &[], &[], &[])).unwrap();
        apply(&service, &client, &mut response).await;
        assert_eq!(response.results[0].matches[0].classifications[0].notation, "PS3608");
        assert_eq!(response.results[0].matches[0].description.as_deref(), Some("existing description"));
        assert!(response.results[1].matches[0].classifications.is_empty());
        let added = &response.results[2];
        assert_eq!(added.status, LookupStatus::Matched);
        assert_eq!(added.matches[0].classifications.len(), 3);
        assert_eq!(added.matches[0].classifications[1].scheme, ClassificationScheme::DeweyDecimal);
        assert_eq!(added.matches[0].classifications[2].scheme, ClassificationScheme::Bisac);
        assert!(added.matches[0].open_library_work_id.is_none());
        assert_eq!(added.matches[0].classifications[0].evidence[0].method, "librarything_isbn:22550208");
    }

    #[tokio::test]
    async fn background_worker_stops_at_reserve_but_foreground_can_continue() {
        use axum::{routing::get, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, Router::new().route("/", get(|| async { response(r#"<field name="canonicallcc"><version archived="0"><fact>PS3608</fact></version></field>"#) }))).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let database = crate::tests::snapshot_from(
            dir.path(),
            &[
                serde_json::json!({"key":"/books/OL1M","works":[{"key":"/works/OL10W"}],"title":"Unique background one","isbn_13":["9781538724736"]}),
                serde_json::json!({"key":"/books/OL2M","works":[{"key":"/works/OL20W"}],"title":"Unique background two","isbn_13":["9781691706631"]}),
            ],
            &[],
            &[],
        );
        let key = dir.path().join("key");
        std::fs::write(&key, "test").unwrap();
        let mut client = LibraryThingClient::open(&key, &dir.path().join("cache.sqlite")).unwrap();
        client.endpoint = endpoint;
        client.cache.lock().unwrap().execute("INSERT INTO librarything_requests VALUES(1,?,949,?)", params![now() / 86400, now() - 4]).unwrap();
        let service = crate::MetadataService::open(database).unwrap();
        let worker = tokio::spawn(crate::librarything_background::run(service, client.clone(), None));
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if client.cached("9781538724736").unwrap().is_some() {
                    let c = client.cache.lock().unwrap();
                    let state = c.query_row("SELECT value FROM librarything_background_state WHERE key='status'", [], |r| r.get::<_, String>(0)).optional().unwrap();
                    if state.as_deref() == Some("daily_budget_reserved") {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(client.cached("9781691706631").unwrap().is_none());
        assert!(!client.lookup_isbn("9781691706631").await.unwrap().codes.is_empty());
        assert_eq!(client.cache.lock().unwrap().query_row("SELECT count FROM librarything_requests", [], |r| r.get::<_, i64>(0)).unwrap(), 951);
        worker.abort();
        server.abort();
    }

    #[tokio::test]
    #[ignore = "requires BOKHEIM_LIBRARYTHING_KEY_FILE and makes three live API calls"]
    async fn live_api_and_persistent_cache() {
        let key = std::env::var_os("BOKHEIM_LIBRARYTHING_KEY_FILE").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cache.sqlite");
        let client = LibraryThingClient::open(Path::new(&key), &db).unwrap();
        let positive = client.lookup_isbn("9781538724736").await.unwrap();
        assert_eq!(positive.work_id, "22550208");
        assert!(positive.codes.iter().any(|s| s.starts_with("PS3608")));
        assert!(client.lookup_isbn("9781691706631").await.unwrap().codes.is_empty());
        assert!(client.lookup_isbn("9798897248889").await.unwrap().work_id.is_empty());
        drop(client);
        let client = LibraryThingClient::open(Path::new(&key), &db).unwrap();
        assert_eq!(client.lookup_isbn("9781538724736").await.unwrap(), positive);
        assert!(client.lookup_isbn("9781691706631").await.unwrap().codes.is_empty());
        assert!(client.lookup_isbn("9798897248889").await.unwrap().work_id.is_empty());
        let count: i64 = client.cache.lock().unwrap().query_row("SELECT count FROM librarything_requests", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn workers_handoff_lc_miss_before_librarything_with_separate_caches() {
        use axum::{routing::get, Router};
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let lc_events = events.clone();
        let lt_events = events.clone();
        let app = Router::new()
            .route(
                "/lc",
                get(move || {
                    lc_events.lock().unwrap().push("lc");
                    async { "<searchRetrieveResponse xmlns='http://www.loc.gov/zing/srw/'><numberOfRecords>0</numberOfRecords><records/></searchRetrieveResponse>" }
                }),
            )
            .route(
                "/lt",
                get(move || {
                    lt_events.lock().unwrap().push("lt");
                    async { response(r#"<field name="canonicalddc"><version archived="0"><fact>813.6</fact></version></field>"#) }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("key");
        std::fs::write(&key, "test-key").unwrap();
        let path = crate::tests::snapshot_from(dir.path(), &[serde_json::json!({"key":"/books/OL1M","works":[{"key":"/works/OL10W"}],"title":"Old book","publish_date":"1995","isbn_13":["9781538724736"]})], &[], &[]);
        let mut service = crate::MetadataService::open(path).unwrap();
        let mut lt = LibraryThingClient::open(&key, &dir.path().join("lt.sqlite")).unwrap();
        lt.endpoint = format!("http://{address}/lt");
        let lc_path = dir.path().join("lc.sqlite");
        let lc = crate::library_of_congress::LibraryOfCongressClient::with_endpoint(&format!("http://{address}/lc")).unwrap().with_cache(&lc_path).unwrap();
        service.librarything = Some(lt.clone());
        service.library_of_congress = Some(lc.clone());
        let lt_worker = tokio::spawn(crate::librarything_background::run(service.clone(), lt.clone(), None));
        let lc_worker = tokio::spawn(crate::loc_background::run(service.clone(), lc, lc_path));
        let done = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if lt.cached("9781538724736").unwrap().is_some_and(|v| !v.ddc.is_empty()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        lt_worker.abort();
        lc_worker.abort();
        server.abort();
        done.unwrap();
        assert_eq!(*events.lock().unwrap(), vec!["lc", "lt"]);
        assert_eq!(lt.cached("9781538724736").unwrap().unwrap().ddc, vec!["813.6"]);
        // The new result is served from disk without either provider being called again.
        let mut response = service.enrich_rich(crate::MetadataEnrichmentRequest { isbns: vec!["9781538724736".into()] }).unwrap();
        service.apply_library_of_congress_fallback(&mut response).await;
        service.apply_librarything_fallback(&mut response).await;
        assert_eq!(response.results[0].matches[0].classifications[0].notation, "813.6");
    }
}
