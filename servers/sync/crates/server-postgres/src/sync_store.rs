use crate::sync::LibraryNameOutcome;
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use sync_common::StateCell;
use sync_common::api::libraries::{LibraryNameRequest, LibrarySummary};
use sync_common::{ContentHash, LibraryId, LibraryRevision, MAX_PULL_CHANGES, MutationId, MutationRejection, MutationRejectionReason, ReplicaId, ReplicaSeq, ServerMutation, SyncCursor, UnixMillis, VersionKey, WireMutation};

const FETCH_PAGE_SQL: &str = include_str!("sql/sync/fetch_page.sql");
const FETCH_READING_POSITION_PAGE_SQL: &str = include_str!("sql/sync/fetch_reading_position_page.sql");

pub(super) async fn list_libraries(pool: &PgPool, user_id: &str) -> Result<Vec<LibrarySummary>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, String)>(include_str!("sql/sync/list_libraries.sql")).bind(user_id).fetch_all(pool).await?;
    rows.into_iter().map(|(id, name)| Ok(LibrarySummary { library_id: LibraryId::parse_str(&id).map_err(decode_err)?, library_name: name })).collect()
}

pub(super) async fn create_library(pool: &PgPool, user_id: &str, request: &LibraryNameRequest) -> Result<Option<LibraryNameOutcome>, sqlx::Error> {
    let library_id = request.library_id.to_string();
    let row = sqlx::query_as::<_, (String, String, bool)>(include_str!("sql/sync/create_library.sql")).bind(library_id).bind(user_id).bind(&request.library_name).fetch_optional(pool).await?;
    row.map(|(id, name, changed)| Ok(LibraryNameOutcome { library: LibrarySummary { library_id: LibraryId::parse_str(&id).map_err(decode_err)?, library_name: name }, changed })).transpose()
}

pub(super) async fn rename_library(pool: &PgPool, user_id: &str, request: &LibraryNameRequest) -> Result<Option<LibraryNameOutcome>, sqlx::Error> {
    let library_id = request.library_id.to_string();
    let row = sqlx::query_as::<_, (String, String, bool)>(include_str!("sql/sync/rename_library.sql")).bind(library_id).bind(user_id).bind(&request.library_name).fetch_optional(pool).await?;
    row.map(|(id, name, changed)| Ok(LibraryNameOutcome { library: LibrarySummary { library_id: LibraryId::parse_str(&id).map_err(decode_err)?, library_name: name }, changed })).transpose()
}

pub(super) async fn owns_library(pool: &PgPool, user_id: &str, library_id: &LibraryId) -> Result<bool, sqlx::Error> {
    let library_id = library_id.to_string();
    Ok(sqlx::query_scalar::<_, bool>(include_str!("sql/sync/owns_library.sql")).bind(library_id).bind(user_id).fetch_one(pool).await?)
}

pub(super) async fn missing_state_cells(pool: &PgPool, library_id: &LibraryId, cells: &[StateCell]) -> Result<Vec<StateCell>, sqlx::Error> {
    if cells.is_empty() {
        return Ok(Vec::new());
    }
    let kinds = cells.iter().map(|cell| cell.kind.clone()).collect::<Vec<_>>();
    let keys = cells.iter().map(|cell| cell.entity_key.clone()).collect::<Vec<_>>();
    let subkeys = cells.iter().map(|cell| cell.entity_subkey.clone()).collect::<Vec<_>>();
    let rows = sqlx::query(
        "WITH requested(kind, entity_key, entity_subkey) AS (
             SELECT * FROM UNNEST($2::text[], $3::text[], $4::text[])
         )
         SELECT requested.kind, requested.entity_key, requested.entity_subkey
         FROM requested
         WHERE NOT EXISTS (
             SELECT 1 FROM sync_state
             WHERE library_id=$1 AND sync_state.kind=requested.kind
               AND sync_state.entity_key=requested.entity_key
               AND sync_state.entity_subkey=requested.entity_subkey
         ) AND NOT EXISTS (
             SELECT 1 FROM sync_reading_state
             WHERE library_id=$1 AND sync_reading_state.kind=requested.kind
               AND sync_reading_state.entity_key=requested.entity_key
               AND sync_reading_state.entity_subkey=requested.entity_subkey
         )",
    )
    .bind(library_id.to_string())
    .bind(kinds)
    .bind(keys)
    .bind(subkeys)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(|row| Ok(StateCell { kind: row.try_get("kind")?, entity_key: row.try_get("entity_key")?, entity_subkey: row.try_get("entity_subkey")? })).collect()
}

pub(super) struct InsertChangesResult {
    pub accepted: Vec<MutationId>,
    pub rejected: Vec<MutationRejection>,
}

