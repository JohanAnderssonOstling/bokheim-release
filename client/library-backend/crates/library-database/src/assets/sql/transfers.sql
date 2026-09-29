-- Transfer presentation SQL migrated from library-backend.

-- Asset transfer planning and settlement.

-- name: transfer_ready_uploads?
-- param: hashes: &str
SELECT p.content_hash FROM json_each(:hashes) requested
JOIN local_upload_preparation p ON p.content_hash=requested.value
LEFT JOIN local_book_upload u ON u.content_hash=p.content_hash
WHERE u.id IS NULL OR p.upload_id=u.id;

-- name: transfer_local_versions?
-- param: content_hash: &str
SELECT dir_id, file_name, local_hash FROM book_dir JOIN book ON book.row_id=book_dir.book_row_id
WHERE book.content_hash=:content_hash AND book_dir.deleted_at IS NULL ORDER BY dir_id,file_name;

-- name: transfer_upload_intent?
-- param: content_hash: &str
SELECT id, checksum, size_bytes FROM local_book_upload WHERE content_hash=:content_hash;

-- Hash-level transfer snapshot reads. Each query returns only rows for the
-- requested hashes; one-to-many values are grouped by the Rust assembler.
-- name: transfer_snapshot_versions?
-- param: hashes: &str
SELECT b.content_hash, bd.dir_id, bd.file_name, bd.local_hash
FROM json_each(:hashes) requested
JOIN book b ON b.content_hash = requested.value
JOIN book_dir bd ON bd.book_row_id = b.row_id AND bd.deleted_at IS NULL
ORDER BY b.content_hash, bd.dir_id, bd.file_name;

-- name: transfer_snapshot_intents?
-- param: hashes: &str
SELECT upload.content_hash, upload.id, upload.checksum, upload.size_bytes
FROM json_each(:hashes) requested
JOIN local_book_upload upload ON upload.content_hash = requested.value;

-- name: transfer_snapshot_paths?
-- param: hashes: &str
SELECT requested.value, paths.relative_path
FROM json_each(:hashes) requested
JOIN (
    SELECT content_hash, relative_path, 0 AS priority
    FROM local_file_projection
    UNION ALL
    SELECT b.content_hash,
           '/' || CASE WHEN dp.path = '' THEN bd.file_name ELSE dp.path || '/' || bd.file_name END,
           1 AS priority
    FROM book_dir bd
    JOIN book b ON b.row_id = bd.book_row_id
    JOIN dir_paths dp ON dp.id = bd.dir_id
    WHERE b.deleted_at IS NULL AND bd.deleted_at IS NULL
) paths ON paths.content_hash = requested.value
ORDER BY requested.value, paths.priority;

-- name: transfer_snapshot_thumbnail_allowed?
-- param: hashes: &str
SELECT requested.value,
       -- Covers can only be owned by a book the server already knows about.
       -- `local_book_upload` is an intent queue and is deliberately removed
       -- once the book upload finishes, so it must not gate a later cover
       -- retry.  Match the candidate planner's server-presence rule instead.
       EXISTS(SELECT 1 FROM remote_asset parent
              WHERE parent.kind = 'book' AND parent.hash = book.content_hash)
       AND NOT EXISTS(SELECT 1 FROM sync_outbox lifecycle
                      WHERE lifecycle.state_kind = 'book_lifecycle'
                        AND lifecycle.state_key = book.content_hash)
       AND rejected.hash IS NULL
       AND remote.hash IS NULL
       AND COALESCE(work.state = 'ready', 0)
       AND EXISTS(SELECT 1 FROM local_thumbnail_source source
                  WHERE source.content_hash = book.content_hash AND source.origin = 'local')
FROM json_each(:hashes) requested
LEFT JOIN book ON book.content_hash = requested.value AND book.deleted_at IS NULL
LEFT JOIN remote_asset remote ON remote.kind = 'thumbnail' AND remote.hash = book.content_hash
LEFT JOIN rejected_asset_upload rejected
  ON rejected.kind = 'thumbnail' AND rejected.hash = book.content_hash
 AND rejected.rejected_at > unixepoch() - 300
