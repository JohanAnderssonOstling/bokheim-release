use super::super::{error, open_library_id, params, valid_open_library_description, BTreeSet, DescriptionWorkRecord, MetadataError, Path, SystemTime, DESCRIPTION_BUILDER_VERSION, DESCRIPTION_SCHEMA_VERSION, UNIX_EPOCH};
use super::pipeline::read_open_library_resumable;
use super::sidecar::{SidecarBuilder, SidecarSpec};

/// Builds an immutable Open Library work-description sidecar without
/// rebuilding the ISBN, edition, author, or classification snapshot.
pub fn import_description_snapshot(works_path: &Path, output_path: &Path, dump_date: &str, record_limit: Option<usize>) -> Result<(), MetadataError> {
    let mut builder = SidecarBuilder::open(works_path, output_path, dump_date, record_limit, SidecarSpec::description(DESCRIPTION_BUILDER_VERSION))?;

    let work_records = read_open_library_resumable::<DescriptionWorkRecord>(builder.connection_mut(), works_path, record_limit, "descriptions", |transaction, records| {
        let mut insert = transaction.prepare_cached("INSERT OR REPLACE INTO work_description(work_id,description) VALUES(?1,?2)").map_err(error)?;
        let mut subject_insert = transaction.prepare_cached("INSERT OR IGNORE INTO work_bisac_subject(work_id,code,path,source_subject) VALUES(?1,?2,?3,?4)").map_err(error)?;
        let mut subject_heading_records = 0_i64;
        let mut matched_subject_heading_records = 0_i64;
        for record in records {
            let Some(work_id) = open_library_id(&record.key, 'W') else { continue };
            if let Some(description) = record.description.as_deref().and_then(valid_open_library_description) {
                insert.execute(params![work_id, description]).map_err(error)?;
            }
            let subjects = record.subjects.into_iter().filter_map(|subject| subject_projection::BookSubject::new(None, &subject, "openlibrary:subject", None, None).ok()).collect::<Vec<_>>();
            subject_heading_records = subject_heading_records.saturating_add(i64::try_from(subjects.len()).map_err(error)?);
            let mut matched_positions = BTreeSet::new();
            for assignment in subject_projection::match_bisac_subjects_with_minimum_adjacent(&subjects, 3) {
                let Some(code) = subject_projection::bisac_code_for_path(assignment.node_path()) else { continue };
                for &position in assignment.evidence_positions() {
                    let Some(subject) = subjects.get(position) else { continue };
                    subject_insert.execute(params![work_id, code, assignment.node_path(), subject.name()]).map_err(error)?;
                    matched_positions.insert(position);
                }
            }
            matched_subject_heading_records = matched_subject_heading_records.saturating_add(i64::try_from(matched_positions.len()).map_err(error)?);
        }
        transaction.execute("UPDATE description_import_metric SET value=value+?2 WHERE name=?1", params!["subject_heading_records", subject_heading_records]).map_err(error)?;
        transaction.execute("UPDATE description_import_metric SET value=value+?2 WHERE name=?1", params!["matched_subject_heading_records", matched_subject_heading_records]).map_err(error)?;
        Ok(())
    })?;
    builder.finalize_once("description_finalize", |connection| {
        let description_records: i64 = connection.query_row("SELECT COUNT(*) FROM work_description", (), |row| row.get(0)).map_err(error)?;
        let subject_heading_records: i64 = connection.query_row("SELECT value FROM description_import_metric WHERE name='subject_heading_records'", (), |row| row.get(0)).map_err(error)?;
        let matched_subject_heading_records: i64 = connection.query_row("SELECT value FROM description_import_metric WHERE name='matched_subject_heading_records'", (), |row| row.get(0)).map_err(error)?;
        let bisac_assignment_records: i64 = connection.query_row("SELECT COUNT(*) FROM (SELECT DISTINCT work_id,code FROM work_bisac_subject)", (), |row| row.get(0)).map_err(error)?;
        let imported_at_ms = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).map_err(error)?.as_millis()).map_err(error)?;
        connection
            .execute(
                "INSERT INTO description_snapshot(singleton,schema_version,dump_date,imported_at_ms,work_records,description_records,subject_heading_records,matched_subject_heading_records,bisac_assignment_records)
                 VALUES(1,?1,?2,?3,?4,?5,?6,?7,?8)",
                params![DESCRIPTION_SCHEMA_VERSION, dump_date, imported_at_ms, i64::try_from(work_records).map_err(error)?, description_records, subject_heading_records, matched_subject_heading_records, bisac_assignment_records],
            )
            .map_err(error)?;
        Ok(())
    })?;
    builder.publish()
}