pub(super) fn server_now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_millis() as u64).unwrap_or(0)
}

pub(super) fn storage_rejection(change: &WireMutation, server_now_ms: u64) -> Option<MutationRejectionReason> {
    if change.changed_at > server_now_ms.saturating_add(sync_common::MAX_LWW_FUTURE_SKEW_MS) {
        return Some(MutationRejectionReason::TimestampTooFarFuture);
    }
    // The service never decodes `value`, so a lifecycle mutation's declared
    // reference is its only statement about whether the book's bytes are still
    // held. Admitting one without it stores an undeclared presence that the
    // quota ledger cannot interpret. Declaring `present: false` is how a client
    // says "no blob", so requiring the field costs it nothing.
    if change.kind == sync_common::mutation_kind::BOOK_LIFECYCLE && change.blob_reference.is_none() {
        return Some(MutationRejectionReason::InvalidPayload);
    }
    None
}

#[derive(Debug)]
struct StoredLifecycleReference {
    /// `None` is an undeclared presence, not corruption: rows written before a
    /// lifecycle declaration was mandatory have it. It must decode, or that one
    /// cell answers every later exchange for its library with an internal
    /// error. Absent means the cell holds no reference to release.
    present: Option<bool>,
    content_hash: Option<String>,
    changed_at: i64,
    version_rank: i16,
    replica_id: String,
    replica_seq: i64,
    event_id: String,
}

fn decode_lifecycle_reference(row: PgRow) -> Result<StoredLifecycleReference, sqlx::Error> {
    Ok(StoredLifecycleReference {
        present: row.try_get("present").map_err(decode_err)?,
        content_hash: row.try_get("content_hash").map_err(decode_err)?,
        changed_at: row.try_get("changed_at").map_err(decode_err)?,
        version_rank: row.try_get("version_rank").map_err(decode_err)?,
        replica_id: row.try_get("replica_id").map_err(decode_err)?,
        replica_seq: row.try_get("replica_seq").map_err(decode_err)?,
        event_id: row.try_get("event_id").map_err(decode_err)?,
    })
}

/// All writes share the library lock held by the caller. Resolve lifecycle
/// first, remove retired fields, then admit ordinary updates against that final
/// existence state. Ignored updates are acknowledged so offline clients drain.
pub(super) async fn insert_changes(tx: &mut Transaction<'_, Postgres>, user_id: &str, library_id: &LibraryId, replica_id: &ReplicaId, changes: &[WireMutation]) -> Result<InsertChangesResult, sqlx::Error> {
    let lifecycles = changes.iter().filter(|c| c.kind == sync_common::mutation_kind::BOOK_LIFECYCLE).cloned().collect::<Vec<_>>();
    let mut result = insert_admitted_changes(tx, user_id, library_id, replica_id, &lifecycles).await?;
    // Purge owns both the cold fields and the independent reading channel.
    // A stale purge does nothing because these queries inspect the LWW winner.
    for table in ["sync_state", "sync_reading_state"] {
        sqlx::query(&format!(
            "DELETE FROM {table} field USING sync_state lifecycle
             WHERE lifecycle.library_id=$1 AND lifecycle.kind='book_lifecycle' AND lifecycle.entity_subkey=''
               AND lifecycle.entity_key=ANY($2) AND lifecycle.present IS FALSE
               AND field.library_id=lifecycle.library_id
               AND (CASE WHEN field.kind='annotation' THEN field.content_hash
                         WHEN field.kind IN ('book_facts','placement','reading_position','metadata','description','pdf_reader_metadata','book_toc') THEN field.entity_key END)=lifecycle.entity_key"
        ))
        .bind(library_id.to_string())
        .bind(lifecycles.iter().map(|c| c.entity_key.clone()).collect::<Vec<_>>())
        .execute(&mut **tx)
        .await?;
    }
    // A losing purge has already removed the sender's entire local book.
    // Redeliver all surviving fields, even those behind its cursor. Preserve
    // the canonical value/version; only delivery revisions advance. The caller
    // still holds the library lock, including across both channel updates.
    let purged = lifecycles.iter().filter(|c| c.blob_reference.as_ref().is_some_and(|r| !r.present) && result.accepted.contains(&c.mutation_id)).map(|c| c.entity_key.clone()).collect::<Vec<_>>();
    if !purged.is_empty() {
        for table in ["sync_state", "sync_reading_state"] {
            sqlx::query(&format!(
                "UPDATE {table} field SET server_seq=nextval('sync_server_sequence')
                 FROM sync_state lifecycle
                 WHERE lifecycle.library_id=$1 AND lifecycle.kind='book_lifecycle' AND lifecycle.entity_subkey=''
                   AND lifecycle.entity_key=ANY($2) AND lifecycle.present IS TRUE
                   AND field.library_id=lifecycle.library_id
                   AND (CASE WHEN field.kind='annotation' THEN field.content_hash
                             WHEN field.kind IN ('book_facts','placement','reading_position','metadata','description','pdf_reader_metadata','book_toc') THEN field.entity_key END)=lifecycle.entity_key"
            ))
            .bind(library_id.to_string())
            .bind(&purged)
            .execute(&mut **tx)
            .await?;
        }
    }
    let mut fields = Vec::new();
    let owners = changes.iter().filter_map(|c| field_owner(c).ok().flatten().map(str::to_owned)).collect::<Vec<_>>();
    let existing: std::collections::HashSet<String> = sqlx::query_scalar::<_, String>("SELECT entity_key FROM sync_state WHERE library_id=$1 AND kind='book_lifecycle' AND entity_subkey='' AND present IS TRUE AND entity_key=ANY($2)")
        .bind(library_id.to_string())
        .bind(owners)
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .collect();
    for change in changes.iter().filter(|c| c.kind != sync_common::mutation_kind::BOOK_LIFECYCLE) {
        if let Some(reason) = storage_rejection(change, server_now_ms()) {
            result.rejected.push(MutationRejection { mutation_id: change.mutation_id, reason });
            continue;
        }
        let owner = match field_owner(change) {
            Ok(owner) => owner,
            Err(reason) => {
                result.rejected.push(MutationRejection { mutation_id: change.mutation_id, reason });
                continue;
            }
        };
        if owner.is_some_and(|hash| !existing.contains(hash)) {
            let failed_creation = lifecycles.iter().filter(|c| Some(c.entity_key.as_str()) == owner).find_map(|c| result.rejected.iter().find(|r| r.mutation_id == c.mutation_id).map(|r| r.reason.clone()));
            if let Some(reason) = failed_creation {
                result.rejected.push(MutationRejection { mutation_id: change.mutation_id, reason });
            } else {
                result.accepted.push(change.mutation_id);
            }
        } else {
            fields.push(change.clone());
        }
    }
    let inserted = insert_admitted_changes(tx, user_id, library_id, replica_id, &fields).await?;
    result.accepted.extend(inserted.accepted);
    result.rejected.extend(inserted.rejected);
    let accepted = result.accepted.into_iter().collect::<std::collections::HashSet<_>>();
    result.accepted = changes.iter().filter(|c| accepted.contains(&c.mutation_id)).map(|c| c.mutation_id).collect();
    Ok(result)
}