LEFT JOIN local_thumbnail_work work ON work.content_hash = book.content_hash;

-- name: transfer_snapshot_placements?
-- param: hashes: &str
SELECT b.content_hash,
       '/' || CASE WHEN paths.path = '' THEN bd.file_name ELSE paths.path || '/' || bd.file_name END
FROM json_each(:hashes) requested
JOIN book b ON b.content_hash = requested.value
JOIN book_dir bd ON bd.book_row_id = b.row_id AND bd.deleted_at IS NULL
JOIN dir_paths paths ON paths.id = bd.dir_id
WHERE b.deleted_at IS NULL
ORDER BY b.content_hash, bd.dir_id, bd.file_name;

-- Upload planning reads only intents whose current local preparation has
-- completed. Applying the limit after that readiness test prevents an
-- unprepared early intent from hiding later transferable books behind a
-- 32-item page boundary.
-- name: transfer_pending_book_uploads?
-- param: after: i64
-- param: limit: i64
SELECT upload.id, upload.content_hash, upload.checksum, upload.size_bytes FROM local_book_upload upload
JOIN local_upload_preparation preparation
  ON preparation.content_hash=upload.content_hash AND preparation.upload_id=upload.id
WHERE upload.id>:after
AND EXISTS(SELECT 1 FROM book b WHERE b.content_hash=upload.content_hash AND b.deleted_at IS NULL)
AND EXISTS(SELECT 1 FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id WHERE b.content_hash=upload.content_hash AND b.deleted_at IS NULL AND bd.deleted_at IS NULL)
AND NOT EXISTS(SELECT 1 FROM rejected_asset_upload rejected WHERE rejected.kind='book' AND rejected.hash=upload.content_hash AND rejected.rejected_at>unixepoch()-300)
ORDER BY upload.id LIMIT :limit;

-- Covers are derived work, not an intent queue.  Select them directly from
-- authoritative state so a stale `asset_work` notification can never make a
-- completed cover look pending (or prevent an eligible cover from running).
-- The upload snapshot repeats this predicate only as a last-moment safety
-- check before bytes are sent.
-- name: transfer_pending_cover_uploads?
-- param: limit: i64
SELECT book.content_hash
FROM book
JOIN local_thumbnail_work work
  ON work.content_hash = book.content_hash AND work.state = 'ready'
JOIN local_thumbnail_source source
  ON source.content_hash = book.content_hash AND source.origin = 'local'
JOIN remote_asset parent
  ON parent.kind = 'book' AND parent.hash = book.content_hash
LEFT JOIN remote_asset cover
  ON cover.kind = 'thumbnail' AND cover.hash = book.content_hash
LEFT JOIN rejected_asset_upload rejected
  ON rejected.kind = 'thumbnail' AND rejected.hash = book.content_hash
 AND rejected.rejected_at > unixepoch() - 300
WHERE book.deleted_at IS NULL
  AND cover.hash IS NULL
  AND rejected.hash IS NULL
  AND NOT EXISTS(
      SELECT 1 FROM sync_outbox lifecycle
      WHERE lifecycle.state_kind = 'book_lifecycle'
        AND lifecycle.state_key = book.content_hash
  )
ORDER BY book.content_hash
LIMIT :limit;

-- name: transfer_complete_upload!
-- param: id: i64
-- param: content_hash: &str
-- param: checksum: &str
DELETE FROM local_book_upload WHERE id=:id AND content_hash=:content_hash AND checksum=:checksum;

-- name: transfer_is_book_downloaded?
-- param: content_hash: &str
SELECT COUNT(*) > 0
FROM book_dir
WHERE book_row_id = (SELECT row_id FROM book WHERE content_hash=:content_hash)
  AND deleted_at is null
  AND is_downloaded = 1;

