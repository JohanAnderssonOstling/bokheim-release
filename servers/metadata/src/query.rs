use super::{error, Connection, Duration, Instant, MetadataError, MetadataService, Pool, QueryConnection, SqliteConnectionManager, QUERY_EXECUTION_TIMEOUT, QUERY_PROGRESS_OPS};

pub(crate) fn query_connection(pool: &Pool<SqliteConnectionManager>) -> Result<QueryConnection, MetadataError> {
    query_connection_with_timeout(pool, QUERY_EXECUTION_TIMEOUT)
}

pub(crate) fn query_connection_with_timeout(pool: &Pool<SqliteConnectionManager>, timeout: Duration) -> Result<QueryConnection, MetadataError> {
    let connection = pool.get().map_err(error)?;
    install_query_deadline(&connection, timeout);
    Ok(connection)
}

pub(crate) fn install_query_deadline(connection: &Connection, timeout: Duration) {
    let started = Instant::now();
    let _ = connection.progress_handler(QUERY_PROGRESS_OPS, Some(move || started.elapsed() >= timeout));
}

pub(crate) async fn run_database<T, F>(service: MetadataService, operation: F) -> Result<Result<T, MetadataError>, tokio::task::JoinError>
where
    T: Send + 'static,
    F: FnOnce(&MetadataService) -> Result<T, MetadataError> + Send + 'static,
{
    let permit = match service.state.query_slots.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return Ok(Err(MetadataError("metadata query scheduler stopped".to_owned()))),
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation(&service)
    })
    .await
}