/// Annotation identity is independent of its book hash, including on deletion.
fn field_owner(change: &WireMutation) -> Result<Option<&str>, MutationRejectionReason> {
    let owner = change.book_field_owner();
    if change.kind == sync_common::mutation_kind::ANNOTATION && owner.is_none() {
        return Err(MutationRejectionReason::InvalidPayload);
    }
    Ok(owner)
}

/// Stores opaque wire rows. The only routing knowledge is that reading state
/// belongs to the hot table; cold kinds need no server-side enum arm.
async fn insert_admitted_changes(tx: &mut Transaction<'_, Postgres>, user_id: &str, library_id: &LibraryId, replica_id: &ReplicaId, changes: &[WireMutation]) -> Result<InsertChangesResult, sqlx::Error> {
    if changes.is_empty() {
        return Ok(InsertChangesResult { accepted: Vec::new(), rejected: Vec::new() });
    }
    let now = server_now_ms();
    let (accepted, rejected): (Vec<_>, Vec<_>) = changes.iter().partition(|change| storage_rejection(change, now).is_none());
    let mut rejected = rejected.into_iter().map(|change| MutationRejection { mutation_id: change.mutation_id, reason: storage_rejection(change, now).expect("partitioned rejection") }).collect::<Vec<_>>();
    let dependencies = accepted.iter().map(|change| crate::book_admission::dependency(change)).collect::<Vec<_>>();
    let hashes = dependencies.iter().filter_map(|d| d.as_ref().ok().and_then(|h| h.as_ref()).map(ToString::to_string)).collect::<Vec<_>>();
    let available = crate::book_admission::available(tx, user_id, library_id, &hashes).await?;
    let accepted = accepted
        .into_iter()
        .zip(dependencies)
        .filter_map(|(change, dependency)| {
            let reason = match dependency {
                Err(reason) => Some(reason),
                Ok(Some(hash)) if !available.contains(hash.as_str()) => Some(MutationRejectionReason::MissingDependency),
                _ => None,
            };
            if let Some(reason) = reason {
                rejected.push(MutationRejection { mutation_id: change.mutation_id, reason });
                None
            } else {
                Some(change)
            }
        })
        .collect::<Vec<_>>();

    // The server reads only declared lifecycle facts. It never decodes the
    // opaque value to maintain quota references.
    let mut lifecycle_deltas = Vec::new();
    let mut lifecycle_winners = HashMap::<(&str, &str, &str), &WireMutation>::new();
    for change in accepted.iter().copied().filter(|change| change.kind == sync_common::mutation_kind::BOOK_LIFECYCLE) {
        let cell = (change.kind.as_str(), change.entity_key.as_str(), change.entity_subkey.as_str());
        lifecycle_winners
            .entry(cell)
            .and_modify(|winner| {
                if VersionKey::from_wire(change, *replica_id) > VersionKey::from_wire(winner, *replica_id) {
                    *winner = change;
                }
            })
            .or_insert(change);
    }
    for change in lifecycle_winners.into_values() {
        let existing = sqlx::query(include_str!("sql/sync/get_lifecycle_reference_state.sql"))
            .bind(library_id.to_string())
            .bind(&change.entity_key)
            .bind(&change.entity_subkey)
            .fetch_optional(&mut **tx)
            .await?
            .map(decode_lifecycle_reference)
            .transpose()?;
        let incoming = VersionKey::from_wire(change, *replica_id);
        let wins = match existing.as_ref() {
            None => true,
            Some(stored) => {
                let stored_replica = ReplicaId::parse_str(&stored.replica_id).map_err(decode_err)?;
                let stored_mutation = MutationId::parse(&stored.event_id).map_err(decode_err)?;
                let stored_changed_at = u64::try_from(stored.changed_at).map_err(decode_err)?;
                let stored_rank = u8::try_from(stored.version_rank).map_err(decode_err)?;
                let stored_seq = u64::try_from(stored.replica_seq).map_err(decode_err)?;
                incoming > VersionKey::new(stored_changed_at, stored_rank, stored_replica, stored_seq, stored_mutation)
            }
        };
        if !wins {
            continue;
        }
        if let Some(hash) = existing.as_ref().filter(|stored| stored.present == Some(true)).and_then(|stored| stored.content_hash.as_ref()) {
            lifecycle_deltas.push((ContentHash::new(hash), -1));
            if !change.blob_reference.as_ref().is_some_and(|reference| reference.present && reference.content_hash.as_ref().is_some_and(|next| next.as_str() == hash)) {
                sqlx::query(include_str!("sql/cloud_storage/delete_claim.sql")).bind(library_id.to_string()).bind(hash).execute(&mut **tx).await?;
            }
        }
        if let Some(hash) = change.blob_reference.as_ref().filter(|reference| reference.present).and_then(|reference| reference.content_hash.clone()) {
            lifecycle_deltas.push((hash, 1));
        }
    }
    let cloud_enabled: bool = sqlx::query_scalar(include_str!("sql/cloud_storage/enabled.sql")).bind(library_id.to_string()).fetch_one(&mut **tx).await?;
    if cloud_enabled {
        crate::assets::apply_lifecycle_reference_deltas(tx, user_id, lifecycle_deltas).await?;
    }

    for (table, rows) in [
        ("sync_state", accepted.iter().copied().filter(|change| change.kind != sync_common::mutation_kind::READING_POSITION).collect::<Vec<_>>()),
        ("sync_reading_state", accepted.iter().copied().filter(|change| change.kind == sync_common::mutation_kind::READING_POSITION).collect::<Vec<_>>()),
    ] {
        if rows.is_empty() {
            continue;
        }
        let kinds = rows.iter().map(|change| change.kind.clone()).collect::<Vec<_>>();
        let keys = rows.iter().map(|change| change.entity_key.clone()).collect::<Vec<_>>();
        let subkeys = rows.iter().map(|change| change.entity_subkey.clone()).collect::<Vec<_>>();
        let values = rows.iter().map(|change| change.value.clone()).collect::<Vec<_>>();
        let present = rows.iter().map(|change| change.blob_reference.as_ref().map(|reference| reference.present)).collect::<Vec<_>>();
        let hashes = rows.iter().map(|change| change.blob_reference.as_ref().and_then(|reference| reference.content_hash.as_ref()).map(|hash| hash.as_str().to_owned())).collect::<Vec<_>>();
        let changed = rows.iter().map(|change| i64::try_from(change.changed_at)).collect::<Result<Vec<_>, _>>().map_err(decode_err)?;
        let versions = rows.iter().map(|change| VersionKey::from_wire(change, *replica_id)).collect::<Vec<_>>();
        let sequences = versions.iter().map(|version| i64::try_from(version.replica_seq)).collect::<Result<Vec<_>, _>>().map_err(decode_err)?;
        let ranks = rows.iter().map(|change| change.conflict_rank as i16).collect::<Vec<_>>();
        let events = versions.iter().map(|version| version.mutation_id.to_string()).collect::<Vec<_>>();
        let replicas = versions.iter().map(|version| version.replica_id.to_string()).collect::<Vec<_>>();
        let statement = include_str!("sql/sync/upsert_state_batch.sql").replace("{{TABLE}}", table);
        sqlx::query(&statement).bind(library_id.to_string()).bind(kinds).bind(keys).bind(subkeys).bind(values).bind(present).bind(hashes).bind(changed).bind(sequences).bind(ranks).bind(events).bind(replicas).execute(&mut **tx).await?;
    }
    Ok(InsertChangesResult { accepted: accepted.into_iter().map(|change| change.mutation_id).collect(), rejected })
}

