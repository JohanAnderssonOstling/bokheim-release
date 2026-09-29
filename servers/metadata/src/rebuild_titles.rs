//! Rebuild an immutable authority database's title keys and backfill subtitles from its dumps.
use crate::import::{core::import_input_identity, pipeline::read_open_library_resumable};
use crate::{error, MetadataError};
use metadata_contract::matching::bibliographic_title_match_key;
use rusqlite::{params, Connection};
use std::{fs, path::Path};

#[derive(serde::Deserialize)]
struct SubtitleRecord {
    key: String,
    #[serde(default, deserialize_with = "crate::deserialize_optional_string")]
    subtitle: Option<String>,
    #[serde(default, deserialize_with = "crate::deserialize_optional_string")]
    title: Option<String>,
}

pub fn rebuild_titles(source: &Path, editions: &Path, works: &Path, output: &Path) -> Result<(), MetadataError> {
    if output.exists() {
        return Err(MetadataError("output already exists".into()));
    }
    let building = output.with_extension("titles-building.sqlite");
    let inputs = serde_json::to_string(&(1, import_input_identity(source)?, import_input_identity(editions)?, import_input_identity(works)?)).map_err(error)?;
    if !building.exists() {
        println!("copying immutable source {}", source.display());
        let copying = output.with_extension("titles-copying.sqlite");
        fs::copy(source, &copying).map_err(error)?;
        let c = Connection::open(&copying).map_err(error)?;
        c.execute_batch("CREATE TABLE title_rebuild_input(identity TEXT NOT NULL);").map_err(error)?;
        c.execute("INSERT INTO title_rebuild_input VALUES(?1)", [&inputs]).map_err(error)?;
        drop(c);
        fs::rename(copying, &building).map_err(error)?;
    }
    let mut c = Connection::open(&building).map_err(error)?;
    let stored: String = c.query_row("SELECT identity FROM title_rebuild_input", [], |r| r.get(0)).map_err(error)?;
    if stored != inputs {
        return Err(MetadataError("title rebuild inputs changed; refusing to resume".into()));
    }
    let cache_kib = std::env::var("BOKHEIM_TITLE_REBUILD_CACHE_KIB").ok().map(|value| value.parse::<usize>().map_err(error)).transpose()?.unwrap_or(131072);
    if !(16384..=1048576).contains(&cache_kib) {
        return Err(MetadataError("title rebuild cache must be 16384..1048576 KiB".into()));
    }
    c.execute_batch(&format!("PRAGMA cache_size=-{cache_kib};")).map_err(error)?;
    c.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
        CREATE TABLE IF NOT EXISTS import_checkpoint(phase TEXT PRIMARY KEY,records_read INTEGER NOT NULL,completed INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS edition_subtitle(edition_id INTEGER PRIMARY KEY,subtitle TEXT NOT NULL,normalized_title TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS work_subtitle(work_id INTEGER PRIMARY KEY,subtitle TEXT NOT NULL,normalized_title TEXT NOT NULL);
        DROP INDEX IF EXISTS edition_bibliography_by_title; DROP INDEX IF EXISTS work_bibliography_by_title;",
    )
    .map_err(error)?;
    for (table, id) in [("edition_bibliography", "edition_id"), ("work_bibliography", "work_id")] {
        let phase = format!("title_keys_v2_{table}");
        let mut last: i64 = c.query_row("SELECT COALESCE((SELECT records_read FROM import_checkpoint WHERE phase=?1),0)", [&phase], |r| r.get(0)).map_err(error)?;
        loop {
            let rows = c
                .prepare(&format!("SELECT {id},title FROM {table} WHERE {id}>?1 AND (instr(title,char(39))>0 OR instr(title,'’')>0 OR instr(title,'ʼ')>0) ORDER BY {id} LIMIT 10000"))
                .map_err(error)?
                .query_map([last], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .map_err(error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(error)?;
            if rows.is_empty() {
                break;
            }
            let tx = c.transaction().map_err(error)?;
            {
                let mut update = tx.prepare_cached(&format!("UPDATE {table} SET normalized_title=?2 WHERE {id}=?1 AND normalized_title!=?2")).map_err(error)?;
                for (row_id, title) in rows {
                    update.execute(params![row_id, bibliographic_title_match_key(&title)]).map_err(error)?;
                    last = row_id;
                }
            }
            tx.execute("INSERT OR REPLACE INTO import_checkpoint VALUES(?1,?2,0)", params![phase, last]).map_err(error)?;
            tx.commit().map_err(error)?;
        }
        println!("rebuilt {table} title keys");
    }
    for (path, table, id, kind, phase) in [(editions, "edition", "edition_id", 'M', "subtitle_editions_v1"), (works, "work", "work_id", 'W', "subtitle_works_v1")] {
        // The pinned dump already has the display title. Retained IDs need
        // only one bit each, avoiding a random SQLite read per subtitle.
        let max_id: i64 = c.query_row(&format!("SELECT COALESCE(MAX({id}),0) FROM {table}_bibliography"), [], |r| r.get(0)).map_err(error)?;
        let size = usize::try_from(max_id / 8 + 1).map_err(error)?;
        if size > 128 * 1024 * 1024 {
            return Err(MetadataError("authority ID range exceeds title backfill memory bound".into()));
        }
        let mut retained = vec![0u8; size];
        {
            let mut ids = c.prepare(&format!("SELECT {id} FROM {table}_bibliography")).map_err(error)?;
            for row in ids.query_map([], |r| r.get::<_, i64>(0)).map_err(error)? {
                let value = usize::try_from(row.map_err(error)?).map_err(error)?;
                retained[value / 8] |= 1 << (value % 8);
            }
        }
        println!("loaded {table} retained-ID filter");
        read_open_library_resumable::<SubtitleRecord>(&mut c, path, None, phase, |tx, records| {
            let mut insert = tx.prepare_cached(&format!("INSERT OR REPLACE INTO {table}_subtitle VALUES(?1,?2,?3)")).map_err(error)?;
            for record in records {
                let Some(subtitle) = record.subtitle.filter(|s| !s.trim().is_empty() && s.len() <= 4096) else { continue };
                let Some(record_id) = crate::open_library_id(&record.key, kind) else { continue };
                let value = usize::try_from(record_id).map_err(error)?;
                if !retained.get(value / 8).is_some_and(|byte| byte & (1 << (value % 8)) != 0) {
                    continue;
                }
                let Some(main) = record.title.filter(|s| !s.trim().is_empty() && s.len() <= 4096) else { continue };
                let main = main.split_whitespace().collect::<Vec<_>>().join(" ");
                let subtitle = subtitle.split_whitespace().collect::<Vec<_>>().join(" ");
                insert.execute(params![record_id, subtitle, bibliographic_title_match_key(&format!("{main}: {subtitle}"))]).map_err(error)?;
            }
            Ok(())
        })?;
    }
    c.execute_batch(
        "CREATE INDEX IF NOT EXISTS edition_bibliography_by_title ON edition_bibliography(normalized_title,edition_id);
        CREATE INDEX IF NOT EXISTS work_bibliography_by_title ON work_bibliography(normalized_title,work_id);
        CREATE INDEX IF NOT EXISTS edition_subtitle_by_title ON edition_subtitle(normalized_title,edition_id);
        CREATE INDEX IF NOT EXISTS work_subtitle_by_title ON work_subtitle(normalized_title,work_id);
        ANALYZE edition_subtitle; ANALYZE work_subtitle; PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;",
    )
    .map_err(error)?;
    let check: String = c.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(error)?;
    if check != "ok" {
        return Err(MetadataError(check));
    }
    for table in ["edition_subtitle", "work_subtitle"] {
        let count: i64 = c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).map_err(error)?;
        println!("{table}: {count}");
    }
    drop(c);
    fs::rename(building, output).map_err(error)?;
    println!("finished {}", output.display());
    Ok(())
}