-- name: transfer_has_pending_file_work?
SELECT EXISTS(SELECT 1 FROM local_directory_work)
    OR EXISTS(SELECT 1 FROM local_file_work)
    OR EXISTS(SELECT 1 FROM local_book_work);

-- name: transfer_record_book_rejection!
-- param: content_hash: &str
-- param: reason: &str
INSERT INTO rejected_asset_upload(kind,hash,reason,rejected_at) VALUES ('book',:content_hash,:reason,unixepoch())
ON CONFLICT(kind,hash) DO UPDATE SET reason=excluded.reason,rejected_at=excluded.rejected_at;

-- name: transfer_record_thumbnail_rejection!
-- param: content_hash: &str
-- param: reason: &str
INSERT INTO rejected_asset_upload(kind,hash,reason,rejected_at) VALUES ('thumbnail',:content_hash,:reason,unixepoch())
ON CONFLICT(kind,hash) DO UPDATE SET reason=excluded.reason,rejected_at=excluded.rejected_at;

-- name: transfer_work_watermark ->
SELECT COALESCE(MAX(id),0) FROM asset_work;

-- Finding half of candidate planning: page identity and liveness only.
-- All allowance/state flags are checked separately in Rust over the page's
-- hashes, so each query below stays a plain existence check.
-- name: transfer_candidate_page?
-- param: after: i64
-- param: through: i64
SELECT w.id, w.content_hash, b.row_id, b.deleted_at FROM asset_work w
LEFT JOIN book b ON b.content_hash = w.content_hash
WHERE w.id > :after AND w.id <= :through ORDER BY w.id LIMIT 32;

-- Targeted drain entry: one asset_work row by hash for payload wakes.
-- Same shape as the page finder; shares the assemble path. Absent rows
-- (already settled) simply yield nothing.
-- name: transfer_candidate_row?
-- param: content_hash: &str
SELECT w.id, w.content_hash, b.row_id, b.deleted_at FROM asset_work w
LEFT JOIN book b ON b.content_hash = w.content_hash
WHERE w.content_hash = :content_hash;

-- Checking half of candidate planning: one allowance/state flag per hash,
-- each a plain existence check. No joins: every flag is answered by a single
-- indexed lookup, and directory work is intersected in Rust against the
-- walked ancestors instead.
-- name: transfer_candidate_flags?
-- param: hashes: &str
SELECT requested.value,
 NOT EXISTS(SELECT 1 FROM remote_asset remote WHERE remote.kind = 'thumbnail' AND remote.hash = requested.value)
   AND NOT EXISTS(SELECT 1 FROM rejected_asset_upload rejected WHERE rejected.kind = 'thumbnail' AND rejected.hash = requested.value AND rejected.rejected_at > unixepoch() - 300)
   AND EXISTS(SELECT 1 FROM local_thumbnail_work work WHERE work.content_hash = requested.value AND work.state = 'ready')
   AND EXISTS(SELECT 1 FROM local_thumbnail_source source WHERE source.content_hash = requested.value AND source.origin = 'local')
   -- Covers require the parent book's lifecycle record on the server. A
   -- confirmed blob alone is insufficient because the server validates its
   -- ownership from state sync.
   AND EXISTS(SELECT 1 FROM remote_asset book WHERE book.kind = 'book' AND book.hash = requested.value)
   AND NOT EXISTS(SELECT 1 FROM sync_outbox lifecycle WHERE lifecycle.state_kind = 'book_lifecycle' AND lifecycle.state_key = requested.value),
 EXISTS(SELECT 1 FROM remote_asset remote WHERE remote.kind = 'thumbnail' AND remote.hash = requested.value),
 EXISTS(SELECT 1 FROM local_thumbnail_work work WHERE work.content_hash = requested.value AND work.state = 'pending')
 OR (
     EXISTS(SELECT 1 FROM local_thumbnail_work work WHERE work.content_hash = requested.value AND work.state = 'ready')
     AND EXISTS(SELECT 1 FROM local_thumbnail_source source WHERE source.content_hash = requested.value AND source.origin = 'local')
     AND (
         NOT EXISTS(SELECT 1 FROM remote_asset book WHERE book.kind = 'book' AND book.hash = requested.value)
         OR EXISTS(SELECT 1 FROM sync_outbox lifecycle WHERE lifecycle.state_kind = 'book_lifecycle' AND lifecycle.state_key = requested.value)
     )
 ),
 EXISTS(SELECT 1 FROM download_requests request WHERE request.target_kind = 'book' AND request.target_key = requested.value),
 EXISTS(SELECT 1 FROM rejected_asset_upload rejected WHERE (rejected.kind = 'book' OR rejected.kind = 'thumbnail') AND rejected.hash = requested.value AND rejected.rejected_at > unixepoch() - 300),
 EXISTS(SELECT 1 FROM local_file_work work WHERE work.operation IN ('restore', 'trash') AND work.content_hash = requested.value)
