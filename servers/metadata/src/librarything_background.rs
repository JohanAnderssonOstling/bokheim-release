//! Resumable, low-priority enrichment using the same cache and quota as HTTP fallback.
use crate::{error, librarything::LibraryThingClient, MetadataError, MetadataService};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}
fn next_day(now: i64) -> i64 {
    (now / 86400 + 1) * 86400 + 5
}
fn db(client: &LibraryThingClient) -> Result<Connection, MetadataError> {
    let c = Connection::open(client.cache_path.as_ref()).map_err(error)?;
    c.busy_timeout(Duration::from_secs(2)).map_err(error)?;
    Ok(c)
}
fn initialize(c: &Connection) -> Result<(), MetadataError> {
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS librarything_background_work (
        work_id INTEGER PRIMARY KEY, ratings INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL DEFAULT 'pending', retry_at INTEGER NOT NULL DEFAULT 0,
        checked_at INTEGER NOT NULL DEFAULT 0, last_error TEXT);
        CREATE INDEX IF NOT EXISTS librarything_background_pending ON librarything_background_work(status,ratings DESC,work_id);
        CREATE TABLE IF NOT EXISTS librarything_background_state (key TEXT PRIMARY KEY,value TEXT NOT NULL);",
    )
    .map_err(error)
}
fn state(c: &Connection, key: &str) -> Result<Option<String>, MetadataError> {
    c.query_row("SELECT value FROM librarything_background_state WHERE key=?", [key], |r| r.get(0)).optional().map_err(error)
}
fn set(c: &Connection, key: &str, value: impl ToString) -> Result<(), MetadataError> {
    c.execute("INSERT OR REPLACE INTO librarything_background_state VALUES (?,?)", params![key, value.to_string()]).map_err(error)?;
    Ok(())
}
fn finish(c: &Connection, work: i64, status: &str, retry_at: i64, failure: Option<&str>) -> Result<(), MetadataError> {
    c.execute("UPDATE librarything_background_work SET status=?,retry_at=?,checked_at=?,last_error=? WHERE work_id=?", params![status, retry_at, now(), failure, work]).map_err(error)?;
    Ok(())
}
pub(crate) fn has_codes(r: &crate::librarything::Lookup) -> bool {
    !r.codes.is_empty() || !r.ddc.is_empty() || !r.bisac.is_empty()
}

// LC owns admission to the second provider. Preserve completed LibraryThing
// attempts and any existing retry delay, including when caches are separate files.
pub(crate) fn enqueue_after_lc(client: &LibraryThingClient, work: i64, ratings: i64) -> Result<(), MetadataError> {
    let c = db(client)?;
    initialize(&c)?;
    c.execute(
        "INSERT INTO librarything_background_work(work_id,ratings,status) VALUES (?,?,'ready_after_lc')
        ON CONFLICT(work_id) DO UPDATE SET status='ready_after_lc',ratings=MAX(ratings,excluded.ratings)
        WHERE status IN ('pending','awaiting_lc','ready_after_lc')",
        params![work, ratings],
    )
    .map_err(error)?;
    Ok(())
}
#[derive(Debug, PartialEq)]
pub(crate) enum LcGate {
    Ready,
    Pending,
    HasCode,
}
pub(crate) fn lc_gate(service: &MetadataService, isbns: &[String]) -> Result<LcGate, MetadataError> {
    let Some(loc) = &service.library_of_congress else { return Ok(LcGate::Ready) };
    let mut pending = false;
    for isbn in isbns {
        match loc.cached(isbn)? {
            Some(crate::library_of_congress::LibraryOfCongressLookup::Matched { classifications, .. }) if !classifications.is_empty() => return Ok(LcGate::HasCode),
            None => pending = true,
            _ => {}
        }
    }
    Ok(if pending { LcGate::Pending } else { LcGate::Ready })
}

