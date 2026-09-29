//! Reader annotations: highlights, notes, and their anchors.

use crate::{Database, DatabaseError};
use book_model::ReaderAnnotation;
use include_sqlite_sql::include_sql;
use rusqlite::{Row, types::Type};
use sync_common::ContentHash;

impl Database {
    pub fn annotations(&self, content_hash: ContentHash) -> Result<Vec<ReaderAnnotation>, DatabaseError> {
        let mut values = Vec::new();
        self.connection.get_annotations(content_hash.as_str(), |row| {
            values.push(read_annotation(row)?);
            Ok(())
        })?;
        Ok(values)
    }

    pub fn upsert_annotation(&self, value: &ReaderAnnotation) -> Result<(), DatabaseError> {
        value.validate(false).map_err(DatabaseError::operation)?;
        let mut state = value.to_sync_state(false).map_err(DatabaseError::operation)?;
        if let Some(existing) = self.annotation_by_id(&value.id)? {
            state.created_at = u64::try_from(existing.created_at).unwrap_or(state.created_at);
        }
        let detail = state.detail_json().map_err(DatabaseError::operation)?;
        let transaction = self.connection.unchecked_transaction()?;
        transaction.upsert_annotation(&value.id, value.content_hash.as_str(), value.toc_ordinal, value.progress, &detail, value.modified_at, None)?;
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    pub fn delete_annotation(&self, id: &str, modified_at: i64) -> Result<(), DatabaseError> {
        u64::try_from(modified_at).map_err(DatabaseError::operation)?;
        let transaction = self.connection.unchecked_transaction()?;
        let changed = transaction.delete_annotation(id, modified_at)?;
        if changed == 0 {
            return Err(DatabaseError::message("annotation not found"));
        }
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    fn annotation_by_id(&self, id: &str) -> Result<Option<ReaderAnnotation>, DatabaseError> {
        let mut existing = None;
        self.connection.get_annotation_by_id(id, |row| {
            existing = Some(read_annotation(row)?);
            Ok(())
        })?;
        Ok(existing)
    }
}

fn read_annotation(row: &Row<'_>) -> rusqlite::Result<ReaderAnnotation> {
    ReaderAnnotation::from_row(row.get(0)?, ContentHash::new(&row.get::<_, String>(1)?), row.get(2)?, row.get(3)?, row.get(4)?, &row.get::<_, Vec<u8>>(5)?).map_err(|error| rusqlite::Error::FromSqlConversionFailure(5, Type::Blob, error))
}

include_sql!("src/annotations/sql/schema.sql");
include_sql!("src/annotations/sql/annotations.sql");