FROM json_each(:hashes) requested;

-- name: transfer_settle_candidates!
-- param: candidates: &str
WITH RECURSIVE candidates AS MATERIALIZED (SELECT w.id,w.content_hash,json_extract(j.value,'$[1]') AS check_file_work FROM json_each(:candidates) j JOIN asset_work w ON w.id=json_extract(j.value,'$[0]')), ancestors(work_id,id) AS MATERIALIZED (
 SELECT c.id,bd.dir_id FROM candidates c JOIN book b ON b.content_hash=c.content_hash JOIN book_dir bd ON bd.book_row_id=b.row_id
 UNION SELECT c.id,f.dir_id FROM candidates c JOIN local_file_projection f ON f.content_hash=c.content_hash
 UNION SELECT a.work_id,d.parent_id FROM ancestors a JOIN dir d ON d.id=a.id WHERE d.id!=d.parent_id)
DELETE FROM asset_work WHERE id IN (SELECT c.id FROM candidates c WHERE NOT c.check_file_work OR (NOT EXISTS(SELECT 1 FROM local_book_work WHERE content_hash=c.content_hash) AND NOT EXISTS(SELECT 1 FROM local_file_work WHERE operation IN ('restore','trash') AND content_hash=c.content_hash) AND NOT EXISTS(SELECT 1 FROM ancestors a JOIN local_directory_work w ON w.dir_id=a.id WHERE a.work_id=c.id)));

-- Book paths are walked in Rust (see placement_paths); this query file no
-- longer needs its own copy of the directory recursion. The walk below
-- needs only single-row lookups, one per step.

-- name: transfer_dir_entry?
-- param: id: &str
SELECT parent_id, name, deleted_at FROM dir WHERE id = :id;

-- name: transfer_book_dirs?
-- param: book_row_id: i64
SELECT dir_id, file_name, deleted_at FROM book_dir WHERE book_row_id = :book_row_id;

-- name: transfer_projection_dirs?
-- param: content_hash: &str
SELECT dir_id FROM local_file_projection WHERE content_hash = :content_hash;

-- name: transfer_live_placements?
-- param: content_hash: &str
SELECT bd.dir_id,bd.file_name,'/' || CASE WHEN paths.path='' THEN bd.file_name ELSE paths.path || '/' || bd.file_name END FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id JOIN dir_paths paths ON paths.id=bd.dir_id WHERE b.content_hash=:content_hash AND b.deleted_at IS NULL AND bd.deleted_at IS NULL;

-- name: transfer_queue_restore!
-- param: content_hash: &str
-- param: relative_path: &str
INSERT OR IGNORE INTO local_file_work(operation,content_hash,relative_path) VALUES ('restore',:content_hash,:relative_path);

-- name: transfer_placement_matches ->
-- param: content_hash: &str
-- param: relative_path: &str
SELECT EXISTS(SELECT 1 FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id JOIN dir_paths paths ON paths.id=bd.dir_id WHERE b.content_hash=:content_hash AND b.deleted_at IS NULL AND bd.deleted_at IS NULL AND ('/' || CASE WHEN paths.path='' THEN bd.file_name ELSE paths.path || '/' || bd.file_name END)=:relative_path);

