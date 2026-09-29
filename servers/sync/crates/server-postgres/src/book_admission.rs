//! A shared book row must not precede its completed, account-owned upload.
use sqlx::{Postgres, Transaction};
use sync_common::{mutation_kind as kind, ContentHash, MutationRejectionReason as Reason, WireMutation};

/// Older clients did not declare prerequisites on metadata/placements. Their
/// canonical entity key still identifies the book, so they cannot bypass
/// admission. Opaque annotation/catalogue values need an explicit declaration.
pub(super) fn dependency(change: &WireMutation) -> Result<Option<ContentHash>, Reason> {
    let reference = change.blob_reference.as_ref();
    match change.kind.as_str() {
        kind::DIRECTORY_NAME | kind::DIRECTORY_PARENT | kind::DIRECTORY_LIFECYCLE => Ok(None),
        kind::BOOK_LIFECYCLE | kind::PLACEMENT | kind::ANNOTATION
            if reference.is_some_and(|r| !r.present && r.content_hash.is_none()) => Ok(None),
        kind::BOOK_FACTS | kind::BOOK_LIFECYCLE | kind::PLACEMENT | kind::READING_POSITION | kind::METADATA
        | kind::DESCRIPTION | kind::PDF_READER_METADATA => {
            if let Some(reference) = reference {
                if let Ok(identity) = change.entity_key.parse::<ContentHash>() {
                    if reference.content_hash != Some(identity) { return Err(Reason::InvalidPayload); }
                }
                return reference.content_hash.filter(|_| reference.present).map(Some).ok_or(Reason::InvalidPayload);
            }
            change.entity_key.parse::<ContentHash>().map(Some).map_err(|_| Reason::InvalidPayload)
        }
        kind::ANNOTATION if reference.is_some_and(|r| !r.present && r.content_hash.is_some()) => Ok(None),
        kind::ANNOTATION => reference
            .filter(|r| r.present).and_then(|r| r.content_hash).map(Some).ok_or(Reason::MissingDependency),
        _ => Ok(reference.filter(|r| r.present).and_then(|r| r.content_hash)),
    }
}

pub(super) async fn available(
    tx: &mut Transaction<'_, Postgres>, user: &str, library: &sync_common::LibraryId, hashes: &[String],
) -> Result<std::collections::HashSet<String>, sqlx::Error> {
    let enabled: bool = sqlx::query_scalar(include_str!("sql/cloud_storage/enabled.sql")).bind(library.to_string()).fetch_one(&mut **tx).await?;
    if !enabled { return Ok(hashes.iter().cloned().collect()); }
    if hashes.is_empty() { return Ok(Default::default()); }
    // Use the same lock order as upload finalization and lifecycle cleanup:
    // library, then account. A concurrent purge/revision replacement must not
    // invalidate admission between this check and the library commit.
    sqlx::query("SELECT user_id FROM user_storage_account WHERE user_id=$1 FOR UPDATE")
        .bind(user).fetch_optional(&mut **tx).await?;
    // Lifecycle references alone are not proof of upload. The upload endpoint
    // creates blob_object + charge + revision only after durable file storage;
    // negotiation creates them only after checking an existing stored object.
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT requested.identity FROM UNNEST($2::text[]) AS requested(identity)
         LEFT JOIN user_book_revision revision ON revision.user_id=$1 AND revision.content_hash=requested.identity
         JOIN user_blob_charge charge ON charge.user_id=$1
             AND charge.content_hash=COALESCE(revision.blob_hash,requested.identity)
         JOIN blob_object object ON object.content_hash=charge.content_hash AND object.size_bytes>0"
    ).bind(user).bind(hashes).fetch_all(&mut **tx).await?;
    Ok(rows.into_iter().collect())
}
