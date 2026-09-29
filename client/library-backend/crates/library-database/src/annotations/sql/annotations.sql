-- Reader annotations: identity and ordering stay normalized, all detail
-- lives in the JSON `detail` blob. Sync mutations are emitted by triggers.

-- name: get_annotations?
-- param: content_hash: &str
SELECT annotation.id, book.content_hash, annotation.toc_ordinal,
       annotation.progress, annotation.modified_at, annotation.detail
FROM annotation JOIN book ON book.row_id=annotation.book_row_id
WHERE book.content_hash=:content_hash AND annotation.deleted_at IS NULL
ORDER BY annotation.toc_ordinal NULLS LAST, annotation.progress NULLS LAST,
         annotation.modified_at DESC, annotation.id;

-- name: get_annotation_by_id?
-- param: id: &str
SELECT annotation.id, book.content_hash, annotation.toc_ordinal,
       annotation.progress, annotation.modified_at, annotation.detail
FROM annotation JOIN book ON book.row_id=annotation.book_row_id
WHERE annotation.id=:id;

-- name: upsert_annotation!
-- param: id: &str
-- param: content_hash: &str
-- param: toc_ordinal: Option<i64>
-- param: progress: Option<f32>
-- param: detail: &[u8]
-- param: modified_at: i64
-- param: deleted_at: Option<i64>
INSERT INTO annotation(
    id,book_row_id,toc_ordinal,progress,detail,
    modified_at,deleted_at
)
VALUES(
    :id,(SELECT row_id FROM book WHERE content_hash=:content_hash),:toc_ordinal,:progress,:detail,
    :modified_at,:deleted_at
)
ON CONFLICT(id) DO UPDATE SET
    book_row_id=excluded.book_row_id,
    toc_ordinal=excluded.toc_ordinal,
    progress=excluded.progress,
    detail=excluded.detail,
    modified_at=excluded.modified_at,
    deleted_at=excluded.deleted_at;

-- name: delete_annotation!
-- param: id: &str
-- param: modified_at: i64
UPDATE annotation
SET modified_at=:modified_at, deleted_at=:modified_at
WHERE id=:id;
