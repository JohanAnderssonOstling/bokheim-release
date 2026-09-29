-- Shared statements used by multiple database features.

-- name: shared_owner_replica_id?
SELECT replica_id FROM sync_metadata WHERE singleton = 1;


-- name: shared_owner_last_successful_sync_at?
SELECT CAST(last_successful_sync_at AS TEXT) FROM sync_metadata WHERE singleton = 1;


-- name: shared_owner_remote_library_name?
SELECT remote_library_name FROM sync_metadata WHERE singleton = 1;


-- name: shared_owner_server_library_created?
SELECT server_library_created FROM sync_metadata WHERE singleton = 1;


-- name: shared_audiobook_upsert_toc!
-- param: content_hash: &str
-- param: entry_count: i64
-- param: toc_json: &str
INSERT INTO book_toc(content_hash,entry_count,toc_json)
VALUES(:content_hash,:entry_count,:toc_json)
ON CONFLICT(content_hash) DO UPDATE SET
    entry_count=excluded.entry_count,
    toc_json=excluded.toc_json;


-- name: shared_enrichment_mark_description_scanned!
-- param: content_hash: &str
UPDATE book SET description_scanned = 1
WHERE content_hash = :content_hash AND description_scanned IS NOT 1;

-- Thumbnail recovery and remote availability.


-- name: shared_request_thumbnail!
-- param: content_hash: &str
INSERT INTO local_thumbnail_work(content_hash, state) VALUES (:content_hash, 'pending')
ON CONFLICT(content_hash) DO UPDATE SET state = 'pending', retry_after = 0;


-- name: shared_scanner_sequence?
SELECT CAST(scan_seq AS TEXT) FROM sync_metadata WHERE singleton = 1;


-- name: shared_scanner_enqueue_upload!
-- param: content_hash: &str
-- param: checksum: &str
-- param: size_bytes: i64
INSERT INTO local_book_upload(content_hash,checksum,size_bytes) VALUES(:content_hash,:checksum,:size_bytes) ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id,checksum=excluded.checksum,size_bytes=excluded.size_bytes;


-- name: shared_scanner_enqueue_asset_work!
-- param: content_hash: &str
INSERT INTO asset_work(content_hash) VALUES(:content_hash) ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;


-- name: shared_scanner_publish_projection!
-- param: content_hash: &str
-- param: dir_id: &str
-- param: relative_path: &str
INSERT INTO local_file_projection(content_hash,dir_id,relative_path) VALUES(:content_hash,:dir_id,:relative_path) ON CONFLICT(content_hash,dir_id) DO UPDATE SET relative_path=excluded.relative_path;


-- name: shared_occupied_file_names?
-- param: destination: &str
SELECT file_name FROM book_dir WHERE dir_id = :destination AND deleted_at IS NULL;


-- name: shared_set_book_hash_downloaded!
-- param: content_hash: &str
-- param: is_downloaded: i32
UPDATE book_dir
SET is_downloaded = :is_downloaded
WHERE book_row_id = (SELECT row_id FROM book WHERE content_hash=:content_hash)
  AND deleted_at IS NULL;


-- name: shared_placements_queue_directory_work!
-- param: dir_id: &str
INSERT OR REPLACE INTO local_directory_work(dir_id) VALUES(:dir_id);


-- name: shared_placements_queue_file_work!
-- param: operation: &str
-- param: content_hash: &str
-- param: relative_path: &str
INSERT OR IGNORE INTO local_file_work(operation,content_hash,relative_path) VALUES(:operation,:content_hash,:relative_path);


-- name: shared_cancel_book_restore!
-- param: content_hash: &str
DELETE FROM local_file_work WHERE content_hash=:content_hash AND operation='restore';


-- name: shared_purge_book_identifier!
-- param: book_row_id: i64
DELETE FROM book_identifier WHERE book_row_id=:book_row_id;


-- name: shared_purge_book_record_insert!
-- param: content_hash: &str
INSERT INTO local_purge_work(content_hash) VALUES(:content_hash)
ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;


-- name: shared_cursor_state_select?
SELECT CAST(last_pull_state_seq AS TEXT) FROM sync_metadata WHERE singleton = 1;


-- name: shared_cursor_reading_select?
SELECT CAST(last_pull_reading_seq AS TEXT) FROM sync_metadata WHERE singleton = 1;


-- name: shared_apply_remote_book_purged_select?
-- param: content_hash: &str
SELECT row_id FROM book WHERE content_hash=:content_hash;


-- name: shared_authors_insert_author_identity!
-- param: stable_id: &str
-- param: preferred_name: &str
-- param: normalized_name: &str
-- param: created_at: i64
INSERT INTO author_identity(stable_id, preferred_name, normalized_name, is_provisional, created_at)
VALUES(:stable_id, :preferred_name, :normalized_name, 1, :created_at);


-- name: shared_transfer_complete_download!
-- param: content_hash: &str
DELETE FROM download_requests WHERE target_kind='book' AND target_key=:content_hash;


-- name: shared_observe_local_book_insert!
-- param: content_hash: &str
-- param: checksum: &str
-- param: size_bytes: i64
-- param: origin: &str
INSERT INTO local_book_versions(content_hash,checksum,size_bytes,origin) VALUES(:content_hash,:checksum,:size_bytes,:origin) ON CONFLICT(content_hash,checksum) DO NOTHING;


-- name: shared_current_checksum_select?
-- param: dir_id: &str
-- param: content_hash: &str
SELECT checksum FROM local_book_current WHERE dir_id=:dir_id AND content_hash=:content_hash;


-- name: shared_version_origin_select?
-- param: content_hash: &str
-- param: checksum: &str
SELECT origin FROM local_book_versions WHERE content_hash=:content_hash AND checksum=:checksum;


-- name: shared_current_replace!
-- param: dir_id: &str
-- param: content_hash: &str
-- param: checksum: &str
-- param: origin: &str
INSERT INTO local_book_current(dir_id,content_hash,checksum,origin) VALUES(:dir_id,:content_hash,:checksum,:origin)
        ON CONFLICT(dir_id,content_hash) DO UPDATE SET id=excluded.id,checksum=excluded.checksum,origin=excluded.origin;


-- name: shared_rejected_book_cleanup!
-- param: content_hash: &str
DELETE FROM rejected_asset_upload WHERE kind='book' AND hash=:content_hash;


-- name: shared_update_canonical_metadata!
-- param: content_hash: &str
-- param: metadata: &[u8]
UPDATE book SET book_metadata=:metadata WHERE content_hash=:content_hash AND book_metadata IS NOT :metadata;

-- name: shared_book_metadata?
-- param: content_hash: &str
SELECT book_metadata FROM book WHERE content_hash=:content_hash;

-- name: shared_book_subjects?
-- param: content_hash: &str
SELECT name,source,authority,code FROM book_subject WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) ORDER BY position;
