-- Immutable scanner inventory snapshots and revision guards.

-- name: scanner_file_work_revision ->
SELECT COALESCE(SUM(seq), 0)
FROM sqlite_sequence
WHERE name IN ('local_directory_work', 'local_book_work', 'local_file_work');

-- name: scanner_pending_directories?
-- param: directories: &str
-- Folders with queued work and everything below them, with where each was
-- last materialized; Rust adds each folder's live path.
SELECT pending.value, projection.relative_path
FROM json_each(:directories) pending
LEFT JOIN local_directory_projection projection ON projection.dir_id = pending.value;

-- name: scanner_pending_files?
WITH pending(content_hash) AS (
    SELECT content_hash FROM local_book_work
    UNION
    SELECT content_hash FROM local_file_work
)
SELECT p.relative_path
FROM local_file_projection p
JOIN pending USING(content_hash)
UNION ALL
SELECT relative_path FROM local_file_work;

-- name: scanner_pending_placements?
-- Live placements of books with queued work; Rust adds each one's path.
WITH pending(content_hash) AS (
    SELECT content_hash FROM local_book_work
    UNION
    SELECT content_hash FROM local_file_work
)
SELECT bd.dir_id, bd.file_name
FROM pending
JOIN book b USING(content_hash)
JOIN book_dir bd ON bd.book_row_id = b.row_id
WHERE b.deleted_at IS NULL AND bd.deleted_at IS NULL;

-- Scanner inventory and apply operations.

-- name: scanner_set_sequence!
-- param: value: &str
INSERT INTO sync_metadata(singleton,scan_seq) VALUES(1,:value) ON CONFLICT(singleton) DO UPDATE SET scan_seq=excluded.scan_seq;

-- name: scanner_known_books?
SELECT bd.local_hash,b.content_hash,bd.dir_id,bd.file_name,i.inspection_version,
       b.format,b.duration_ms,CAST(b.toc_json AS BLOB),CAST(b.tracks_json AS BLOB)
FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id
LEFT JOIN book_local_state i ON i.content_hash=b.content_hash AND i.inspection_format=b.format
WHERE bd.deleted_at IS NULL AND b.deleted_at IS NULL AND bd.local_hash!='';

-- name: scanner_known_directories?
SELECT id,parent_id,name,deleted_at IS NULL FROM dir;

-- name: scanner_tombstoned_files?
SELECT removed.dir_id,removed.file_name FROM book_dir removed JOIN book b ON b.row_id=removed.book_row_id
WHERE (removed.deleted_at IS NOT NULL OR b.deleted_at IS NOT NULL)
AND NOT EXISTS(SELECT 1 FROM book_dir live JOIN book owner ON owner.row_id=live.book_row_id
    WHERE live.dir_id=removed.dir_id AND live.file_name=removed.file_name AND live.deleted_at IS NULL AND owner.deleted_at IS NULL);

-- name: scanner_placements?
SELECT b.content_hash, bd.dir_id, bd.file_name,
       bd.local_hash, bd.is_downloaded, projection.relative_path
FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id
LEFT JOIN local_file_projection projection ON projection.content_hash=b.content_hash AND projection.dir_id=bd.dir_id
WHERE bd.deleted_at IS NULL AND b.deleted_at IS NULL
ORDER BY bd.dir_id,b.content_hash;
