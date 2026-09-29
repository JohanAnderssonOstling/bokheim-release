-- The one synchronized reading-position register, shared by every format.

-- name: reading_get_position?
-- param: content_hash: &str
SELECT read_pos FROM book WHERE content_hash = :content_hash AND deleted_at IS NULL;

-- name: reading_set_position!
-- param: content_hash: &str
-- param: position: &str
-- param: progress: Option<f32>
-- A reader which cannot cheaply calculate book-wide progress (PDF,
-- audiobook seeking) updates only the canonical location register.
UPDATE book
SET read_pos = :position,
    read_progress = COALESCE(:progress, read_progress)
WHERE content_hash = :content_hash;

-- name: reading_upsert_toc_entry!
-- param: content_hash: &str
-- param: target: &str
INSERT INTO book_reading_entry(content_hash,target)
VALUES(:content_hash,:target)
ON CONFLICT(content_hash) DO UPDATE SET target=excluded.target;
