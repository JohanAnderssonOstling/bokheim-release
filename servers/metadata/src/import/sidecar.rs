use super::core::{completed_phase, import_input_identity};
use crate::{building_path, error, valid_dump_date, MetadataError, DESCRIPTION_IMPORT_CACHE_KIB};
use rusqlite::{params, Connection};
use std::fs;
use std::path::{Path, PathBuf};

const DESCRIPTION_SCHEMA: &str = "CREATE TABLE description_snapshot(
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    schema_version INTEGER NOT NULL,
    dump_date TEXT NOT NULL,
    imported_at_ms INTEGER NOT NULL,
    work_records INTEGER NOT NULL,
    description_records INTEGER NOT NULL,
    subject_heading_records INTEGER NOT NULL,
    matched_subject_heading_records INTEGER NOT NULL,
    bisac_assignment_records INTEGER NOT NULL
) STRICT;
CREATE TABLE description_import_setting(
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    builder_version INTEGER NOT NULL,
    record_limit INTEGER,
    dump_date TEXT NOT NULL
) STRICT;
CREATE TABLE description_import_input(
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    path TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    modified_ms INTEGER NOT NULL
) STRICT;
CREATE TABLE import_checkpoint(
    phase TEXT PRIMARY KEY,
    records_read INTEGER NOT NULL,
    completed INTEGER NOT NULL DEFAULT 0 CHECK(completed IN (0,1))
) WITHOUT ROWID;
CREATE TABLE work_description(
    work_id INTEGER PRIMARY KEY,
    description TEXT NOT NULL
) STRICT;
CREATE TABLE work_bisac_subject(
    work_id INTEGER NOT NULL,
    code TEXT NOT NULL,
    path TEXT NOT NULL,
    source_subject TEXT NOT NULL,
    PRIMARY KEY(work_id,code,source_subject)
) WITHOUT ROWID;
CREATE INDEX work_bisac_subject_by_code ON work_bisac_subject(code,work_id);
CREATE TABLE description_import_metric(
    name TEXT PRIMARY KEY,
    value INTEGER NOT NULL
) WITHOUT ROWID;
INSERT INTO description_import_metric(name,value) VALUES
    ('subject_heading_records',0),
    ('matched_subject_heading_records',0);";

pub(super) struct SidecarSpec {
    name: &'static str,
    input_name: &'static str,
    builder_version: i64,
    setting_table: &'static str,
    input_table: &'static str,
    schema: &'static str,
}

impl SidecarSpec {
    pub(super) fn description(builder_version: i64) -> Self {
        Self { name: "description", input_name: "works", builder_version, setting_table: "description_import_setting", input_table: "description_import_input", schema: DESCRIPTION_SCHEMA }
    }
}

pub(super) struct SidecarBuilder<'a> {
    output_path: &'a Path,
    building_path: PathBuf,
    name: &'static str,
    connection: Connection,
}

impl<'a> SidecarBuilder<'a> {
    pub(super) fn open(input_path: &Path, output_path: &'a Path, dump_date: &str, record_limit: Option<usize>, spec: SidecarSpec) -> Result<Self, MetadataError> {
        if !valid_dump_date(dump_date) {
            return Err(MetadataError(format!("{} dump date must use YYYY-MM-DD", spec.name)));
        }
        if output_path.exists() {
            return Err(MetadataError(format!("output already exists: {}", output_path.display())));
        }
        let building_path = building_path(output_path);
        let resuming = building_path.exists();
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).map_err(error)?;
        }
        let connection = Connection::open(&building_path).map_err(error)?;
        connection.execute_batch(&format!("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA temp_store=FILE; PRAGMA cache_size=-{DESCRIPTION_IMPORT_CACHE_KIB}; PRAGMA locking_mode=EXCLUSIVE;")).map_err(error)?;
        let input = import_input_identity(input_path)?;
        let stored_limit = record_limit.map(i64::try_from).transpose().map_err(error)?;
        if resuming {
            let settings_sql = format!("SELECT builder_version,record_limit,dump_date FROM {} WHERE singleton=1", spec.setting_table);
            let settings = connection
                .query_row(&settings_sql, (), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, String>(2)?)))
                .map_err(|failure| MetadataError(format!("unfinished {} sidecar cannot be resumed: {failure}", spec.name)))?;
            if settings != (spec.builder_version, stored_limit, dump_date.to_owned()) {
                return Err(MetadataError(format!("unfinished {} sidecar uses different builder settings", spec.name)));
            }
            let input_sql = format!("SELECT path,size_bytes,modified_ms FROM {} WHERE singleton=1", spec.input_table);
            let stored_input = connection.query_row(&input_sql, (), |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))).map_err(error)?;
            if stored_input != input {
                return Err(MetadataError(format!("cannot resume {} sidecar: {} dump path, size, or modification time changed", spec.name, spec.input_name)));
            }
            println!("resuming {} sidecar build {}", spec.name, building_path.display());
        } else {
            connection.execute_batch(spec.schema).map_err(error)?;
            let settings_sql = format!("INSERT INTO {}(singleton,builder_version,record_limit,dump_date) VALUES(1,?1,?2,?3)", spec.setting_table);
            connection.execute(&settings_sql, params![spec.builder_version, stored_limit, dump_date]).map_err(error)?;
            let input_sql = format!("INSERT INTO {}(singleton,path,size_bytes,modified_ms) VALUES(1,?1,?2,?3)", spec.input_table);
            connection.execute(&input_sql, params![input.0, input.1, input.2]).map_err(error)?;
        }
        Ok(Self { output_path, building_path, name: spec.name, connection })
    }

    pub(super) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    pub(super) fn finalize_once(&self, phase: &str, finalize: impl FnOnce(&Connection) -> Result<(), MetadataError>) -> Result<(), MetadataError> {
        if completed_phase(&self.connection, phase)?.is_some() {
            return Ok(());
        }
        finalize(&self.connection)?;
        self.connection.execute_batch("ANALYZE; PRAGMA optimize;").map_err(error)?;
        let integrity: String = self.connection.query_row("PRAGMA integrity_check", (), |row| row.get(0)).map_err(error)?;
        if integrity != "ok" {
            return Err(MetadataError(format!("{} sidecar integrity check failed: {integrity}", self.name)));
        }
        self.connection.execute("INSERT INTO import_checkpoint(phase,records_read,completed) VALUES(?1,0,1)", [phase]).map_err(error)?;
        Ok(())
    }

    pub(super) fn publish(self) -> Result<(), MetadataError> {
        self.connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;").map_err(error)?;
        drop(self.connection);
        fs::rename(&self.building_path, self.output_path).map_err(error)?;
        let size = fs::metadata(self.output_path).map_err(error)?.len();
        println!("created {} sidecar {} ({size} bytes)", self.name, self.output_path.display());
        Ok(())
    }
}