#[derive(Debug, thiserror::Error)]
pub(super) enum FetchChangesError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("client cursor is ahead of server state")]
    CursorAhead,
    #[error("corrupt stored state at revision {revision}: {reason}")]
    CorruptStoredEvent { revision: i64, reason: &'static str },
}
pub(super) struct FetchChangesPage {
    pub book_creations: Vec<ServerMutation>,
    pub changes: Vec<ServerMutation>,
    pub next_cursor: SyncCursor,
    pub has_more: bool,
}
pub(super) enum FetchChangesResult {
    Unauthorized,
    Page(FetchChangesPage),
}

fn decode_err<E: std::error::Error + Send + Sync + 'static>(error: E) -> sqlx::Error {
    sqlx::Error::Decode(Box::new(error))
}

fn millis(value: i64) -> Result<UnixMillis, sqlx::Error> {
    u64::try_from(value).map_err(|_| sqlx::Error::Decode("negative Unix-millisecond timestamp".into()))
}

fn revision(value: Option<LibraryRevision>) -> i64 {
    value.map(|revision| i64::try_from(revision.get()).unwrap_or(i64::MAX)).unwrap_or(0)
}

#[derive(Clone, Copy, Debug)]
struct PullHeads {
    reading: i64,
    state: i64,
}

fn cursor_is_ahead(cursor: SyncCursor, heads: PullHeads) -> bool {
    revision(cursor.reading_revision) > heads.reading || revision(cursor.state_revision) > heads.state
}

