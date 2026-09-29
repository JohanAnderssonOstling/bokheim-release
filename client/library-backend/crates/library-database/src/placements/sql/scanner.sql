-- Immutable scanner inventory snapshots and revision guards.

-- name: scanner_file_work_revision ->
SELECT COALESCE(SUM(seq), 0)
FROM sqlite_sequence
WHERE name IN ('local_directory_work', 'local_book_work', 'local_file_work');

-- name: scanner_pending_directories?
-- param: root_id: &str
WITH RECURSIVE pending(id) AS (
    SELECT dir_id FROM local_directory_work
    UNION
    SELECT d.id FROM dir d JOIN pending p ON d.parent_id = p.id WHERE d.id != d.parent_id
)
SELECT pending.id,
       CASE WHEN pending.id = :root_id THEN '' ELSE paths.path END,
       projection.relative_path
FROM pending
LEFT JOIN dir_paths paths ON paths.id = pending.id
LEFT JOIN local_directory_projection projection ON projection.dir_id = pending.id;

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
SELECT relative_path FROM local_file_work
UNION ALL
SELECT '/' || CASE WHEN dp.path = '' THEN bd.file_name ELSE dp.path || '/' || bd.file_name END
FROM pending
JOIN book b USING(content_hash)
JOIN book_dir bd ON bd.book_row_id = b.row_id
JOIN dir_paths dp ON dp.id = bd.dir_id
WHERE b.deleted_at IS NULL AND bd.deleted_at IS NULL;

-- name: scanner_audible_current?
-- param: policy: &str
SELECT content_hash
FROM audible_enrichment_jobs
WHERE policy = :policy;

-- Scanner inventory and apply operations.

-- name: scanner_set_sequence!
-- param: value: &str
INSERT INTO sync_metadata(singleton,scan_seq) VALUES(1,:value) ON CONFLICT(singleton) DO UPDATE SET scan_seq=excluded.scan_seq;

-- name: scanner_known_books?
SELECT bd.local_hash,b.content_hash,bd.dir_id,bd.file_name,b.description_scanned=0,i.version
FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id
LEFT JOIN local_import_inspection i ON i.content_hash=b.content_hash AND i.format=b.format
WHERE bd.deleted_at IS NULL AND b.deleted_at IS NULL AND bd.local_hash!='';

-- name: scanner_known_directories?
SELECT d.id,d.parent_id,d.name,d.deleted_at IS NULL,paths.path FROM dir d LEFT JOIN dir_paths paths ON paths.id=d.id;

-- name: scanner_tombstoned_files?
SELECT removed.dir_id,removed.file_name FROM book_dir removed JOIN book b ON b.row_id=removed.book_row_id
WHERE (removed.deleted_at IS NOT NULL OR b.deleted_at IS NOT NULL)
AND NOT EXISTS(SELECT 1 FROM book_dir live JOIN book owner ON owner.row_id=live.book_row_id
    WHERE live.dir_id=removed.dir_id AND live.file_name=removed.file_name AND live.deleted_at IS NULL AND owner.deleted_at IS NULL);

-- name: scanner_upsert_directory!
-- param: id: &str
-- param: parent_id: &str
-- param: name: &str
INSERT INTO dir(id,parent_id,name,intent_parent_id,intent_name,intent_lifecycle,deleted_at) VALUES(:id,:parent_id,:name,:parent_id,:name,0,NULL)
ON CONFLICT(id) DO UPDATE SET parent_id=excluded.parent_id,name=excluded.name,intent_parent_id=excluded.parent_id,intent_name=excluded.name,intent_lifecycle=0,deleted_at=NULL;

-- name: scanner_insert_book!
-- param: content_hash: &str
-- param: created_at: i64
-- param: format: &str
INSERT INTO book(content_hash,added_at,format) VALUES(:content_hash,:created_at,:format) ON CONFLICT(content_hash) DO NOTHING;

-- name: scanner_insert_book_dir!
-- param: dir_id: &str
-- param: content_hash: &str
-- param: file_name: &str
-- param: local_hash: &str
-- param: last_scan: i64
INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,last_scan,is_downloaded,deleted_at)
VALUES(:dir_id,(SELECT row_id FROM book WHERE content_hash=:content_hash),:file_name,:local_hash,:last_scan,1,NULL)
ON CONFLICT(dir_id,book_row_id) DO UPDATE SET file_name=excluded.file_name,local_hash=excluded.local_hash,last_scan=excluded.last_scan,is_downloaded=1,deleted_at=NULL;

-- name: scanner_clear_renamed_placement!
-- param: dir_id: &str
-- param: content_hash: &str
-- param: file_name: &str
UPDATE book_dir SET deleted_at=unixepoch() WHERE dir_id=:dir_id AND book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND file_name!=:file_name AND deleted_at IS NULL;

-- name: scanner_observe_local_version!
-- param: content_hash: &str
-- param: checksum: &str
-- param: size_bytes: i64
INSERT INTO local_book_versions(content_hash,checksum,size_bytes,origin) VALUES(:content_hash,:checksum,:size_bytes,'local') ON CONFLICT(content_hash,checksum) DO NOTHING;

-- name: scanner_remember_placement!
-- param: dir_id: &str
-- param: content_hash: &str
-- param: checksum: &str
INSERT INTO local_book_current(dir_id,content_hash,checksum,origin) VALUES(:dir_id,:content_hash,:checksum,'local') ON CONFLICT(dir_id,content_hash) DO UPDATE SET id=excluded.id,checksum=excluded.checksum,origin=excluded.origin;

-- name: scanner_mark_seen!
-- param: scan_id: i64
-- param: placements: &str
UPDATE book_dir SET last_scan=:scan_id WHERE (dir_id,file_name,local_hash) IN (SELECT json_extract(value,'$[0]'),json_extract(value,'$[1]'),json_extract(value,'$[2]') FROM json_each(:placements)) AND deleted_at IS NULL;

-- name: scanner_placements?
SELECT b.content_hash, bd.dir_id, bd.file_name,
       CASE WHEN paths.path='' THEN bd.file_name ELSE paths.path || '/' || bd.file_name END,
       bd.local_hash, bd.is_downloaded, bd.last_scan, projection.relative_path
FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id
JOIN dir_paths paths ON paths.id=bd.dir_id
LEFT JOIN local_file_projection projection ON projection.content_hash=b.content_hash AND projection.dir_id=bd.dir_id
WHERE bd.deleted_at IS NULL AND b.deleted_at IS NULL
ORDER BY bd.dir_id,b.content_hash;

-- name: scanner_retire_replaced_placement!
-- param: dir_id: &str
-- param: content_hash: &str
-- param: deleted_at: i64
UPDATE book_dir SET deleted_at=:deleted_at,is_downloaded=0,local_hash=''
WHERE dir_id=:dir_id AND book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND deleted_at IS NULL;

-- name: scanner_clear_local_availability!
-- param: dir_id: &str
-- param: content_hash: &str
UPDATE book_dir SET is_downloaded=0,local_hash=''
WHERE dir_id=:dir_id AND book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND deleted_at IS NULL;

-- name: scanner_forget_projection!
-- param: dir_id: &str
-- param: content_hash: &str
DELETE FROM local_file_projection WHERE dir_id=:dir_id AND content_hash=:content_hash;

-- name: scanner_forget_current_version!
-- param: dir_id: &str
-- param: content_hash: &str
DELETE FROM local_book_current WHERE dir_id=:dir_id AND content_hash=:content_hash;
