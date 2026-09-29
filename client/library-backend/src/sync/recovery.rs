//! Cursor-recovery sequencing for browser storage commands.
use library_database::{Database, DatabaseError};

pub(crate) async fn complete(database: &Database) -> Result<usize, DatabaseError> {
    let mut after = None;
    let mut inserted = 0;
    loop {
        let (next, count) = database.sync_recover_state_page(after.as_ref())?;
        inserted += count;
        if next.is_none() {
            return Ok(inserted);
        }
        after = next;
        // Each page has committed before yielding so the browser event loop
        // stays responsive through long recoveries.
        crate::executor::sleep(std::time::Duration::from_millis(1)).await;
    }
}