fn seed(service: &MetadataService, client: &LibraryThingClient, ratings: Option<&Path>) -> Result<(), MetadataError> {
    let mut c = db(client)?;
    initialize(&c)?;
    let signature = format!("{}:{}:{}", service.state.snapshot.dump_date, service.state.snapshot.imported_at_ms, std::fs::canonicalize(&service.state.database).map_err(error)?.display());
    if state(&c, "snapshot")?.as_deref() != Some(signature.as_str()) {
        let tx = c.transaction().map_err(error)?;
        tx.execute("UPDATE librarything_background_work SET status='pending',retry_at=0 WHERE status IN ('local','no_isbn','too_many_isbns')", []).map_err(error)?;
        set(&tx, "snapshot", &signature)?;
        set(&tx, "cursor", 0)?;
        tx.commit().map_err(error)?;
    }
    let key = if let Some(path) = ratings {
        let m = std::fs::metadata(path).map_err(error)?;
        format!("{}:{}:{:?}", path.display(), m.len(), m.modified().map_err(error)?)
    } else {
        signature
    };
    if state(&c, "rank_seed")?.as_deref() == Some(key.as_str()) {
        return Ok(());
    }
    let mut counts = BTreeMap::<i64, i64>::new();
    if let Some(path) = ratings {
        let reader = BufReader::new(flate2::read::MultiGzDecoder::new(std::fs::File::open(path).map_err(error)?));
        for line in reader.lines() {
            let line = line.map_err(error)?;
            if let Some(id) = line.split('\t').next().and_then(|s| s.strip_prefix("/works/OL")).and_then(|s| s.strip_suffix('W')).and_then(|s| s.parse::<i64>().ok()) {
                *counts.entry(id).or_default() += 1;
            }
        }
    }
    // Small transactions keep the serving cache available during initial ranking import.
    let rows = counts.into_iter().collect::<Vec<_>>();
    for chunk in rows.chunks(1000) {
        let tx = c.transaction().map_err(error)?;
        {
            let mut q = tx.prepare("INSERT INTO librarything_background_work(work_id,ratings) VALUES (?,?) ON CONFLICT(work_id) DO UPDATE SET ratings=excluded.ratings").map_err(error)?;
            for (id, count) in chunk {
                q.execute(params![id, count]).map_err(error)?;
            }
        }
        tx.commit().map_err(error)?;
    }
    set(&c, "rank_seed", key)?;
    tracing::info!(rated_works = rows.len(), "LibraryThing background ranking loaded");
    Ok(())
}

fn next_work(service: &MetadataService, client: &LibraryThingClient) -> Result<Option<i64>, MetadataError> {
    let mut c = db(client)?;
    let status = if service.library_of_congress.is_some() { "ready_after_lc" } else { "pending" };
    let pending =
        |c: &Connection| c.query_row("SELECT work_id FROM librarything_background_work WHERE status=? AND retry_at<=? ORDER BY ratings DESC,work_id LIMIT 1", params![status, now()], |r| r.get::<_, i64>(0)).optional().map_err(error);
    if let Some(id) = pending(&c)? {
        return Ok(Some(id));
    }
    if service.library_of_congress.is_some() {
        return Ok(None);
    }
    // After rated works, advance through every retained work, with a durable keyset cursor.
    let cursor = state(&c, "cursor")?.and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let identity = crate::query::query_connection(&service.state.identity_pool)?;
    let mut q = identity.prepare("SELECT DISTINCT work_id FROM edition WHERE work_id>? ORDER BY work_id LIMIT 256").map_err(error)?;
    let ids = q.query_map([cursor], |r| r.get::<_, i64>(0)).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
    if let Some(last) = ids.last() {
        let tx = c.transaction().map_err(error)?;
        for id in &ids {
            tx.execute("INSERT OR IGNORE INTO librarything_background_work(work_id) VALUES (?)", [id]).map_err(error)?;
        }
        set(&tx, "cursor", last)?;
        tx.commit().map_err(error)?;
    }
    pending(&c)
}

