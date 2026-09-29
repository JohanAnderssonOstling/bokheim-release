-- Import commit writes.

-- name: audible_discover_insert!
-- param: content_hash: &str
-- param: policy: &str
-- param: request_json: &str
-- param: updated_at: i64
INSERT INTO audible_enrichment_jobs(content_hash,policy,request_json,status,updated_at)
        VALUES(:content_hash,:policy,:request_json,'pending',:updated_at)
        ON CONFLICT(content_hash) DO UPDATE SET policy=excluded.policy,request_json=excluded.request_json,status='pending',response_json=NULL,
        claim_token=NULL,lease_until=NULL,retry_at=0,updated_at=excluded.updated_at
        WHERE audible_enrichment_jobs.policy<>excluded.policy OR audible_enrichment_jobs.request_json<>excluded.request_json;

-- name: audiobook_applied_request_select?
-- param: content_hash: &str
SELECT request_json FROM audible_enrichment_jobs WHERE content_hash=:content_hash AND status='applied';

-- name: batch_flag_flush&
SELECT bokheim_accept_local(bokheim_mutation('metadata',b.content_hash,COALESCE(b.title,''),b.subtitle,contributors_for_book(b.row_id),b.book_metadata), CAST((julianday('now')-2440587.5)*86400000 AS INTEGER)) FROM pending_metadata_payload p JOIN book b ON b.row_id=p.book_row_id WHERE 1;
            DELETE FROM pending_metadata_payload;

-- name: existing_book_contributor_credit?
-- param: content_hash: &str
-- param: position: i64
SELECT author_identity.id, author_identity.stable_id, book_contributor.name
FROM book_contributor JOIN author_identity ON author_identity.id=book_contributor.author_identity_id
WHERE book_contributor.book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND book_contributor.position=:position;

-- name: has_import_inspection_select?
-- param: content_hash: &str
-- param: format: &str
-- param: version: i64
SELECT EXISTS(
    SELECT 1 FROM local_import_inspection i JOIN book b USING(content_hash)
    WHERE i.content_hash = :content_hash AND i.format = :format AND i.version = :version
      AND b.format = :format AND b.deleted_at IS NULL
);

-- name: live_destination_select?
-- param: dir_id: &str
SELECT EXISTS(SELECT 1 FROM dir_paths WHERE id=:dir_id);

-- name: provisional_author_identity_by_name?
-- param: normalized_name: &str
SELECT id, stable_id
FROM author_identity
WHERE normalized_name=:normalized_name AND is_provisional=1
ORDER BY id LIMIT 1;

-- name: publish_conflict_select?
-- param: dir_id: &str
-- param: file_name_key: &str
-- param: content_hash: &str
SELECT EXISTS(SELECT 1 FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id WHERE bd.dir_id=:dir_id AND portable_name_key(bd.file_name)=:file_name_key AND b.content_hash!=:content_hash AND bd.deleted_at IS NULL);

-- name: publish_update_2!
-- param: dir_id: &str
-- param: content_hash: &str
-- param: local_hash: &str
UPDATE book_dir SET local_hash=:local_hash WHERE dir_id=:dir_id AND book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash);

-- name: record_import_inspection!
-- param: content_hash: &str
-- param: format: &str
-- param: version: i64
INSERT INTO local_import_inspection(content_hash, format, version)
VALUES (:content_hash, :format, :version)
ON CONFLICT(content_hash) DO UPDATE SET format = excluded.format, version = excluded.version;

-- name: restore_candidates_select?
-- param: content_hash: &str
SELECT '/' || CASE WHEN dp.path='' THEN bd.file_name ELSE dp.path || '/' || bd.file_name END,p.relative_path
             FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id
             JOIN dir_paths dp ON dp.id=bd.dir_id
             LEFT JOIN local_file_projection p ON p.content_hash=b.content_hash AND p.dir_id=bd.dir_id
             WHERE b.content_hash=:content_hash AND b.deleted_at IS NULL AND bd.deleted_at IS NULL;

-- name: scan_seq_update!
-- param: scan_seq: i64
INSERT INTO sync_metadata(singleton, scan_seq) VALUES (1, :scan_seq) ON CONFLICT(singleton) DO UPDATE SET scan_seq = excluded.scan_seq;

-- name: set_description!
-- param: content_hash: &str
-- param: description: &str
INSERT INTO book_description(book_row_id, description)
SELECT row_id, :description FROM book WHERE content_hash = :content_hash
  AND (:description <> '' OR EXISTS(SELECT 1 FROM book_description WHERE book_row_id = book.row_id))
ON CONFLICT(book_row_id) DO UPDATE SET description = excluded.description
WHERE book_description.description IS NOT excluded.description;

