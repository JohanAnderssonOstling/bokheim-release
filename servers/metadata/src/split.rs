use super::{error, Connection, MetadataError, Path};

const SUBJECT_TABLES: &[&str] = &["snapshot", "metadata_metric", "edition", "edition_isbn", "edition_classification", "isbn_classification", "edition_work_classification", "work_classification", "isbn_lcc", "lcc_import"];
const RICH_TABLES: &[&str] = &["edition_description", "wikidata_ingestion", "snapshot", "metadata_metric", "edition_author", "work_author", "author", "author_identifier", "work_description", "lc_record", "lc_record_isbn"];
const IDENTITY_TABLES: &[&str] = &[
    "lc_record",
    "lc_record_lcc",
    "lc_title",
    "isbn_lcc",
    "snapshot",
    "metadata_metric",
    "edition",
    "edition_isbn",
    "edition_subtitle",
    "work_subtitle",
    "edition_bibliography",
    "work_bibliography",
    "edition_publisher",
    "edition_author",
    "work_author",
    "author",
    "author_identifier",
    "edition_classification",
    "isbn_classification",
    "title_author_classification",
    "edition_work_classification",
    "work_classification",
];

pub fn split_snapshot(source: &Path, subjects: &Path, rich: &Path, identities: &Path) -> Result<(), MetadataError> {
    for output in [subjects, rich, identities] {
        if output.exists() {
            return Err(MetadataError(format!("split snapshot output already exists: {}", output.display())));
        }
    }
    build_store(source, subjects, SUBJECT_TABLES)?;
    if let Err(failure) = build_store(source, rich, RICH_TABLES) {
        let _ = std::fs::remove_file(subjects);
        return Err(failure);
    }
    if let Err(failure) = build_store(source, identities, IDENTITY_TABLES) {
        let _ = std::fs::remove_file(subjects);
        let _ = std::fs::remove_file(rich);
        return Err(failure);
    }
    Ok(())
}

fn build_store(source: &Path, output: &Path, requested_tables: &[&str]) -> Result<(), MetadataError> {
    let building = output.with_extension(format!("{}.building", output.extension().and_then(|value| value.to_str()).unwrap_or("sqlite")));
    if building.exists() {
        std::fs::remove_file(&building).map_err(error)?;
    }
    let mut target = Connection::open(&building).map_err(error)?;
    target.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=FILE; PRAGMA foreign_keys=OFF;").map_err(error)?;
    target.execute("ATTACH DATABASE ?1 AS source", [source.to_string_lossy().as_ref()]).map_err(error)?;

    let available = requested_tables
        .iter()
        .filter_map(|table| target.query_row("SELECT sql FROM source.sqlite_master WHERE type='table' AND name=?1 AND sql IS NOT NULL", [*table], |row| row.get::<_, String>(0)).ok().map(|sql| ((*table).to_owned(), sql)))
        .collect::<Vec<_>>();
    if !available.iter().any(|(table, _)| table == "snapshot") {
        return Err(MetadataError("source metadata snapshot has no snapshot table".to_owned()));
    }

    let transaction = target.transaction().map_err(error)?;
    for (table, sql) in &available {
        transaction.execute_batch(sql).map_err(error)?;
        transaction.execute_batch(&format!("INSERT INTO main.\"{table}\" SELECT * FROM source.\"{table}\";")).map_err(error)?;
    }
    for (table, _) in &available {
        let mut indexes = transaction.prepare("SELECT sql FROM source.sqlite_master WHERE type='index' AND tbl_name=?1 AND sql IS NOT NULL ORDER BY name").map_err(error)?;
        let definitions = indexes.query_map([table], |row| row.get::<_, String>(0)).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
        drop(indexes);
        for definition in definitions {
            transaction.execute_batch(&definition).map_err(error)?;
        }
    }
    transaction.commit().map_err(error)?;
    target.execute_batch("DETACH DATABASE source; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; VACUUM; PRAGMA integrity_check;").map_err(error)?;
    drop(target);
    std::fs::rename(&building, output).map_err(error)
}
