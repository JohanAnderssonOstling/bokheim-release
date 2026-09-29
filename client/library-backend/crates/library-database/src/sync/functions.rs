//! SQLite scalar functions owned by the sync module.

use include_sqlite_sql::include_sql;

use book_model::AgentId;
use book_model::AnnotationState;
use book_model::BookFormat;
use book_model::BookMetadata;
use book_model::Contributor;
use library_replica::BookLifecycleState;
use library_replica::DirectoryLifecycleState;
use library_replica::MutationBody;
use library_replica::ReadingPositionState;
use library_replica::SyncBookMetadata;
use rusqlite::Connection;
use rusqlite::functions::Context as FunctionContext;
use rusqlite::functions::FunctionFlags;
use sync_common::ContentHash as SyncContentHash;
use sync_common::UnixMillis;
use uuid::Uuid;

pub(crate) fn function_error(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::UserFunctionError(Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())))
}

pub(crate) fn function_timestamp(context: &FunctionContext<'_>, index: usize) -> rusqlite::Result<UnixMillis> {
    let value = context.get::<i64>(index)?;
    u64::try_from(value).map_err(|_| function_error("negative mutation timestamp"))
}

pub(crate) fn function_book_format(context: &FunctionContext<'_>, index: usize) -> rusqlite::Result<BookFormat> {
    let raw = context.get::<String>(index)?;
    BookFormat::from_extension(&raw).ok_or_else(|| function_error(format!("book format '{raw}' is not a supported book format")))
}

pub(crate) fn function_content_hash(context: &FunctionContext<'_>, index: usize) -> rusqlite::Result<SyncContentHash> {
    Ok(SyncContentHash::new(&context.get::<String>(index)?))
}

pub(crate) fn function_contributors(context: &FunctionContext<'_>, index: usize) -> rusqlite::Result<Vec<Contributor>> {
    let bytes = context.get::<Vec<u8>>(index)?;
    sync_common::wire::decode(&bytes, 64 * 1024).map_err(|error| function_error(error.to_string()))
}

pub(crate) fn contributors_for_book(context: &FunctionContext<'_>) -> rusqlite::Result<Vec<u8>> {
    let book_row_id = context.get::<i64>(0)?;
    // SAFETY: `contributors_for_book` is only invoked from within triggers
    // firing on the same connection that owns this function registration;
    // sqlite guarantees the context's db handle is that connection while the
    // triggering statement is executing.
    let connection = unsafe { context.get_connection()? };
    let mut contributors = Vec::new();
    (&*connection).function_contributors(book_row_id, |row| {
        let author_identity_id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        let role_bytes: Vec<u8> = row.get(2)?;
        let role_array: [u8; 3] = role_bytes.as_slice().try_into().map_err(|_| function_error("stored contributor role must be exactly 3 bytes"))?;
        let role = book_model::MarcRelatorCode(role_array);
        let mut stable_id = None;
        (&*connection).function_author_stable_id(author_identity_id, |identity_row| {
            stable_id = Some(identity_row.get::<_, String>(0)?);
            Ok(())
        })?;
        let stable_id = stable_id.ok_or_else(|| function_error("contributor author identity disappeared"))?;
        let contributor_id = AgentId::parse_str(&stable_id).map_err(|error| function_error(error.to_string()))?;
        contributors.push(Contributor::with_id(contributor_id, name, role).map_err(|error| function_error(error.to_string()))?);
        Ok(())
    })?;
    sync_common::wire::encode(&contributors).map_err(|error| function_error(error.to_string()))
}