-- name: transfer_placement_directory?
-- param: content_hash: &str
-- param: relative_path: &str
SELECT bd.dir_id FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id JOIN dir_paths paths ON paths.id=bd.dir_id WHERE b.content_hash=:content_hash AND b.deleted_at IS NULL AND bd.deleted_at IS NULL AND ('/' || CASE WHEN paths.path='' THEN bd.file_name ELSE paths.path || '/' || bd.file_name END)=:relative_path;

-- name: transfer_download_placement?
-- param: content_hash: &str
SELECT EXISTS(SELECT 1 FROM book b JOIN book_dir bd ON bd.book_row_id=b.row_id WHERE b.content_hash=:content_hash AND b.deleted_at IS NULL AND bd.deleted_at IS NULL);

-- name: transfer_add_download_request!
-- param: target_key: &str
-- param: origin: &str
-- param: created_at: i64
INSERT INTO download_requests(target_kind, target_key, origin, created_at)
VALUES ('book', :target_key, :origin, :created_at)
ON CONFLICT(target_kind, target_key) DO UPDATE SET
    origin = CASE WHEN excluded.origin = 'user_initiated' THEN 'user_initiated' ELSE download_requests.origin END;

-- Download planning reads the request queue directly instead of deriving
-- from the asset_work drain. FIFO by insertion; user-facing work runs
-- ahead of bulk uploads.
-- name: transfer_pending_download_requests?
SELECT target_key FROM download_requests WHERE target_kind='book' ORDER BY rowid;

-- Durable download state in one row: bytes marked downloaded anywhere, and
-- a live placement with a queued request. Single-row existence checks only.
-- name: transfer_download_state?
-- param: content_hash: &str
SELECT
    EXISTS (
        SELECT 1
        FROM book_dir downloaded
        WHERE downloaded.book_row_id = (SELECT row_id FROM book WHERE content_hash = :content_hash)
          AND downloaded.deleted_at IS NULL
          AND downloaded.is_downloaded = 1
    ),
    EXISTS (
        SELECT 1
        FROM book_dir placement
        JOIN book ON book.row_id = placement.book_row_id
        JOIN download_requests request
          ON request.target_kind = 'book'
         AND request.target_key = book.content_hash
        WHERE book.content_hash = :content_hash
          AND placement.deleted_at IS NULL
    );

-- name: transfer_uploads_waiting_for_storage ->
SELECT EXISTS (
    SELECT 1 FROM rejected_asset_upload rejected
    JOIN local_book_upload upload ON upload.content_hash = rejected.hash
    WHERE rejected.kind = 'book'
      AND rejected.reason = 'server rejected upload admission: QuotaExceeded'
      AND rejected.rejected_at > unixepoch() - 300
);

-- name: transfer_first_file_names?
-- param: hashes: &str
SELECT requested.value, (SELECT bd.file_name FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id WHERE b.content_hash=requested.value ORDER BY bd.file_name LIMIT 1)
FROM json_each(:hashes) requested;

-- name: transfer_directory_work?
SELECT dir_id FROM local_directory_work;

-- name: transfer_book_upload_rejection_reason?
-- param: content_hash: &str
SELECT max(rejected.reason) FROM rejected_asset_upload rejected
JOIN local_book_upload upload ON upload.content_hash=rejected.hash
WHERE rejected.kind='book' AND rejected.hash=:content_hash AND rejected.rejected_at>unixepoch()-300