#[derive(Debug, PartialEq)]
pub(crate) enum Eligibility {
    Local,
    NoIsbn,
    TooMany,
    Isbns(Vec<String>),
}
pub(crate) fn eligible(service: &MetadataService, work: i64) -> Result<Eligibility, MetadataError> {
    let identity = crate::query::query_connection(&service.state.identity_pool)?;
    // Raw DDC rows must count too, even where the ordinary LCC lookup ignores them.
    for table in ["work_classification", "edition_work_classification"] {
        if identity.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE work_id=? AND trim(notation)!='')"), [work], |r| r.get::<_, bool>(0)).map_err(error)? {
            return Ok(Eligibility::Local);
        }
    }
    if identity.query_row("SELECT EXISTS(SELECT 1 FROM edition e JOIN edition_classification c USING(edition_id) WHERE e.work_id=? AND trim(c.notation)!='')", [work], |r| r.get::<_, bool>(0)).map_err(error)? {
        return Ok(Eligibility::Local);
    }
    let mut related = std::collections::BTreeSet::from([work]);
    let mut q = identity.prepare("SELECT DISTINCT i.isbn13 FROM edition e JOIN edition_isbn i USING(edition_id) WHERE e.work_id=? ORDER BY i.isbn13 LIMIT 513").map_err(error)?;
    let isbns = q.query_map([work], |r| r.get::<_, i64>(0)).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
    if isbns.is_empty() {
        return Ok(Eligibility::NoIsbn);
    }
    if isbns.len() > 512 {
        return Ok(Eligibility::TooMany);
    }
    // An ISBN may belong to another work: inspect all of those editions and works.
    for isbn in &isbns {
        let sql = "SELECT EXISTS(SELECT 1 FROM edition_isbn i JOIN edition e USING(edition_id) WHERE i.isbn13=? AND (
            EXISTS(SELECT 1 FROM edition sibling JOIN edition_classification c USING(edition_id) WHERE sibling.work_id=e.work_id AND trim(c.notation)!='') OR
            EXISTS(SELECT 1 FROM work_classification c WHERE c.work_id=e.work_id AND trim(c.notation)!='') OR
            EXISTS(SELECT 1 FROM edition_work_classification c WHERE c.work_id=e.work_id AND trim(c.notation)!='')))";
        if identity.query_row(sql, [isbn], |r| r.get::<_, bool>(0)).map_err(error)? {
            return Ok(Eligibility::Local);
        }
    }
    for isbn in &isbns {
        let mut q = identity.prepare("SELECT DISTINCT e.work_id FROM edition_isbn i JOIN edition e USING(edition_id) WHERE i.isbn13=? AND e.work_id IS NOT NULL").map_err(error)?;
        for id in q.query_map([isbn], |r| r.get::<_, i64>(0)).map_err(error)? {
            related.insert(id.map_err(error)?);
        }
    }
    drop(q);
    drop(identity);
    let rich = crate::query::query_connection(&service.state.rich_pool)?;
    if rich.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='work_bisac_subject')", [], |r| r.get::<_, bool>(0)).map_err(error)? {
        for id in related {
            if rich.query_row("SELECT EXISTS(SELECT 1 FROM work_bisac_subject WHERE work_id=? AND trim(code)!='')", [id], |r| r.get::<_, bool>(0)).map_err(error)? {
                return Ok(Eligibility::Local);
            }
        }
    }
    drop(rich);
    // LC supplement, classification index, and normal duplicate-work recovery.
    // A failed precheck is retryable; it never authorizes a provider request.
    let isbns = isbns.into_iter().map(|i| i.to_string()).collect::<Vec<_>>();
    for isbn in &isbns {
        let local = service.lookup(metadata_contract::ClassificationRequest { isbns: vec![isbn.clone()] })?;
        if local.results.iter().any(|r| r.matches.iter().any(|m| !m.classifications.is_empty())) {
            return Ok(Eligibility::Local);
        }
    }
    Ok(Eligibility::Isbns(isbns))
}

async fn process(service: &MetadataService, client: &LibraryThingClient, work: i64) -> Result<&'static str, MetadataError> {
    let eligibility = crate::query::run_database(service.clone(), move |s| eligible(s, work)).await.map_err(error)??;
    let isbns = match eligibility {
        Eligibility::Local => return Ok("local"),
        Eligibility::NoIsbn => return Ok("no_isbn"),
        Eligibility::TooMany => return Ok("too_many_isbns"),
        Eligibility::Isbns(v) => v,
    };
    match lc_gate(service, &isbns)? {
        LcGate::HasCode => return Ok("cached_codes"),
        LcGate::Pending => return Ok("awaiting_lc"),
        LcGate::Ready => {}
    }
    // Check every cached ISBN before allowing a new request on another edition.
    let mut completed_work = false;
    for isbn in &isbns {
        if let Some(found) = client.cached(isbn)? {
            if has_codes(&found) {
                return Ok("cached_codes");
            }
            completed_work |= !found.work_id.is_empty() && found.all_schemes_checked;
        }
    }
    if completed_work {
        return Ok("api_without_codes");
    }
    for isbn in isbns {
        let found = client.lookup_background(&isbn).await?;
        if !found.work_id.is_empty() {
            return Ok(if has_codes(&found) { "codes_found" } else { "api_without_codes" });
        }
    }
    Ok("no_match")
}

