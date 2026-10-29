-- Table-of-contents reads.

-- name: book_toc_select?
-- param: content_hash: &BookId
SELECT CAST(toc_json AS BLOB) FROM book WHERE content_hash=:content_hash AND toc_json IS NOT NULL;

-- name: reading_entry_select?
-- param: content_hash: &BookId
SELECT reading_target FROM book_local_state WHERE content_hash=:content_hash AND reading_target IS NOT NULL;

-- Complete synchronized navigation documents replace all optional fields.
-- name: navigation_upsert!
-- param: content_hash: &BookId
-- param: entry_count: i64
-- param: toc_json: &[u8]
-- param: duration_ms: Option<i64>
-- param: tracks_json: Option<&[u8]>
UPDATE book SET entry_count=:entry_count, toc_json=:toc_json,
    duration_ms=:duration_ms, tracks_json=:tracks_json
WHERE content_hash=:content_hash;
