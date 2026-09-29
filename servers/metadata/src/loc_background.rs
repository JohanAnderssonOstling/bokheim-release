//! Durable first-stage LC queue; completed code misses feed LibraryThing.
use crate::{
    error,
    library_of_congress::{LibraryOfCongressClient, LibraryOfCongressLookup},
    librarything_background::{eligible, Eligibility},
    MetadataError, MetadataService,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::{path::PathBuf, time::Duration};
fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}
fn next(service: &MetadataService, c: &Connection) -> Result<Option<i64>, MetadataError> {
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS loc_background_work(work_id INTEGER PRIMARY KEY,ratings INTEGER NOT NULL DEFAULT 0,status TEXT NOT NULL DEFAULT 'pending',retry_at INTEGER NOT NULL DEFAULT 0,last_error TEXT);
        CREATE INDEX IF NOT EXISTS loc_background_pending ON loc_background_work(status,ratings DESC,work_id);
        CREATE TABLE IF NOT EXISTS loc_background_state(key TEXT PRIMARY KEY,value TEXT NOT NULL);",
    )
    .map_err(error)?;
    let policy: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM loc_background_state WHERE key='provider_order_v1')", [], |r| r.get(0)).map_err(error)?;
    if !policy {
        // Re-evaluate old year-gated/skipped work using permanent cached results.
        let tx = c.unchecked_transaction().map_err(error)?;
        tx.execute("UPDATE loc_background_work SET status='pending',retry_at=0 WHERE status IN ('local_or_not_recent','no_usable_subjects','cached_subjects','subjects_found')", []).map_err(error)?;
        tx.execute("INSERT INTO loc_background_state VALUES ('provider_order_v1','1')", []).map_err(error)?;
        tx.commit().map_err(error)?;
    }
    let seeded: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM loc_background_state WHERE key='seeded')", [], |r| r.get(0)).map_err(error)?;
    if !seeded {
        if let Some(lt) = &service.librarything {
            let ranking = Connection::open(lt.cache_path.as_ref()).map_err(error)?;
            ranking.busy_timeout(Duration::from_secs(2)).map_err(error)?;
            let exists: bool = ranking.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='librarything_background_state')", [], |r| r.get(0)).map_err(error)?;
            let ready = exists && ranking.query_row("SELECT EXISTS(SELECT 1 FROM librarything_background_state WHERE key='rank_seed')", [], |r| r.get::<_, bool>(0)).map_err(error)?;
            if ready {
                let rows = {
                    let mut q = ranking.prepare("SELECT work_id,ratings FROM librarything_background_work").map_err(error)?;
                    let rows = q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
                    rows
                };
                drop(ranking);
                let tx = c.unchecked_transaction().map_err(error)?;
                for (id, ratings) in rows {
                    tx.execute("INSERT INTO loc_background_work(work_id,ratings) VALUES(?,?) ON CONFLICT(work_id) DO UPDATE SET ratings=excluded.ratings", params![id, ratings]).map_err(error)?;
                }
                tx.execute("INSERT OR REPLACE INTO loc_background_state VALUES ('seeded','1')", []).map_err(error)?;
                tx.commit().map_err(error)?;
            } else if std::env::var("BOKHEIM_LIBRARYTHING_BACKGROUND").as_deref() != Ok("false") {
                return Ok(None);
            }
        }
    }
    let pending = || c.query_row("SELECT work_id FROM loc_background_work WHERE status='pending' AND retry_at<=? ORDER BY ratings DESC,work_id LIMIT 1", [now()], |r| r.get(0)).optional().map_err(error);
    if let Some(id) = pending()? {
        return Ok(Some(id));
    }
    let cursor: i64 = c.query_row("SELECT CAST(value AS INTEGER) FROM loc_background_state WHERE key='cursor'", [], |r| r.get(0)).optional().map_err(error)?.unwrap_or(0);
    let identity = crate::query::query_connection(&service.state.identity_pool)?;
    let mut q = identity.prepare("SELECT DISTINCT work_id FROM edition WHERE work_id>? ORDER BY work_id LIMIT 256").map_err(error)?;
    let ids = q.query_map([cursor], |r| r.get::<_, i64>(0)).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
    for id in &ids {
        c.execute("INSERT OR IGNORE INTO loc_background_work(work_id) VALUES(?)", [id]).map_err(error)?;
    }
    if let Some(id) = ids.last() {
        c.execute("INSERT OR REPLACE INTO loc_background_state VALUES ('cursor',?)", [id.to_string()]).map_err(error)?;
    }
    pending()
}
fn usable(result: &LibraryOfCongressLookup) -> bool {
    matches!(result,LibraryOfCongressLookup::Matched{classifications,..} if !classifications.is_empty())
}
#[derive(Debug, PartialEq)]
enum Plan {
    Done(&'static str),
    Request(String),
    Backoff,
}
fn plan(service: &MetadataService, provider: &LibraryOfCongressClient, work: i64) -> Result<Plan, MetadataError> {
    let all = match eligible(service, work)? {
        Eligibility::Isbns(v) => v,
        _ => return Ok(Plan::Done("local")),
    };
    for isbn in &all {
        if let Some(lt) = &service.librarything {
            if let Some(v) = lt.cached(isbn)? {
                if !v.codes.is_empty() || !v.ddc.is_empty() || !v.bisac.is_empty() {
                    return Ok(Plan::Done("local"));
                }
            }
        }
        if provider.cached(isbn)?.as_ref().is_some_and(usable) {
            return Ok(Plan::Done("cached_subjects"));
        }
    }
    let mut deferred = false;
    let mut candidate = None;
    // Only one untried edition per work enters a batch. Further editions are
    // attempted on the next round only when this one returns no usable subjects.
    for isbn in all {
        if provider.cached(&isbn)?.is_some() {
            continue;
        }
        if !provider.retry_ready(&isbn)? {
            deferred = true;
            continue;
        }
        if candidate.is_none() {
            candidate = Some(isbn)
        }
    }
    Ok(if let Some(isbn) = candidate {
        Plan::Request(isbn)
    } else if deferred {
        Plan::Backoff
    } else {
        Plan::Done("no_usable_subjects")
    })
}
fn finish(c: &Connection, work: i64, status: &str, retry: i64, error: Option<&str>) -> Result<(), MetadataError> {
    c.execute("UPDATE loc_background_work SET status=?,retry_at=?,last_error=? WHERE work_id=?", params![status, retry, error, work]).map_err(crate::error)?;
    Ok(())
}
pub(crate) async fn run(service: MetadataService, provider: LibraryOfCongressClient, path: PathBuf) {
    let mut failures = 0u32;
    let mut snapshot_checked = false;
    loop {
        let step: Result<Duration, MetadataError> = async {
            if !snapshot_checked {
                let p = path.clone();
                crate::query::run_database(service.clone(), move |s| {
                    let c = Connection::open(p).map_err(error)?;
                    c.busy_timeout(Duration::from_secs(2)).map_err(error)?;
                    c.execute_batch("CREATE TABLE IF NOT EXISTS loc_background_state(key TEXT PRIMARY KEY,value TEXT NOT NULL)").map_err(error)?;
                    let signature = format!("{}:{}:{}", s.state.snapshot.dump_date, s.state.snapshot.imported_at_ms, std::fs::canonicalize(&s.state.database).map_err(error)?.display());
                    let old: Option<String> = c.query_row("SELECT value FROM loc_background_state WHERE key='snapshot'", [], |r| r.get(0)).optional().map_err(error)?;
                    if old.as_deref() != Some(signature.as_str()) {
                        let tx = c.unchecked_transaction().map_err(error)?;
                        if tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='loc_background_work')", [], |r| r.get::<_, bool>(0)).map_err(error)? {
                            tx.execute("UPDATE loc_background_work SET status='pending',retry_at=0 WHERE status!='pending'", []).map_err(error)?;
                        }
                        tx.execute("DELETE FROM loc_background_state WHERE key IN ('cursor','seeded')", []).map_err(error)?;
                        tx.execute("INSERT OR REPLACE INTO loc_background_state VALUES('snapshot',?)", [signature]).map_err(error)?;
                        tx.commit().map_err(error)?;
                    }
                    Ok::<_, MetadataError>(())
                })
                .await
                .map_err(error)??;
                snapshot_checked = true;
            }
            let mut batch = Vec::<(i64, String)>::new();
            let started = std::time::Instant::now();
            for _ in 0..256 {
                let p = path.clone();
                let provider = provider.clone();
                let candidate = crate::query::run_database(service.clone(), move |s| {
                    let c = Connection::open(p).map_err(error)?;
                    c.busy_timeout(Duration::from_secs(2)).map_err(error)?;
                    let Some(work) = next(s, &c)? else { return Ok(None) };
                    // A short durable lease makes interrupted batches retryable.
                    finish(&c, work, "pending", now() + 120, None)?;
                    let plan = match plan(s, &provider, work) {
                        Ok(v) => v,
                        Err(e) => {
                            finish(&c, work, "pending", now() + 3600, Some(&e.to_string()))?;
                            return Ok(Some(None));
                        }
                    };
                    match plan {
                        Plan::Done(status) => {
                            if status == "no_usable_subjects" {
                                if let Some(lt) = &s.librarything {
                                    let ratings = c.query_row("SELECT ratings FROM loc_background_work WHERE work_id=?", [work], |r| r.get(0)).map_err(error)?;
                                    crate::librarything_background::enqueue_after_lc(lt, work, ratings)?;
                                }
                            }
                            finish(&c, work, status, 0, None)?;
                            Ok(Some(None))
                        }
                        Plan::Backoff => {
                            finish(&c, work, "pending", now() + 3600, Some("LC ISBN backoff"))?;
                            Ok(Some(None))
                        }
                        Plan::Request(isbn) => Ok(Some(Some((work, isbn)))),
                    }
                })
                .await
                .map_err(error)??;
                match candidate {
                    Some(Some(v)) => batch.push(v),
                    Some(None) => {}
                    None => break,
                }
                if batch.len() == crate::library_of_congress::MAX_BATCH_ISBNS || started.elapsed() > Duration::from_secs(10) {
                    break;
                }
            }
            if batch.is_empty() {
                return Ok(Duration::from_secs(1));
            }
            let isbns = batch.iter().map(|(_, isbn)| isbn.clone()).collect::<Vec<_>>();
            let outcome = provider.lookup_batch(&isbns).await;
            let c = Connection::open(&path).map_err(error)?;
            c.busy_timeout(Duration::from_secs(2)).map_err(error)?;
            match outcome {
                Ok(values) => {
                    for ((work, _), value) in batch.iter().zip(values) {
                        finish(&c, *work, if usable(&value) { "subjects_found" } else { "pending" }, 0, None)?;
                    }
                    failures = 0;
                }
                Err(e) => {
                    for (work, _) in &batch {
                        finish(&c, *work, "pending", now() + 3600, Some(&e.to_string()))?;
                    }
                    failures += 1;
                    tracing::warn!(isbns=batch.len(),%e,"LC background batch deferred");
                }
            }
            if failures >= 5 {
                failures = 0;
                return Ok(Duration::from_secs(1800));
            }
            Ok(Duration::from_millis(100))
        }
        .await;
        match step {
            Ok(delay) => tokio::time::sleep(delay).await,
            Err(e) => {
                tracing::warn!(%e,"LC background retrying");
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_years_and_queue_do_not_depend_on_lt_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::tests::snapshot_from(
            dir.path(),
            &[
                serde_json::json!({"key":"/books/OL1M","works":[{"key":"/works/OL10W"}],"title":"Old","isbn_13":["9781538724736"],"publish_date":"2015"}),
                serde_json::json!({"key":"/books/OL2M","works":[{"key":"/works/OL20W"}],"title":"New","isbn_13":["9781691706631"],"publish_date":"2016"}),
            ],
            &[],
            &[],
        );
        let service = MetadataService::open(path).unwrap();
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(next(&service, &c).unwrap(), Some(10));
        c.execute("UPDATE loc_background_work SET status='done' WHERE work_id=10", []).unwrap();
        assert_eq!(next(&service, &c).unwrap(), Some(20));
        let provider = LibraryOfCongressClient::new().unwrap().with_cache(&dir.path().join("lc.sqlite")).unwrap();
        assert_eq!(plan(&service, &provider, 10).unwrap(), Plan::Request("9781538724736".into()));
        assert_eq!(plan(&service, &provider, 20).unwrap(), Plan::Request("9781691706631".into()));
        let cache = Connection::open(dir.path().join("lc.sqlite")).unwrap();
        cache.execute("INSERT INTO loc_api_result VALUES('9781691706631',?,0)", [serde_json::to_string(&LibraryOfCongressLookup::NoMatch).unwrap()]).unwrap();
        assert_eq!(plan(&service, &provider, 20).unwrap(), Plan::Done("no_usable_subjects"));
        // A cancelled batch lease cannot permanently lose a candidate.
        finish(&c, 20, "pending", now() + 120, None).unwrap();
        assert_eq!(next(&service, &c).unwrap(), None);
        finish(&c, 20, "pending", 0, None).unwrap();
        assert_eq!(next(&service, &c).unwrap(), Some(20));
    }
    #[test]
    fn migration_revisits_old_skips_once_and_preserves_provider_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::tests::snapshot_from(dir.path(), &[], &[], &[]);
        let service = MetadataService::open(path).unwrap();
        let c = Connection::open_in_memory().unwrap();
        next(&service, &c).unwrap();
        c.execute("DELETE FROM loc_background_state WHERE key='provider_order_v1'", []).unwrap();
        c.execute("INSERT INTO loc_background_work(work_id,status) VALUES(10,'local_or_not_recent'),(20,'no_usable_subjects')", []).unwrap();
        assert_eq!(next(&service, &c).unwrap(), Some(10));
        finish(&c, 10, "local", 0, None).unwrap();
        assert_eq!(next(&service, &c).unwrap(), Some(20));
        finish(&c, 20, "no_usable_subjects", 0, None).unwrap();
        assert_eq!(next(&service, &c).unwrap(), None);
    }

    #[test]
    fn headings_alone_do_not_stop_classification_fallback() {
        let value = LibraryOfCongressLookup::Matched { classifications: vec![], subjects: vec![metadata_contract::SubjectHeading { source: "lc".into(), authority: "lcsh".into(), components: vec!["History".into()], authority_uri: None }] };
        assert!(!usable(&value));
    }
}