pub(crate) async fn run(service: MetadataService, client: LibraryThingClient, ratings: Option<PathBuf>) {
    let mut initialized = false;
    let mut failures = 0u32;
    loop {
        let step: Result<Duration, MetadataError> = async {
            if !initialized {
                let s = service.clone();
                let c = client.clone();
                let r = ratings.clone();
                tokio::task::spawn_blocking(move || seed(&s, &c, r.as_deref())).await.map_err(error)??;
                initialized = true;
            }
            let c = db(&client)?;
            let t = now();
            let (day, count) = c.query_row("SELECT day,count FROM librarything_requests WHERE id=1", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).optional().map_err(error)?.unwrap_or((0, 0));
            let pause = state(&c, "pause_until")?.and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            if (day == t / 86400 && count >= 950) || pause > t {
                set(&c, "status", if pause > t { "provider_backoff" } else { "daily_budget_reserved" })?;
                set(&c, "resume_at", if pause > t { pause } else { next_day(t) })?;
                return Ok(Duration::from_secs(60));
            }
            set(&c, "status", "running")?;
            let s = service.clone();
            let cl = client.clone();
            let work = crate::query::run_database(s, move |s| next_work(s, &cl)).await.map_err(error)??;
            let Some(work) = work else {
                set(&c, "status", "waiting_for_candidates")?;
                return Ok(Duration::from_secs(5));
            };
            set(&c, "last_work", format!("OL{work}W"))?;
            match process(&service, &client, work).await {
                Ok(status) => {
                    finish(&c, work, status, 0, None)?;
                    failures = 0;
                    set(&c, "last_result", status)?;
                }
                Err(e) => {
                    let message = e.to_string();
                    // A quota stop leaves the work pending for the next day.
                    let budget = message.contains("daily budget") || message.contains("daily request limit");
                    let spacing = message.contains("spacing limit");
                    let retry = if budget {
                        next_day(now())
                    } else if spacing {
                        now() + 5
                    } else {
                        now() + 3600
                    };
                    finish(&c, work, if service.library_of_congress.is_some() { "ready_after_lc" } else { "pending" }, retry, Some(&message))?;
                    if !budget && !spacing {
                        failures += 1;
                        set(&c, "last_error", &message)?;
                        if message.contains("HTTP 429") {
                            set(&c, "pause_until", next_day(now()))?;
                        } else if failures >= 5 {
                            set(&c, "pause_until", now() + 1800)?;
                            failures = 0;
                        }
                        tracing::warn!(work,%e,"LibraryThing background work deferred");
                    }
                }
            }
            // Yield to foreground traffic between works, including during local-only scans.
            Ok(Duration::from_millis(100))
        }
        .await;
        match step {
            Ok(delay) => tokio::time::sleep(delay).await,
            Err(e) => {
                tracing::warn!(%e,"LibraryThing background worker retrying");
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    }
}

pub(crate) fn stats(client: &LibraryThingClient) -> Result<serde_json::Value, MetadataError> {
    let c = db(client)?;
    initialize(&c)?;
    let mut q = c.prepare("SELECT key,value FROM librarything_background_state").map_err(error)?;
    let state = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(error)?.collect::<Result<BTreeMap<_, _>, _>>().map_err(error)?;
    let mut q = c.prepare("SELECT status,count(*) FROM librarything_background_work GROUP BY status").map_err(error)?;
    let counts = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).map_err(error)?.collect::<Result<BTreeMap<_, _>, _>>().map_err(error)?;
    let (day, count) = c.query_row("SELECT day,count FROM librarything_requests WHERE id=1", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).optional().map_err(error)?.unwrap_or((0, 0));
    let used = if day == now() / 86400 { count } else { 0 };
    Ok(serde_json::json!({"state":state,"works":counts,"requests_today":used,"daily_limit":1000,"background_remaining":(950-used).max(0),"fallback_remaining":(1000-used).max(0),"fallback_reserve":50}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(path: &Path) -> MetadataService {
        let database = crate::tests::snapshot_from(
            path,
            &[
                serde_json::json!({"key":"/books/OL1M","works":[{"key":"/works/OL10W"}],"title":"Unique one","isbn_13":["9781538724736","9781691706631"]}),
                serde_json::json!({"key":"/books/OL2M","works":[{"key":"/works/OL20W"}],"title":"Unique two","isbn_13":["9781691706631"]}),
            ],
            &[],
            &[],
        );
        MetadataService::open(database).unwrap()
    }
    fn client(path: &Path) -> LibraryThingClient {
        let key = path.join("key");
        std::fs::write(&key, "test-key").unwrap();
        LibraryThingClient::open(&key, &path.join("cache.sqlite")).unwrap()
    }
    #[test]
    fn shared_isbn_ddc_blocks_even_though_normal_lcc_lookup_ignores_it() {
        let dir = tempfile::tempdir().unwrap();
        let service = fixture(dir.path());
        let c = Connection::open(&service.state.database).unwrap();
        c.execute("INSERT INTO edition_classification VALUES (2,1,'813.6')", []).unwrap();
        assert_eq!(eligible(&service, 10).unwrap(), Eligibility::Local);
    }
    #[test]
    fn normal_duplicate_recovery_prevents_background_requests() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::tests::duplicate_work_fixture(dir.path(), "An Empire of Wealth: The Epic History of American Economic Power", "Gordon, John Steele");
        let service = MetadataService::open(path).unwrap();
        assert_eq!(eligible(&service, 10).unwrap(), Eligibility::Local);
    }
    #[tokio::test]
    async fn cached_bisac_on_later_isbn_blocks_all_network_calls() {
        let dir = tempfile::tempdir().unwrap();
        let service = fixture(dir.path());
        let client = client(dir.path());
        let c = db(&client).unwrap();
        c.execute("INSERT INTO librarything_isbn_lcc VALUES ('9781691706631',?,0)", [r#"{"work_id":"1","codes":[],"ddc":[],"bisac":["FIC000000"],"all_schemes_checked":true}"#]).unwrap();
        assert_eq!(process(&service, &client, 10).await.unwrap(), "cached_codes");
        assert_eq!(c.query_row("SELECT count(*) FROM librarything_requests", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
    #[tokio::test]
    async fn failed_local_precheck_never_reaches_provider() {
        let dir = tempfile::tempdir().unwrap();
        let service = fixture(dir.path());
        let client = client(dir.path());
        Connection::open(&service.state.database).unwrap().execute("DROP TABLE edition_classification", []).unwrap();
        assert!(process(&service, &client, 10).await.is_err());
        assert_eq!(db(&client).unwrap().query_row("SELECT count(*) FROM librarything_requests", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
    #[test]
    fn queue_is_ranked_and_resumes_without_repeating_completed_works() {
        let dir = tempfile::tempdir().unwrap();
        let service = fixture(dir.path());
        let client = client(dir.path());
        seed(&service, &client, None).unwrap();
        let c = db(&client).unwrap();
        c.execute("INSERT INTO librarything_background_work(work_id,ratings) VALUES (20,50),(10,100)", []).unwrap();
        assert_eq!(next_work(&service, &client).unwrap(), Some(10));
        finish(&c, 10, "api_without_codes", 0, None).unwrap();
        drop(c);
        drop(client);
        let client = super::tests::client(dir.path());
        seed(&service, &client, None).unwrap();
        assert_eq!(next_work(&service, &client).unwrap(), Some(20));
        finish(&db(&client).unwrap(), 20, "pending", now() + 3600, Some("HTTP 403")).unwrap();
        assert_eq!(next_work(&service, &client).unwrap(), None);
        assert_eq!(state(&db(&client).unwrap(), "cursor").unwrap().as_deref(), Some("20"));
    }
    #[test]
    fn classified_sibling_without_isbn_and_local_bisac_block_requests() {
        let dir = tempfile::tempdir().unwrap();
        let service = fixture(dir.path());
        let c = Connection::open(&service.state.database).unwrap();
        c.execute("INSERT INTO edition VALUES(3,20)", []).unwrap();
        c.execute("INSERT INTO edition_classification VALUES(3,1,'813.6')", []).unwrap();
        // This sibling has no ISBN and no precomputed work aggregate.
        assert_eq!(eligible(&service, 20).unwrap(), Eligibility::Local);
        assert_eq!(eligible(&service, 10).unwrap(), Eligibility::Local);
        c.execute("DELETE FROM edition_classification", []).unwrap();
        c.execute_batch("CREATE TABLE work_bisac_subject(work_id INTEGER,code TEXT,path TEXT,source_subject TEXT)").unwrap();
        c.execute("INSERT INTO work_bisac_subject VALUES(20,'FIC000000','Fiction','Fiction')", []).unwrap();
        assert_eq!(eligible(&service, 10).unwrap(), Eligibility::Local);
    }
    #[tokio::test]
    async fn lc_must_finish_all_siblings_and_errors_never_authorize_librarything() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = fixture(dir.path());
        let lc_path = dir.path().join("separate-lc.sqlite");
        service = service.with_cached_library_of_congress(&lc_path).unwrap();
        let client = client(dir.path());
        let cache = Connection::open(&lc_path).unwrap();
        let isbns = vec!["9781538724736".into(), "9781691706631".into()];
        cache.execute("INSERT INTO loc_api_result VALUES('9781538724736','\"NoMatch\"',0)", []).unwrap();
        cache.execute("INSERT INTO loc_api_failure VALUES('9781691706631',9999999999,'HTTP 503')", []).unwrap();
        assert_eq!(lc_gate(&service, &isbns).unwrap(), LcGate::Pending);
        assert_eq!(process(&service, &client, 10).await.unwrap(), "awaiting_lc");
        assert!(!service.librarything_network_eligible(&isbns[0]).unwrap());
        assert_eq!(db(&client).unwrap().query_row("SELECT count(*) FROM librarything_requests", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        cache.execute("INSERT INTO loc_api_result VALUES('9781691706631','\"Ambiguous\"',0)", []).unwrap();
        assert_eq!(lc_gate(&service, &isbns).unwrap(), LcGate::Ready);
        assert!(service.librarything_network_eligible(&isbns[0]).unwrap());
        let code = crate::library_of_congress::LibraryOfCongressLookup::Matched {
            classifications: vec![metadata_contract::Classification { scheme: metadata_contract::ClassificationScheme::DeweyDecimal, notation: "813.6".into(), source: metadata_contract::ClassificationSource::Work, evidence: vec![] }],
            subjects: vec![],
        };
        cache.execute("UPDATE loc_api_result SET result=? WHERE isbn13='9781691706631'", [serde_json::to_string(&code).unwrap()]).unwrap();
        assert_eq!(lc_gate(&service, &isbns).unwrap(), LcGate::HasCode);
        assert!(!service.librarything_network_eligible(&isbns[0]).unwrap());
        assert_eq!(process(&service, &client, 10).await.unwrap(), "cached_codes");
    }
    #[test]
    fn durable_handoff_keeps_ranking_retries_and_completed_librarything_results() {
        let dir = tempfile::tempdir().unwrap();
        let service = fixture(dir.path()).with_cached_library_of_congress(&dir.path().join("lc.sqlite")).unwrap();
        let client = client(dir.path());
        seed(&service, &client, None).unwrap();
        let c = db(&client).unwrap();
        c.execute("INSERT INTO librarything_background_work(work_id,ratings) VALUES(10,100),(20,50)", []).unwrap();
        assert_eq!(next_work(&service, &client).unwrap(), None);
        enqueue_after_lc(&client, 20, 50).unwrap();
        enqueue_after_lc(&client, 10, 100).unwrap();
        assert_eq!(next_work(&service, &client).unwrap(), Some(10));
        finish(&c, 10, "ready_after_lc", now() + 3600, Some("HTTP 503")).unwrap();
        enqueue_after_lc(&client, 10, 100).unwrap();
        assert_eq!(next_work(&service, &client).unwrap(), Some(20));
        finish(&c, 20, "api_without_codes", 0, None).unwrap();
        enqueue_after_lc(&client, 20, 50).unwrap();
        drop(c);
        drop(client);
        let client = super::tests::client(dir.path());
        assert_eq!(next_work(&service, &client).unwrap(), None);
    }
}