fn protocol_revision(value: i64) -> Result<Option<LibraryRevision>, FetchChangesError> {
    if value == 0 {
        Ok(None)
    } else {
        LibraryRevision::new(u64::try_from(value).map_err(|_| FetchChangesError::CorruptStoredEvent { revision: value, reason: "negative channel revision" })?)
            .map(Some)
            .map_err(|_| FetchChangesError::CorruptStoredEvent { revision: value, reason: "invalid channel revision" })
    }
}

fn head_cursor(heads: PullHeads) -> Result<SyncCursor, FetchChangesError> {
    Ok(SyncCursor { state_revision: protocol_revision(heads.state)?, reading_revision: protocol_revision(heads.reading)? })
}

fn decode_change(row: &PgRow, prefix: &str) -> Result<ServerMutation, FetchChangesError> {
    let raw: i64 = row.try_get(format!("{prefix}server_seq").as_str())?;
    let corrupt = |reason| FetchChangesError::CorruptStoredEvent { revision: raw, reason };
    let changed_at = millis(row.try_get::<i64, _>(format!("{prefix}changed_at").as_str())?).map_err(|_| corrupt("invalid timestamp"))?;
    let replica_id = ReplicaId::parse_str(&row.try_get::<String, _>(format!("{prefix}replica_id").as_str())?).map_err(|_| corrupt("invalid replica identity"))?;
    let replica_seq = ReplicaSeq::new(u64::try_from(row.try_get::<i64, _>(format!("{prefix}replica_seq").as_str())?).map_err(|_| corrupt("negative replica sequence"))?).map_err(|_| corrupt("invalid replica sequence"))?;
    let revision = LibraryRevision::new(u64::try_from(raw).map_err(|_| corrupt("negative library revision"))?).map_err(|_| corrupt("invalid library revision"))?;
    let mutation_id = MutationId::parse(&row.try_get::<String, _>(format!("{prefix}event_id").as_str())?).map_err(|_| corrupt("invalid mutation identity"))?;
    let present: Option<bool> = row.try_get(format!("{prefix}present").as_str())?;
    let content_hash: Option<String> = row.try_get(format!("{prefix}content_hash").as_str())?;
    let blob_reference = match (present, content_hash) {
        (Some(present), content_hash) => Some(sync_common::DeclaredBlobReference { present, content_hash: content_hash.map(|hash| ContentHash::new(&hash)) }),
        (None, None) => None,
        (None, Some(_)) => return Err(corrupt("hash declaration without presence")),
    };
    let mutation = WireMutation {
        origin: None,
        mutation_id,
        kind: row.try_get(format!("{prefix}kind").as_str())?,
        entity_key: row.try_get(format!("{prefix}entity_key").as_str())?,
        entity_subkey: row.try_get(format!("{prefix}entity_subkey").as_str())?,
        value: row.try_get(format!("{prefix}value").as_str())?,
        conflict_rank: u8::try_from(row.try_get::<i16, _>(format!("{prefix}version_rank").as_str())?).map_err(|_| corrupt("invalid conflict rank"))?,
        blob_reference,
        changed_at,
        replica_seq,
    };
    Ok(ServerMutation { mutation, replica_id, revision })
}