-- name: transfer_has_pending_asset_work?
WITH RECURSIVE live_dirs(id) AS (
            SELECT id FROM dir WHERE id='00000000-0000-0000-0000-000000000000' AND deleted_at IS NULL
            UNION ALL
            SELECT d.id FROM dir d JOIN live_dirs p ON d.parent_id=p.id AND d.id!=p.id WHERE d.deleted_at IS NULL
        ), live_books AS (
            SELECT b.content_hash,
                EXISTS(SELECT 1 FROM book_dir bd JOIN live_dirs d ON d.id=bd.dir_id
                    WHERE bd.book_row_id=b.row_id AND bd.deleted_at IS NULL AND bd.is_downloaded=1) AS local
            FROM book b WHERE b.content_hash!='' AND EXISTS(
                SELECT 1 FROM book_dir bd JOIN live_dirs d ON d.id=bd.dir_id
                WHERE bd.book_row_id=b.row_id AND bd.deleted_at IS NULL)
        )
        SELECT EXISTS(SELECT 1 FROM download_requests WHERE target_kind='book' AND origin='user_initiated')
        OR EXISTS(
            SELECT 1 FROM live_books b
            LEFT JOIN local_book_upload upload ON upload.content_hash=b.content_hash
            LEFT JOIN remote_asset rt ON rt.kind='thumbnail' AND rt.hash=b.content_hash
            LEFT JOIN rejected_asset_upload xb ON xb.kind='book' AND xb.hash=b.content_hash AND xb.rejected_at>unixepoch()-300
            LEFT JOIN rejected_asset_upload xt ON xt.kind='thumbnail' AND xt.hash=b.content_hash AND xt.rejected_at>unixepoch()-300
            LEFT JOIN local_thumbnail_work t ON t.content_hash=b.content_hash
            WHERE (b.local AND upload.id IS NOT NULL AND xb.hash IS NULL)
                OR (t.state='ready' AND rt.hash IS NULL AND xt.hash IS NULL AND EXISTS(SELECT 1 FROM local_thumbnail_source source WHERE source.content_hash=b.content_hash AND source.origin='local'))
                OR (COALESCE(t.state NOT IN ('ready','pending'),1) AND (NOT b.local OR rt.hash IS NOT NULL))
        )

-- name: book_row_id_select?
-- param: content_hash: &str
SELECT row_id FROM book WHERE content_hash = :content_hash;

-- name: record_remote_asset!
-- param: kind: &str
-- param: content_hash: &str
INSERT INTO remote_asset(kind, hash) VALUES (:kind, :content_hash) ON CONFLICT(kind, hash) DO NOTHING;

-- name: forget_remote_asset!
-- param: kind: &str
-- param: content_hash: &str
DELETE FROM remote_asset WHERE kind = :kind AND hash = :content_hash;

-- name: remote_asset_hashes?
-- param: kind: &str
SELECT hash FROM remote_asset WHERE kind = :kind;

-- name: add_asset_request!
-- param: kind: &str
-- param: content_hash: &str
-- param: origin: &str
-- param: created_at: i64
INSERT INTO download_requests(target_kind, target_key, origin, created_at) VALUES (:kind, :content_hash, :origin, :created_at)
ON CONFLICT(target_kind, target_key) DO UPDATE SET
    origin = CASE WHEN excluded.origin = 'user_initiated' THEN 'user_initiated' ELSE download_requests.origin END;

-- name: asset_requests?
-- param: kind: &str
SELECT target_key, origin FROM download_requests WHERE target_kind = :kind ORDER BY created_at, target_key;

-- name: rejected_uploads_prune!
-- param: cutoff: i64
DELETE FROM rejected_asset_upload WHERE rejected_at <= :cutoff;

-- name: rejected_upload_hashes?
-- param: kind: &str
-- param: cutoff: i64
SELECT hash FROM rejected_asset_upload WHERE kind = :kind AND rejected_at > :cutoff;

-- name: download_requested?
-- param: content_hash: &str
SELECT EXISTS (
        SELECT 1
        FROM book_dir placement
        JOIN book ON book.row_id = placement.book_row_id
        JOIN download_requests request
          ON request.target_kind = 'book'
         AND request.target_key = book.content_hash
        WHERE book.content_hash = :content_hash
          AND placement.deleted_at IS NULL
    );

-- name: asset_work_page?
-- param: after: i64
-- param: through: i64
SELECT id,content_hash FROM asset_work WHERE id>:after AND id<=:through ORDER BY id LIMIT 32;
