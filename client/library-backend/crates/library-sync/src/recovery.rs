//! Paged cursor recovery shared by native and browser storage owners.
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
        #[cfg(target_arch = "wasm32")]
        client_platform_runtime::executor::sleep(std::time::Duration::from_millis(1)).await.map_err(DatabaseError::operation)?;
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "recovery/tests.rs"]
mod tests;