async fn fetch_projected_page<'e, E: sqlx::Executor<'e, Database = Postgres>>(executor: E, statement: &str, library: &LibraryId, since: SyncCursor, heads: PullHeads) -> Result<FetchChangesPage, FetchChangesError> {
    let rows =
        sqlx::query(statement).bind(library.to_string()).bind(revision(since.state_revision)).bind(revision(since.reading_revision)).bind(heads.state).bind(heads.reading).bind((MAX_PULL_CHANGES + 1) as i64).fetch_all(executor).await?;
    let has_more = rows.len() > MAX_PULL_CHANGES;
    let mut changes = Vec::new();
    let mut book_creations = Vec::new();
    let mut created = std::collections::HashSet::new();
    for row in rows.into_iter().take(MAX_PULL_CHANGES) {
        if row.try_get::<Option<i64>, _>("creation_server_seq")?.is_some() {
            let creation = decode_change(&row, "creation_")?;
            if created.insert(creation.mutation.entity_key.clone()) {
                book_creations.push(creation);
            }
        }
        changes.push(decode_change(&row, "")?);
    }
    let mut next_cursor = since;
    for change in &changes {
        if change.mutation.kind == sync_common::mutation_kind::READING_POSITION {
            next_cursor.reading_revision = Some(change.revision);
        } else {
            next_cursor.state_revision = Some(change.revision);
        }
    }
    if !has_more {
        next_cursor = head_cursor(heads)?;
    }
    Ok(FetchChangesPage { book_creations, changes, next_cursor, has_more })
}

