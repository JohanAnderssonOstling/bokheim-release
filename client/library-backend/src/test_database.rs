//! Test fixtures use the same concrete owner as production.
pub(crate) type TestDatabase = library_database::Database;

pub(crate) fn raw(database: &library_database::Database) -> library_database::rusqlite::Connection {
    library_database::open_fixture_connection(database.path()).expect("open fixture SQL connection")
}
