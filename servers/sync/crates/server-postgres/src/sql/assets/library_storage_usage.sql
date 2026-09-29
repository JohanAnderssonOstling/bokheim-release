-- Per-library breakdown of the bytes an account is charged for.
--
-- The quota charges each unique blob once (user_blob_charge is keyed by user
-- and content hash), but the same book may be referenced from several
-- libraries. Summing per library would therefore double count shared books and
-- overrun the quota, so each charged blob is attributed to exactly one library:
-- the lowest library id that still holds a present book_lifecycle reference to
-- it. The choice is arbitrary but stable, so a book does not move between
-- libraries in the breakdown from one reading to the next.
--
-- Blobs that are still charged but referenced by no library — a book removed
-- everywhere whose bytes have not yet been released — are attributed to no
-- library and appear as the difference between this total and used_bytes.
WITH referenced AS (
    SELECT DISTINCT state.library_id, COALESCE(revision.blob_hash, state.content_hash) AS content_hash
    FROM sync_state AS state
    JOIN libraries AS library
      ON library.id = state.library_id
     AND library.user_id = $1
     AND library.deleted_at IS NULL
     AND library.cloud_storage_enabled
    LEFT JOIN user_book_revision AS revision ON revision.user_id=$1 AND revision.content_hash=state.content_hash
    WHERE state.kind = 'book_lifecycle'
      AND state.present
      AND state.content_hash IS NOT NULL
    UNION
    SELECT claim.library_id, claim.blob_hash FROM library_blob_claim claim
    JOIN libraries library ON library.id=claim.library_id AND library.deleted_at IS NULL AND library.cloud_storage_enabled
    WHERE claim.user_id=$1
),
owning AS (
    SELECT referenced.content_hash, MIN(referenced.library_id) AS library_id
    FROM referenced
    JOIN user_blob_charge AS charge
      ON charge.user_id = $1
     AND charge.content_hash = referenced.content_hash
    GROUP BY referenced.content_hash
), book_usage AS (
    SELECT owning.library_id AS library_id,
           COALESCE(SUM(object.size_bytes), 0)::BIGINT AS used_bytes
    FROM owning
    JOIN blob_object AS object
      ON object.content_hash = owning.content_hash
    GROUP BY owning.library_id
)
SELECT library_id, used_bytes
FROM book_usage
ORDER BY library_id;
