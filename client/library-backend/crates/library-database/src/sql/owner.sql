-- Library ownership, lifecycle, and durable session state.

-- name: owner_set_replica_id!
-- param: value: &str
INSERT INTO sync_metadata(singleton, replica_id) VALUES (1, :value)
ON CONFLICT(singleton) DO UPDATE SET replica_id = excluded.replica_id;

-- name: owner_set_change_origin!
-- param: value: &str
INSERT INTO sync_metadata(singleton, change_origin) VALUES (1, :value)
ON CONFLICT(singleton) DO UPDATE SET change_origin = excluded.change_origin;

-- name: owner_set_remote_library_name!
-- param: value: &str
INSERT INTO sync_metadata(singleton, remote_library_name) VALUES (1, :value)
ON CONFLICT(singleton) DO UPDATE SET remote_library_name = excluded.remote_library_name;

-- name: owner_set_server_library_created!
-- param: value: &str
INSERT INTO sync_metadata(singleton, server_library_created) VALUES (1, :value)
ON CONFLICT(singleton) DO UPDATE SET server_library_created = excluded.server_library_created;

-- name: owner_transfer_history?
SELECT entries_json FROM local_transfer_history WHERE singleton = 1;

-- name: owner_save_transfer_history!
-- param: entries_json: &str
INSERT INTO local_transfer_history(singleton, entries_json) VALUES (1, :entries_json)
ON CONFLICT(singleton) DO UPDATE SET entries_json = excluded.entries_json;

-- name: owner_sync_initialized ->
SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'sync_metadata' AND type = 'table');

-- name: owner_has_unsynced_changes ->
SELECT EXISTS(SELECT 1 FROM sync_outbox LIMIT 1);

-- name: owner_cloud_storage_policy!
-- param: enabled: &str
INSERT INTO sync_metadata(singleton, cloud_storage_enabled) VALUES (1, :enabled)
ON CONFLICT(singleton) DO UPDATE SET cloud_storage_enabled = excluded.cloud_storage_enabled WHERE cloud_storage_enabled IS NOT excluded.cloud_storage_enabled;

-- name: owner_reset_remote_books!
DELETE FROM remote_asset WHERE kind = 'book';

-- name: owner_reset_rejected_book_uploads!
DELETE FROM rejected_asset_upload WHERE kind = 'book';

-- name: owner_requeue_cloud_storage!
WITH RECURSIVE live_dirs(id) AS (
    SELECT id FROM dir WHERE id = '00000000-0000-0000-0000-000000000000' AND deleted_at IS NULL
    UNION ALL
    SELECT d.id FROM dir d JOIN live_dirs p ON d.parent_id = p.id AND d.id != p.id WHERE d.deleted_at IS NULL
), eligible AS (
    SELECT current.content_hash, current.checksum, version.size_bytes,
        row_number() OVER(PARTITION BY current.content_hash ORDER BY current.id DESC) AS priority
    FROM local_book_current current
    JOIN local_book_versions version USING(content_hash, checksum)
    JOIN book b ON b.content_hash = current.content_hash AND b.deleted_at IS NULL
    JOIN book_dir bd ON bd.book_row_id = b.row_id AND bd.dir_id = current.dir_id AND bd.deleted_at IS NULL
    JOIN live_dirs d ON d.id = current.dir_id
    WHERE version.size_bytes > 0
)
INSERT INTO local_book_upload(content_hash, checksum, size_bytes)
SELECT content_hash, checksum, size_bytes FROM eligible WHERE priority = 1
ON CONFLICT(content_hash) DO NOTHING;

-- name: owner_refresh_book_uploads!
INSERT INTO local_book_upload(content_hash, checksum, size_bytes)
SELECT content_hash, checksum, size_bytes FROM local_book_upload WHERE true
ON CONFLICT(content_hash) DO UPDATE SET id = excluded.id;

-- name: owner_refresh_asset_work!
INSERT INTO asset_work(content_hash) SELECT content_hash FROM local_book_upload WHERE true
ON CONFLICT(content_hash) DO UPDATE SET id = excluded.id;

-- name: owner_operation_record?
SELECT operation_kind, operation_scan_complete, operation_completed FROM sync_metadata WHERE singleton = 1;

-- name: owner_book_format?
-- param: content_hash: &str
SELECT format FROM book WHERE content_hash = :content_hash;

-- name: taxonomy_attached?
SELECT EXISTS(SELECT 1 FROM pragma_database_list WHERE name='curated');

-- name: taxonomy_attach!
-- param: db_name: &str
ATTACH :db_name AS curated;
