-- Durable import/sync workflow record and one-snapshot progress accounting.

-- name: read_operation ->
SELECT operation_id, operation_kind, operation_scan_complete, operation_scan_failed, operation_discovered, operation_failures, operation_requires_sync, operation_completed, operation_sync_unfinished
FROM sync_metadata WHERE singleton = 1;

-- name: write_operation!
-- param: id: &str
-- param: kind: &str
-- param: scan_complete: i64
-- param: scan_failed: i64
-- param: discovered: i64
-- param: failures: i64
-- param: requires_sync: i64
-- param: completed: i64
-- param: sync_unfinished: i64
INSERT INTO sync_metadata(singleton, operation_id, operation_kind, operation_scan_complete, operation_scan_failed, operation_discovered, operation_failures, operation_requires_sync, operation_completed, operation_sync_unfinished)
VALUES (1, :id, :kind, :scan_complete, :scan_failed, :discovered, :failures, :requires_sync, :completed, :sync_unfinished)
ON CONFLICT(singleton) DO UPDATE SET
    operation_id=excluded.operation_id,
    operation_kind=excluded.operation_kind,
    operation_scan_complete=excluded.operation_scan_complete,
    operation_scan_failed=excluded.operation_scan_failed,
    operation_discovered=excluded.operation_discovered,
    operation_failures=excluded.operation_failures,
    operation_requires_sync=excluded.operation_requires_sync,
    operation_completed=excluded.operation_completed,
    operation_sync_unfinished=excluded.operation_sync_unfinished;

-- name: operation_progress ->
SELECT
 (SELECT count(*) FROM book WHERE deleted_at IS NULL AND hidden_at IS NULL),
 (SELECT count(*) FROM remote_asset WHERE kind='book' AND hash IN (SELECT content_hash FROM book WHERE deleted_at IS NULL AND hidden_at IS NULL)),
 (SELECT count(*) FROM local_book_upload),
 (SELECT count(*) FROM sync_outbox),
 (SELECT count(*) FROM local_thumbnail_work t JOIN book b ON b.content_hash=t.content_hash WHERE t.state='pending' AND b.deleted_at IS NULL),
 (SELECT count(*) FROM local_thumbnail_work t JOIN local_thumbnail_source s USING(content_hash)
  WHERE t.state='ready' AND s.origin='local' AND NOT EXISTS(SELECT 1 FROM remote_asset r WHERE r.kind='thumbnail' AND r.hash=t.content_hash)),
 (SELECT count(*) FROM rejected_asset_upload r WHERE r.rejected_at>unixepoch()-300 AND
  ((r.kind='book' AND EXISTS(SELECT 1 FROM local_book_upload u WHERE u.content_hash=r.hash)) OR
   (r.kind='thumbnail' AND EXISTS(SELECT 1 FROM local_thumbnail_work t JOIN local_thumbnail_source s USING(content_hash) WHERE t.content_hash=r.hash AND t.state='ready' AND s.origin='local') AND NOT EXISTS(SELECT 1 FROM remote_asset a WHERE a.kind='thumbnail' AND a.hash=r.hash)))),
 EXISTS(SELECT 1 FROM sync_metadata WHERE singleton = 1 AND last_successful_sync_at IS NOT NULL);

-- name: operations_set_last_successful_sync_at!
-- param: value: &str
INSERT INTO sync_metadata(singleton, last_successful_sync_at) VALUES (1, :value)
ON CONFLICT(singleton) DO UPDATE SET last_successful_sync_at = excluded.last_successful_sync_at;

-- name: clear_scan_failures!
DELETE FROM local_scan_failure;

-- name: record_scan_failure!
-- param: path: &str
-- param: error: &str
INSERT INTO local_scan_failure(path,error) VALUES(:path,:error) ON CONFLICT(path) DO UPDATE SET error=excluded.error;

-- name: read_scan_failures?
SELECT path,error FROM local_scan_failure ORDER BY path;