async fn fetch_page<'e, E: sqlx::Executor<'e, Database = Postgres>>(executor: E, library: &LibraryId, since: SyncCursor, heads: PullHeads) -> Result<FetchChangesPage, FetchChangesError> {
    fetch_projected_page(executor, FETCH_PAGE_SQL, library, since, heads).await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PullRoute {
    Empty,
    ReadingOnly,
    Full,
}

fn pull_route(since: SyncCursor, heads: PullHeads) -> PullRoute {
    let reading_since = revision(since.reading_revision);
    let state_since = revision(since.state_revision);
    if heads.reading <= reading_since && heads.state <= state_since {
        PullRoute::Empty
    } else if heads.state <= state_since {
        PullRoute::ReadingOnly
    } else {
        PullRoute::Full
    }
}

async fn fetch_reading_position_page<'e, E: sqlx::Executor<'e, Database = Postgres>>(executor: E, library: &LibraryId, since: SyncCursor, heads: PullHeads) -> Result<FetchChangesPage, FetchChangesError> {
    fetch_projected_page(executor, FETCH_READING_POSITION_PAGE_SQL, library, since, heads).await
}

pub(super) async fn fetch_changes(pool: &PgPool, user: &str, library: &LibraryId, _replica_id: &ReplicaId, since: SyncCursor) -> Result<FetchChangesResult, FetchChangesError> {
    let library_str = library.to_string();
    let heads = sqlx::query_as::<_, (i64, i64)>(include_str!("sql/sync/get_pull_heads.sql")).bind(library_str).bind(user).fetch_optional(pool).await?;
    let Some((reading_revision, other_revision)) = heads else { return Ok(FetchChangesResult::Unauthorized) };
    let heads = PullHeads { reading: reading_revision, state: other_revision };
    if cursor_is_ahead(since, heads) {
        return Err(FetchChangesError::CursorAhead);
    }
    let page = match pull_route(since, heads) {
        PullRoute::Empty => FetchChangesPage { book_creations: Vec::new(), changes: Vec::new(), next_cursor: head_cursor(heads)?, has_more: false },
        PullRoute::ReadingOnly => fetch_reading_position_page(pool, library, since, heads).await?,
        PullRoute::Full => fetch_page(pool, library, since, heads).await?,
    };
    Ok(FetchChangesResult::Page(page))
}

pub(super) async fn validate_owned_cursor(tx: &mut Transaction<'_, Postgres>, library: &LibraryId, since: SyncCursor) -> Result<(), FetchChangesError> {
    let library_str = library.to_string();
    let (reading, state) = sqlx::query_as(include_str!("sql/sync/validate_owned_cursor.sql")).bind(library_str).fetch_one(&mut **tx).await?;
    if cursor_is_ahead(since, PullHeads { reading, state }) {
        return Err(FetchChangesError::CursorAhead);
    }
    Ok(())
}

pub(super) async fn fetch_owned_changes(tx: &mut Transaction<'_, Postgres>, library: &LibraryId, since: SyncCursor) -> Result<FetchChangesPage, FetchChangesError> {
    let (reading, state) = sqlx::query_as(include_str!("sql/sync/validate_owned_cursor.sql")).bind(library.to_string()).fetch_one(&mut **tx).await?;
    let heads = PullHeads { reading, state };
    match pull_route(since, heads) {
        PullRoute::Empty => Ok(FetchChangesPage { book_creations: Vec::new(), changes: Vec::new(), next_cursor: head_cursor(heads)?, has_more: false }),
        PullRoute::ReadingOnly => fetch_reading_position_page(&mut **tx, library, since, heads).await,
        PullRoute::Full => fetch_page(&mut **tx, library, since, heads).await,
    }
}

#[cfg(test)]
mod tests {
    use super::{PullHeads, PullRoute, cursor_is_ahead, decode_lifecycle_reference, missing_state_cells, pull_route, storage_rejection};
    use sync_common::{LibraryRevision, MAX_LWW_FUTURE_SKEW_MS, MutationId, MutationRejectionReason, ReplicaSeq, StateCell, SyncCursor, WireMutation};

    fn cursor(state: u64, reading: u64) -> SyncCursor {
        SyncCursor { state_revision: LibraryRevision::new(state).ok(), reading_revision: LibraryRevision::new(reading).ok() }
    }

    fn heads(state: i64, reading: i64) -> PullHeads {
        PullHeads { state, reading }
    }

    fn reading_mutation(changed_at: u64) -> WireMutation {
        WireMutation {
            origin: None,
            mutation_id: MutationId::new(),
            kind: "reading_position".to_owned(),
            entity_key: "1".to_owned(),
            entity_subkey: String::new(),
            value: vec![1, 2, 3],
            conflict_rank: 0,
            blob_reference: None,
            changed_at,
            replica_seq: ReplicaSeq::new(1).unwrap(),
        }
    }

    #[test]
    fn pull_routing_advances_channels_independently() {
        assert_eq!(pull_route(cursor(10, 10), heads(10, 10)), PullRoute::Empty);
        assert_eq!(pull_route(cursor(10, 10), heads(10, 11)), PullRoute::ReadingOnly);
        assert_eq!(pull_route(cursor(10, 10), heads(11, 10)), PullRoute::Full);
        assert_eq!(pull_route(cursor(10, 12), heads(11, 12)), PullRoute::Full);
        assert_eq!(pull_route(SyncCursor::default(), heads(0, 1)), PullRoute::ReadingOnly);
        assert_eq!(pull_route(SyncCursor::default(), heads(1, 0)), PullRoute::Full);
        assert_eq!(pull_route(cursor(10, 22), heads(21, 22)), PullRoute::Full, "advancing reading must not consume unseen state");
    }

    #[test]
    fn a_cursor_may_equal_but_never_exceed_the_server_head() {
        assert!(!cursor_is_ahead(SyncCursor::default(), heads(0, 0)));
        assert!(!cursor_is_ahead(cursor(10, 10), heads(10, 10)));
        assert!(cursor_is_ahead(cursor(11, 10), heads(10, 10)));
        assert!(cursor_is_ahead(cursor(10, 11), heads(10, 10)));
    }

    #[test]
    fn server_admission_rejects_future_wall_clock() {
        let server_now = 1_000_000;
        assert_eq!(storage_rejection(&reading_mutation(server_now + MAX_LWW_FUTURE_SKEW_MS + 1), server_now), Some(MutationRejectionReason::TimestampTooFarFuture),);
        assert_eq!(storage_rejection(&reading_mutation(server_now), server_now), None);
    }

    /// The declared reference is the whole of what the service knows about a
    /// book's bytes, so a lifecycle mutation that omits it cannot be stored:
    /// its row would leave the quota ledger unable to say what the cell holds.
    /// Retrying cannot fix a malformed payload, so the rejection is permanent.
    #[test]
    fn server_admission_requires_a_lifecycle_blob_declaration() {
        let server_now = 1_000_000;
        let mut lifecycle = reading_mutation(server_now);
        lifecycle.kind = sync_common::mutation_kind::BOOK_LIFECYCLE.to_owned();

        assert_eq!(storage_rejection(&lifecycle, server_now), Some(MutationRejectionReason::InvalidPayload));
        assert!(!MutationRejectionReason::InvalidPayload.is_transient(), "a client cannot make a malformed payload valid by retrying it");

        lifecycle.blob_reference = Some(sync_common::DeclaredBlobReference { present: false, content_hash: None });
        assert_eq!(storage_rejection(&lifecycle, server_now), None, "declaring that no blob is referenced is itself a complete declaration");

        // Every other kind is opaque and carries no blob accounting at all.
        assert_eq!(storage_rejection(&reading_mutation(server_now), server_now), None);
    }

    const LWW_TRIGGER_SQL: &str = include_str!("../schema/03_sync.sql");

    #[test]
    fn postgres_winner_order_matches_the_shared_version_key() {
        let comparison = LWW_TRIGGER_SQL.split("IF (").nth(1).expect("LWW predicate");
        let comparison = comparison.split(')').next().expect("LWW tuple").split_whitespace().collect::<String>();
        assert_eq!(comparison, "NEW.changed_at,NEW.version_rank,NEW.replica_idCOLLATE\"C\",NEW.replica_seq,NEW.event_idCOLLATE\"C\"");
        assert!(!LWW_TRIGGER_SQL.contains("hlc_"));
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn inventory_returns_only_state_cells_absent_from_both_server_tables() {
        let Some(pool) = crate::test_support::isolated_pool("state_inventory", 2).await else { return };
        let user_id = uuid::Uuid::new_v4().to_string();
        let library_id = sync_common::LibraryId::new_v4();
        sqlx::query("INSERT INTO users(id,email) VALUES($1,$2)").bind(&user_id).bind(format!("{user_id}@example.com")).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO libraries(id,user_id,name) VALUES($1,$2,'Books')").bind(library_id.to_string()).bind(&user_id).execute(&pool).await.unwrap();
        for (table, kind, key) in [("sync_state", "book_lifecycle", "book"), ("sync_reading_state", "reading_position", "reading")] {
            let statement = format!("INSERT INTO {table}(library_id,kind,entity_key,value,changed_at,replica_id,replica_seq,version_rank,event_id) VALUES($1,$2,$3,''::bytea,1,$4,1,0,$5)");
            sqlx::query(&statement).bind(library_id.to_string()).bind(kind).bind(key).bind(uuid::Uuid::new_v4().to_string()).bind(uuid::Uuid::new_v4().to_string()).execute(&pool).await.unwrap();
        }
        let cells = vec![
            StateCell { kind: "book_lifecycle".into(), entity_key: "book".into(), entity_subkey: String::new() },
            StateCell { kind: "reading_position".into(), entity_key: "reading".into(), entity_subkey: String::new() },
            StateCell { kind: "placement".into(), entity_key: "book".into(), entity_subkey: "shelf".into() },
        ];

        assert_eq!(missing_state_cells(&pool, &library_id, &cells).await.unwrap(), vec![cells[2].clone()]);
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn malformed_lifecycle_reference_metadata_is_not_defaulted() {
        let Some(pool) = crate::test_support::isolated_pool("lifecycle_decode", 2).await else { return };
        let row = sqlx::query(
            "SELECT TRUE AS present, NULL::text AS content_hash,
                    'not-a-timestamp'::text AS changed_at, 0::smallint AS version_rank,
                    'replica'::text AS replica_id, 1::bigint AS replica_seq,
                    'event'::text AS event_id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert!(matches!(decode_lifecycle_reference(row), Err(sqlx::Error::Decode(_))), "invalid persisted winner metadata must abort accounting instead of becoming a default value");
    }

    /// An undeclared presence is not corruption, and the two must not share a
    /// failure mode: a row written before the declaration was mandatory has
    /// `present = NULL`, and refusing to decode it makes that one cell answer
    /// every later exchange for its library with an internal error forever.
    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn an_undeclared_lifecycle_presence_decodes_as_holding_no_reference() {
        let Some(pool) = crate::test_support::isolated_pool("lifecycle_null_present", 2).await else { return };
        let row = sqlx::query(
            "SELECT NULL::boolean AS present, NULL::text AS content_hash,
                    1::bigint AS changed_at, 0::smallint AS version_rank,
                    'replica'::text AS replica_id, 1::bigint AS replica_seq,
                    'event'::text AS event_id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        let decoded = decode_lifecycle_reference(row).expect("an absent declaration must decode");
        assert_eq!(decoded.present, None, "absent stays absent rather than becoming a default `false`");
    }
}