pub(crate) fn function_book(context: &FunctionContext<'_>, index: usize) -> rusqlite::Result<BookMetadata> {
    let bytes = context.get::<Vec<u8>>(index)?;
    if bytes.is_empty() {
        return Ok(BookMetadata::default());
    }
    sync_common::wire::decode(&bytes, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(|error| function_error(error.to_string()))
}

pub(crate) fn encode_trigger_mutation(context: &FunctionContext<'_>) -> rusqlite::Result<Vec<u8>> {
    let kind = context.get::<String>(0)?;
    let body = match kind.as_str() {
        "directory_name" if context.len() == 3 => {
            let dir_id = function_uuid(context, 1)?;
            let name = context.get::<String>(2)?;
            MutationBody::DirectoryName { dir_id, name }
        }
        "directory_parent" if context.len() == 3 => {
            let dir_id = function_uuid(context, 1)?;
            let parent_id = function_uuid(context, 2)?;
            MutationBody::DirectoryParent { dir_id, parent_id }
        }
        "directory_lifecycle" if context.len() == 3 => {
            let dir_id = function_uuid(context, 1)?;
            let value = match context.get::<i64>(2)? {
                0 => DirectoryLifecycleState::Present,
                1 => DirectoryLifecycleState::Deleted,
                2 => DirectoryLifecycleState::Purged,
                _ => return Err(function_error("invalid directory lifecycle state")),
            };
            MutationBody::DirectoryLifecycle { dir_id, value }
        }
        "book_lifecycle" if context.len() == 6 => {
            let content_hash = function_content_hash(context, 1)?;
            let value = match context.get::<i64>(2)? {
                0 => BookLifecycleState::Present,
                1 => BookLifecycleState::Deleted { origin_folder_id: context.get(5)? },
                2 => BookLifecycleState::Purged,
                _ => return Err(function_error("invalid book lifecycle state")),
            };
            MutationBody::BookLifecycle { content_hash, value }
        }
        "book_facts" if context.len() == 4 => {
            MutationBody::BookFacts { content_hash: function_content_hash(context, 1)?, value: library_replica::BookFacts { added_at: function_timestamp(context, 2)?, format: function_book_format(context, 3)? } }
        }
        "metadata" if context.len() == 6 => MutationBody::Metadata {
            content_hash: function_content_hash(context, 1)?,
            value: SyncBookMetadata { title: context.get(2)?, subtitle: context.get(3)?, contributors: function_contributors(context, 4)?, book: function_book(context, 5)? },
        },
        "pdf_reader_metadata" if context.len() == 3 => {
            let bytes: Vec<u8> = context.get(2)?;
            let value = serde_json::from_slice(&bytes).map_err(|_| function_error("invalid PDF reader metadata"))?;
            MutationBody::PdfReaderMetadata { content_hash: function_content_hash(context, 1)?, value }
        }
        "book_toc" if context.len() == 5 => {
            let toc_json: String = context.get(2)?;
            let entries = serde_json::from_str(&toc_json).map_err(|_| function_error("invalid navigation document"))?;
            // A negative duration is the trigger's spelling of unknown.
            let duration_ms = context.get::<i64>(3).map(|duration| (duration >= 0).then_some(duration as u64))?;
            let tracks = context.get::<Option<String>>(4)?.map(|json| serde_json::from_str(&json).map_err(|_| function_error("invalid audiobook track index"))).transpose()?;
            MutationBody::BookToc { content_hash: function_content_hash(context, 1)?, value: book_model::BookTocDocument { entries, duration_ms, tracks } }
        }
        "description" if context.len() == 3 => MutationBody::Description { content_hash: function_content_hash(context, 1)?, value: context.get(2)? },
        "placement" if context.len() == 5 => {
            let dir_id = function_uuid(context, 1)?;
            let content_hash = function_content_hash(context, 2)?;
            let present = context.get(3)?;
            MutationBody::Placement { dir_id, content_hash, present, origin_folder_id: context.get(4)? }
        }
        "reading_position" if context.len() == 4 => MutationBody::ReadingPosition {
            content_hash: function_content_hash(context, 1)?,
            value: ReadingPositionState { location: book_model::ReadingPosition::parse(&context.get::<String>(2)?).map_err(|error| function_error(error.to_string()))?, progress: context.get(3)? },
        },
        "annotation" if context.len() == 6 => {
            let detail: Vec<u8> = context.get(3)?;
            let modified_at = function_timestamp(context, 4)?;
            let deleted: bool = context.get(5)?;
            let modified_at = i64::try_from(modified_at).map_err(|_| function_error("invalid annotation timestamp"))?;
            let value = AnnotationState::from_detail(function_content_hash(context, 2)?, &detail, modified_at, deleted).map_err(|error| function_error(error.to_string()))?;
            MutationBody::Annotation { annotation_id: context.get(1)?, value }
        }
        _ => return Err(function_error(format!("invalid {kind} trigger mutation arguments"))),
    };
    sync_common::wire::encode(&body).map_err(|error| function_error(error.to_string()))
}

pub(crate) fn register_sync_functions(conn: &Connection) -> rusqlite::Result<()> {
    // metadata_batch_flag is registered once by Database::open with the same
    // Arc used by the RAII batching guard. Re-registering it here disconnects
    // the SQL triggers from that guard during connection/schema initialization.
    conn.create_scalar_function("bokheim_accept_local", 2, FunctionFlags::SQLITE_UTF8, crate::sync::apply::registers::accept_local)?;
    conn.create_scalar_function("bokheim_mutation", -1, FunctionFlags::SQLITE_UTF8, encode_trigger_mutation)?;
    conn.create_scalar_function("normalize_search_text", 1, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |context| Ok(book_model::normalize_search_text(&context.get::<String>(0)?)))?;
    conn.create_scalar_function("portable_name_key", 1, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |context| Ok(library_replica::portable_name_key(&context.get::<String>(0)?)))?;
    // Not SQLITE_DETERMINISTIC: this function re-enters the connection to
    // query live book_contributor state, so its result depends on database
    // contents, not solely its arguments.
    conn.create_scalar_function("contributors_for_book", 1, FunctionFlags::SQLITE_UTF8, |context| contributors_for_book(context))
}

pub(crate) fn function_uuid(context: &FunctionContext<'_>, index: usize) -> rusqlite::Result<Uuid> {
    let value = context.get::<String>(index)?;
    Uuid::parse_str(&value).map_err(|error| function_error(error.to_string()))
}

include_sql!("src/sync/sql/sync_functions.sql");
