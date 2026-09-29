-- Local book-path resolution and PDF metadata fingerprints.

-- name: stored_pdf_metadata?
-- param: content_hash: &str
-- param: checksum: &str
SELECT metadata
FROM pdf_reader_metadata
WHERE content_hash = :content_hash AND checksum = :checksum;

-- name: local_book_info?
-- param: content_hash: &str
-- A book can have more than one live placement (filed in two folders, or a
-- remote mutation adding a second one); only some may have local bytes.
-- Preferring a downloaded placement means the reader opens bytes that are
-- actually present instead of failing on an arbitrarily-chosen empty one.
WITH RECURSIVE ancestors(id, parent_id, path, is_downloaded) AS (
    SELECT d.id, d.parent_id,
        CASE WHEN d.id = '00000000-0000-0000-0000-000000000000' THEN bd.file_name ELSE d.name || '/' || bd.file_name END,
        bd.is_downloaded
    FROM book_dir bd JOIN dir d ON d.id = bd.dir_id
    WHERE bd.book_row_id = (SELECT row_id FROM book WHERE content_hash = :content_hash)
      AND bd.deleted_at IS NULL AND d.deleted_at IS NULL
    UNION ALL
    SELECT d.id, d.parent_id,
        CASE WHEN d.id = '00000000-0000-0000-0000-000000000000' THEN a.path ELSE d.name || '/' || a.path END,
        a.is_downloaded
    FROM ancestors a JOIN dir d ON d.id = a.parent_id
    WHERE a.id != a.parent_id AND d.deleted_at IS NULL
), local_path AS (
    SELECT '/' || path AS path FROM ancestors
    WHERE id = '00000000-0000-0000-0000-000000000000'
    ORDER BY is_downloaded DESC, path LIMIT 1
)
SELECT b.format, (SELECT path FROM local_path)
FROM book b WHERE b.content_hash = :content_hash;

-- name: book_paths?
-- param: content_hash: &str
SELECT relative_path, 0 AS priority
FROM local_file_projection
WHERE content_hash = :content_hash
UNION ALL
SELECT '/' || CASE WHEN dp.path = '' THEN bd.file_name ELSE dp.path || '/' || bd.file_name END, 1
FROM book_dir bd
JOIN book b ON b.row_id = bd.book_row_id
JOIN dir_paths dp ON dp.id = bd.dir_id
WHERE b.content_hash = :content_hash AND b.deleted_at IS NULL AND bd.deleted_at IS NULL
ORDER BY priority;

-- name: local_book_paths?
SELECT p.relative_path
FROM local_file_projection p
JOIN book b ON b.content_hash = p.content_hash
WHERE b.deleted_at IS NULL
UNION
SELECT '/' || CASE WHEN dp.path = '' THEN bd.file_name ELSE dp.path || '/' || bd.file_name END
FROM book_dir bd
JOIN book b ON b.row_id = bd.book_row_id
JOIN dir_paths dp ON dp.id = bd.dir_id
WHERE b.deleted_at IS NULL AND bd.deleted_at IS NULL;

-- name: local_pdf_checksum ->
-- param: content_hash: &str
-- param: fingerprint: &str
-- param: relative_path: &str
SELECT current.checksum
FROM local_book_current current
JOIN book b ON b.content_hash = current.content_hash
JOIN book_dir bd ON bd.book_row_id = b.row_id AND bd.dir_id = current.dir_id
JOIN dir_paths dp ON dp.id = bd.dir_id
WHERE b.content_hash = :content_hash
  AND bd.local_hash = :fingerprint
  AND bd.deleted_at IS NULL
  AND b.deleted_at IS NULL
  AND '/' || CASE WHEN dp.path = '' THEN bd.file_name ELSE dp.path || '/' || bd.file_name END = :relative_path;
