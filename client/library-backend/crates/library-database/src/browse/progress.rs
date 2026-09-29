//! Project reading progress for browse views.
use rusqlite::{Connection, functions::FunctionFlags};

pub(crate) fn register_progress_function(connection: &Connection) -> rusqlite::Result<()> {
    connection.create_scalar_function("browse_reading_progress", 1, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |context| {
        let progress = context.get::<Option<f64>>(0)?.unwrap_or_default();
        Ok(progress.max(0.0))
    })
}
