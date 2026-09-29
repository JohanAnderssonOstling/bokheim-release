//! Browser startup database initialization and taxonomy reprojection.

pub fn initialize_database(locator: &str) -> Result<(), crate::BackendError> {
    let db = library_database::Database::open(locator).map_err(crate::BackendError::operation)?;
    db.initialize_library().map(|_| ()).map_err(crate::BackendError::operation)
}
