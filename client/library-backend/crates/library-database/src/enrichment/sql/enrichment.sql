-- Enrichment state snapshots and durable result accounting.

-- Audible lookup selection and claims. The caller performs the network lookup
-- after receiving a claim; it returns only the resulting response to commit.

-- name: audible_batch_candidates?
-- param: members: &str
-- param: now: i64
-- param: policy: &str
SELECT job.content_hash
FROM json_each(:members) member
JOIN audible_enrichment_jobs job ON job.content_hash = member.value
JOIN book ON book.content_hash = job.content_hash
WHERE job.policy = :policy AND book.deleted_at IS NULL AND book.hidden_at IS NULL
  AND ((job.status IN ('pending', 'unavailable') AND job.retry_at <= :now)
    OR (job.status = 'resolving' AND job.lease_until <= :now))
ORDER BY job.content_hash;

-- name: audible_claim_candidate?
-- param: content_hash: &str
-- param: now: i64
-- param: policy: &str
SELECT job.request_json
FROM audible_enrichment_jobs job
JOIN book ON book.content_hash = job.content_hash
WHERE job.content_hash = :content_hash
  AND job.policy = :policy
  AND book.deleted_at IS NULL AND book.hidden_at IS NULL
  AND ((job.status IN ('pending', 'unavailable') AND job.retry_at <= :now)
    OR (job.status = 'resolving' AND job.lease_until <= :now));

-- name: audible_claim!
-- param: content_hash: &str
-- param: token: &str
-- param: lease_until: i64
-- param: now: i64
UPDATE audible_enrichment_jobs
SET status = 'resolving', claim_token = :token, lease_until = :lease_until, updated_at = :now
WHERE content_hash = :content_hash;

-- name: audible_resolve!
-- param: content_hash: &str
-- param: token: &str
-- param: status: &str
-- param: response: &str
-- param: retry_at: i64
-- param: now: i64
UPDATE audible_enrichment_jobs
SET status = :status, response_json = :response, retry_at = :retry_at,
    claim_token = NULL, lease_until = NULL, updated_at = :now
WHERE content_hash = :content_hash AND claim_token = :token
  AND status = 'resolving' AND lease_until > :now;

-- name: audible_store_toc!
-- param: content_hash: &str
-- param: entry_count: i64
-- param: toc: &str
INSERT INTO book_toc(content_hash, entry_count, toc_json) VALUES (:content_hash, :entry_count, :toc)
ON CONFLICT(content_hash) DO UPDATE SET entry_count = excluded.entry_count, toc_json = excluded.toc_json;

-- name: audible_update_book_metadata!
-- param: content_hash: &str
-- param: metadata: &[u8]
UPDATE book SET book_metadata = :metadata
WHERE content_hash = :content_hash AND book_metadata IS NOT :metadata;

-- name: audible_clear_identifiers!
-- param: content_hash: &str
DELETE FROM book_identifier WHERE book_row_id = (SELECT row_id FROM book WHERE content_hash = :content_hash);

-- name: audible_insert_identifier!
-- param: content_hash: &str
-- param: position: i64
-- param: scheme: &str
-- param: value: &str
-- param: canonical_value: Option<&str>
-- param: scope: &str
INSERT INTO book_identifier(book_row_id, position, scheme, value, canonical_value, scope)
SELECT row_id, :position, :scheme, :value, :canonical_value, :scope
FROM book WHERE content_hash = :content_hash;

-- name: enrichment_record_attempt!
-- param: content_hash: &str
-- param: provider: &str
-- param: identifier: &str
-- param: status: &str
-- param: detail: Option<&str>
INSERT INTO external_metadata_attempt(book_row_id, provider_id, identifier, status, detail, attempted_at)
VALUES ((SELECT row_id FROM book WHERE content_hash = :content_hash), :provider, :identifier, :status, :detail, CAST(strftime('%s', 'now') AS INTEGER) * 1000)
ON CONFLICT(book_row_id, provider_id) DO UPDATE SET
    identifier = excluded.identifier,
    status = excluded.status,
    detail = excluded.detail,
    attempted_at = excluded.attempted_at;

-- name: enrichment_set_missing_description!
-- param: content_hash: &str
-- param: description: &str
INSERT INTO book_description(book_row_id, description)
SELECT b.row_id, :description
FROM book b
WHERE b.content_hash = :content_hash
  AND trim(:description) <> ''
  AND NOT EXISTS(
      SELECT 1 FROM book_description current
      WHERE current.book_row_id = b.row_id AND trim(current.description) <> ''
  )
ON CONFLICT(book_row_id) DO UPDATE SET description = excluded.description
WHERE trim(book_description.description) = '';

-- name: thumbnail_pending_job_count ->
SELECT COUNT(*) FROM local_thumbnail_work w JOIN book b ON b.content_hash=w.content_hash WHERE w.state = 'pending' AND b.deleted_at IS NULL;

-- name: thumbnail_pending_jobs?
-- param: after: &str
SELECT content_hash, retry_after FROM local_thumbnail_work
WHERE state = 'pending' AND content_hash > :after AND EXISTS(SELECT 1 FROM book b WHERE b.content_hash=local_thumbnail_work.content_hash AND b.deleted_at IS NULL)
ORDER BY content_hash LIMIT 32;

-- name: thumbnail_local_format?
-- param: content_hash: &str
SELECT b.format
FROM book b
WHERE b.content_hash = :content_hash
  AND EXISTS(
    SELECT 1 FROM book_dir bd JOIN dir d ON d.id = bd.dir_id
    WHERE bd.book_row_id = b.row_id AND bd.deleted_at IS NULL AND d.deleted_at IS NULL
  );

-- name: thumbnail_record_remote!
-- param: content_hash: &str
INSERT INTO remote_asset(kind, hash) VALUES ('thumbnail', :content_hash)
ON CONFLICT(kind, hash) DO NOTHING;

-- name: thumbnail_forget_remote!
-- param: content_hash: &str
DELETE FROM remote_asset WHERE kind = 'thumbnail' AND hash = :content_hash;

-- name: thumbnail_has_remote ->
-- param: content_hash: &str
SELECT EXISTS(SELECT 1 FROM remote_asset WHERE kind = 'thumbnail' AND hash = :content_hash);

-- name: thumbnail_download_coverage ->
-- Count known cloud thumbnails for visible books and those already ready here.
SELECT COUNT(CASE WHEN work.state = 'ready' THEN 1 END), COUNT(*)
FROM remote_asset remote
JOIN book ON book.content_hash = remote.hash
LEFT JOIN local_thumbnail_work work ON work.content_hash = remote.hash
WHERE remote.kind = 'thumbnail' AND book.deleted_at IS NULL AND book.hidden_at IS NULL;

-- name: thumbnail_confirm_source!
-- param: content_hash: &str
-- param: origin: &str
INSERT INTO local_thumbnail_source(content_hash, origin) VALUES (:content_hash, :origin)
ON CONFLICT(content_hash) DO UPDATE SET origin = excluded.origin;

-- name: thumbnail_sidecar_exists?
-- param: content_hash: &str
SELECT EXISTS(SELECT 1 FROM local_thumbnail_sidecar WHERE content_hash = :content_hash);

-- name: thumbnail_sidecar_mark!
-- param: content_hash: &str
INSERT OR IGNORE INTO local_thumbnail_sidecar(content_hash) VALUES (:content_hash);

-- name: thumbnail_sidecar_clear!
-- param: content_hash: &str
DELETE FROM local_thumbnail_sidecar WHERE content_hash = :content_hash;

-- name: thumbnail_sync_pending_set!
-- param: content_hash: &str
-- param: action: &str
INSERT INTO local_thumbnail_sync_pending(content_hash, action) VALUES (:content_hash, :action)
ON CONFLICT(content_hash) DO UPDATE SET action = excluded.action, generation = local_thumbnail_sync_pending.generation + 1;

-- name: thumbnail_sync_pending_page?
-- param: after: &str
SELECT pending.content_hash, pending.action, pending.generation FROM local_thumbnail_sync_pending pending
JOIN book b ON b.content_hash = pending.content_hash
WHERE pending.content_hash > :after AND b.deleted_at IS NULL
ORDER BY pending.content_hash LIMIT 64;

-- name: thumbnail_sync_pending_clear!
-- param: content_hash: &str
-- param: generation: i64
DELETE FROM local_thumbnail_sync_pending WHERE content_hash = :content_hash AND generation = :generation;

-- name: thumbnail_remote_revision?
-- param: content_hash: &str
SELECT revision FROM remote_thumbnail_revision WHERE content_hash = :content_hash;

-- name: thumbnail_remote_revision_set!
-- param: content_hash: &str
-- param: revision: &str
INSERT INTO remote_thumbnail_revision(content_hash, revision) VALUES (:content_hash, :revision)
ON CONFLICT(content_hash) DO UPDATE SET revision = excluded.revision;

-- name: thumbnail_remote_revision_clear!
-- param: content_hash: &str
DELETE FROM remote_thumbnail_revision WHERE content_hash = :content_hash;

-- name: thumbnail_source_origin?
-- param: content_hash: &str
SELECT origin FROM local_thumbnail_source WHERE content_hash = :content_hash;

-- name: thumbnail_remote_presence_candidates?
-- param: after: &str
SELECT DISTINCT b.content_hash FROM book b
JOIN book_dir bd ON bd.book_row_id = b.row_id AND bd.deleted_at IS NULL
JOIN dir d ON d.id = bd.dir_id AND d.deleted_at IS NULL
WHERE b.content_hash > :after AND b.deleted_at IS NULL AND b.hidden_at IS NULL
ORDER BY b.content_hash LIMIT 512;

-- name: thumbnail_set_result!
-- param: content_hash: &str
-- param: state: &str
INSERT INTO local_thumbnail_work(content_hash, state) VALUES (:content_hash, :state)
ON CONFLICT(content_hash) DO UPDATE SET state = excluded.state, retry_after = 0;

-- name: thumbnail_retry!
-- param: content_hash: &str
-- param: retry_after: i64
INSERT INTO local_thumbnail_work(content_hash, state, retry_after) VALUES (:content_hash, 'pending', :retry_after)
ON CONFLICT(content_hash) DO UPDATE SET state = 'pending', retry_after = excluded.retry_after;

-- name: thumbnail_discard!
-- param: content_hash: &str
DELETE FROM local_thumbnail_work WHERE content_hash = :content_hash;

-- name: thumbnail_request_missing!
-- param: content_hash: &str
INSERT INTO local_thumbnail_work(content_hash, state) VALUES (:content_hash, 'pending')
ON CONFLICT(content_hash) DO UPDATE SET state = 'pending', retry_after = 0
WHERE local_thumbnail_work.state = 'ready';

-- name: thumbnail_work_state?
-- param: content_hash: &str
SELECT state, retry_after FROM local_thumbnail_work WHERE content_hash = :content_hash;

-- name: thumbnail_enqueue_asset_work!
-- param: content_hash: &str
INSERT INTO asset_work(content_hash) VALUES (:content_hash)
ON CONFLICT(content_hash) DO UPDATE SET id = excluded.id;

-- name: thumbnail_upload_allowed ->
-- param: content_hash: &str
SELECT upload.id IS NOT NULL
   AND rejected.hash IS NULL
   AND remote.hash IS NULL
   AND COALESCE(work.state = 'ready', 0)
   AND EXISTS(SELECT 1 FROM local_thumbnail_source source WHERE source.content_hash = book.content_hash AND source.origin = 'local')
FROM book
LEFT JOIN local_book_upload upload ON upload.content_hash = book.content_hash
LEFT JOIN remote_asset remote ON remote.kind = 'thumbnail' AND remote.hash = book.content_hash
LEFT JOIN rejected_asset_upload rejected ON rejected.kind = 'thumbnail' AND rejected.hash = book.content_hash AND rejected.rejected_at > unixepoch() - 300
LEFT JOIN local_thumbnail_work work ON work.content_hash = book.content_hash
WHERE book.content_hash = :content_hash AND book.deleted_at IS NULL;

-- Subject and external metadata selection.
-- name: enrichment_subject_candidates?
-- param: after: &str
-- param: scope: &str
-- param: revision: i64
SELECT b.content_hash,COALESCE(b.title,''),b.format,b.book_metadata,CASE WHEN s.metadata=b.book_metadata AND s.title=COALESCE(b.title,'') AND s.revision=:revision AND (s.complete=0 OR s.retry_at=0) THEN s.phase ELSE NULL END
FROM book b LEFT JOIN local_subject_pipeline s USING(content_hash)
WHERE b.content_hash>:after AND b.deleted_at IS NULL AND b.hidden_at IS NULL AND b.content_hash IN (SELECT value FROM json_each(:scope))
AND NOT EXISTS(SELECT 1 FROM pending_audible_enrichment a WHERE a.content_hash=b.content_hash)
AND (s.content_hash IS NULL OR s.metadata<>b.book_metadata OR s.title<>COALESCE(b.title,'') OR s.revision<>:revision OR ((s.complete=0 OR s.retry_at>0) AND s.retry_at<=unixepoch()))
ORDER BY b.content_hash LIMIT 32;

-- name: enrichment_book?
-- param: content_hash: &str
SELECT book_metadata FROM book WHERE content_hash=:content_hash;

-- name: enrichment_step_succeeded?
-- param: content_hash: &str
-- param: provider: &str
SELECT status <> 'failed' FROM external_metadata_attempt WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND provider_id=:provider;

-- name: enrichment_cached_isbn?
-- param: isbn: &str
SELECT response FROM local_isbn_response WHERE isbn=:isbn AND expires_at>unixepoch();

-- name: enrichment_thumbnail_state?
-- param: content_hash: &str
SELECT state,retry_after FROM local_thumbnail_work WHERE content_hash=:content_hash;

-- name: enrichment_book_format?
-- param: content_hash: &str
SELECT format FROM book WHERE content_hash=:content_hash AND deleted_at IS NULL;

-- name: enrichment_description_candidates?
-- param: provider: &str
-- param: after: &str
-- param: scope: &str
WITH lookup_isbn AS (SELECT book_row_id,canonical_value FROM book_identifier WHERE scheme='isbn' AND canonical_value IS NOT NULL), candidates AS (
 SELECT b.row_id,b.content_hash,group_concat(DISTINCT i.canonical_value ORDER BY i.canonical_value) isbns FROM book b JOIN lookup_isbn i ON i.book_row_id=b.row_id
 WHERE b.deleted_at IS NULL AND b.hidden_at IS NULL AND trim(COALESCE((SELECT description FROM book_description WHERE book_row_id=b.row_id),''))='' AND b.content_hash>:after AND b.content_hash IN (SELECT value FROM json_each(:scope)) AND NOT EXISTS(SELECT 1 FROM pending_audible_enrichment a WHERE a.content_hash=b.content_hash) GROUP BY b.row_id)
SELECT c.content_hash,c.isbns FROM candidates c LEFT JOIN external_metadata_attempt a ON a.book_row_id=c.row_id AND a.provider_id=:provider
WHERE a.book_row_id IS NULL OR a.identifier<>c.isbns OR a.status='updated' OR a.attempted_at<unixepoch()*1000-2592000000 OR (a.status='failed' AND a.attempted_at<unixepoch()*1000-60000) ORDER BY c.content_hash LIMIT 32;

-- name: enrichment_cover_candidates?
-- param: provider: &str
-- param: after: &str
-- param: scope: &str
WITH candidates AS (
 SELECT b.row_id,b.content_hash,w.state,(SELECT group_concat(canonical_value,',') FROM (SELECT DISTINCT canonical_value FROM book_identifier WHERE book_row_id=b.row_id AND scheme='isbn' AND canonical_value IS NOT NULL ORDER BY canonical_value)) isbns FROM book b JOIN local_thumbnail_work w ON w.content_hash=b.content_hash
 WHERE b.content_hash>:after AND b.content_hash IN (SELECT value FROM json_each(:scope)) AND (w.state='no_cover' OR (w.state='ready' AND lower(b.format)='m4b')) AND b.deleted_at IS NULL AND b.hidden_at IS NULL AND NOT EXISTS(SELECT 1 FROM pending_audible_enrichment a WHERE a.content_hash=b.content_hash))
SELECT c.content_hash,c.isbns FROM candidates c LEFT JOIN external_metadata_attempt a ON a.book_row_id=c.row_id AND a.provider_id=:provider WHERE c.isbns IS NOT NULL AND (a.book_row_id IS NULL OR a.identifier<>c.isbns OR (a.status='updated' AND c.state='no_cover') OR (a.status='failed' AND a.attempted_at<=unixepoch()*1000-30000)) ORDER BY c.content_hash LIMIT 32;

-- name: enrichment_author_candidates?
-- param: provider: &str
-- param: after: &str
-- param: scope: &str
WITH lookup_isbn AS (SELECT book_row_id,canonical_value FROM book_identifier WHERE scheme='isbn' AND canonical_value IS NOT NULL)
SELECT b.content_hash,group_concat(DISTINCT i.canonical_value) FROM book b JOIN lookup_isbn i ON i.book_row_id=b.row_id
WHERE b.content_hash>:after AND b.content_hash IN (SELECT value FROM json_each(:scope)) AND b.deleted_at IS NULL AND b.hidden_at IS NULL AND NOT EXISTS(SELECT 1 FROM pending_audible_enrichment a WHERE a.content_hash=b.content_hash)
AND NOT EXISTS(SELECT 1 FROM external_metadata_attempt attempt WHERE attempt.book_row_id=b.row_id AND attempt.provider_id=:provider AND attempt.attempted_at>=unixepoch()*1000-2592000000 AND attempt.status<>'failed') GROUP BY b.row_id ORDER BY b.content_hash LIMIT 32;

-- name: enrichment_author_credits?
-- param: content_hash: &str
SELECT position,name FROM book_contributor WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND role=X'617574' ORDER BY position;

-- name: enrichment_identity_input?
-- param: content_hash: &str
SELECT COALESCE(title,''),book_metadata FROM book WHERE content_hash=:content_hash AND deleted_at IS NULL AND hidden_at IS NULL;

-- name: enrichment_identity_authors?
-- param: content_hash: &str
SELECT name FROM book_contributor WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND role=X'617574' ORDER BY position;

-- name: enrichment_identity_filenames?
-- param: content_hash: &str
SELECT DISTINCT file_name FROM book_dir
WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash)
AND deleted_at IS NULL ORDER BY file_name LIMIT 8;

-- name: upload_preparation_snapshot?
SELECT b.content_hash, u.id
FROM book b LEFT JOIN local_book_upload u ON u.content_hash = b.content_hash
WHERE b.deleted_at IS NULL
ORDER BY b.content_hash;

-- name: prepared_upload_batch?
-- param: scope: &str
WITH RECURSIVE affected_dirs(id) AS (
    SELECT dir_id FROM local_directory_work
    UNION SELECT d.id FROM dir d JOIN affected_dirs a ON d.parent_id = a.id WHERE d.id != d.parent_id
)
SELECT b.content_hash, u.id
FROM json_each(:scope) members JOIN book b ON b.content_hash = members.value
LEFT JOIN local_book_upload u ON u.content_hash = b.content_hash
WHERE b.deleted_at IS NULL
  AND NOT EXISTS(SELECT 1 FROM local_book_work w WHERE w.content_hash = b.content_hash)
  AND NOT EXISTS(SELECT 1 FROM local_file_work w WHERE w.content_hash = b.content_hash AND w.operation IN ('restore', 'trash'))
  AND NOT EXISTS(SELECT 1 FROM book_dir bd JOIN affected_dirs d ON d.id = bd.dir_id WHERE bd.book_row_id = b.row_id)
  AND NOT EXISTS(SELECT 1 FROM local_file_projection fp JOIN affected_dirs d ON d.id = fp.dir_id WHERE fp.content_hash = b.content_hash)
ORDER BY b.content_hash;

-- name: upload_thumbnail_candidates?
-- param: scope: &str
-- param: now: i64
SELECT w.content_hash
FROM json_each(:scope) members
JOIN local_thumbnail_work w ON w.content_hash = members.value
WHERE w.state = 'pending' AND w.retry_after <= :now
ORDER BY w.content_hash;

-- name: finish_upload_preparation!
-- param: members: &str
INSERT INTO local_upload_preparation(content_hash, upload_id)
SELECT b.content_hash, json_extract(member.value, '$[1]')
FROM json_each(:members) member
JOIN book b ON b.content_hash = json_extract(member.value, '$[0]')
WHERE b.deleted_at IS NULL
ON CONFLICT(content_hash) DO UPDATE SET upload_id = excluded.upload_id
WHERE local_upload_preparation.upload_id IS NOT excluded.upload_id;

-- name: rich_fallback_codes?
-- param: content_hash: &str
SELECT LOWER(COALESCE(authority,'')),COALESCE(code,name),source
                     FROM book_subject
                     WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash)
                       AND LOWER(COALESCE(authority,''))='lcc'

-- name: author_resolve_credit?
-- param: content_hash: &str
-- param: position: i64
SELECT author_identity.id, author_identity.stable_id, author_identity.is_provisional
         FROM book_contributor JOIN author_identity ON author_identity.id=book_contributor.author_identity_id
         WHERE book_contributor.book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND book_contributor.position=:position AND book_contributor.role=X'617574'

-- name: author_find_external_identity?
-- param: authority: &str
-- param: external_id: &str
SELECT author_identity.id, author_identity.stable_id
             FROM author_external_identifier JOIN author_identity ON author_identity.id=author_external_identifier.author_identity_id
             WHERE author_external_identifier.authority=:authority AND author_external_identifier.external_id=:external_id

-- name: author_find_stable_identity?
-- param: stable_id: String
SELECT id,stable_id FROM author_identity WHERE stable_id=:stable_id

-- name: author_credit_count?
-- param: local_id: i64
SELECT COUNT(*) FROM book_contributor WHERE author_identity_id=:local_id

-- name: author_update_stable_id!
-- param: stable_id: String
-- param: local_id: i64
UPDATE author_identity SET stable_id=:stable_id WHERE id=:local_id

-- name: author_insert_identity!
-- param: stable_id: String
-- param: name: &str
-- param: match_key: &str
-- param: created_at: i64
INSERT INTO author_identity(stable_id, preferred_name, normalized_name, is_provisional, created_at) VALUES(:stable_id, :name, :match_key, 0, :created_at)

-- name: author_update_identity_names!
-- param: name: &str
-- param: match_key: &str
-- param: local_id: i64
UPDATE author_identity SET preferred_name=:name, normalized_name=:match_key, is_provisional=0 WHERE id=:local_id

-- name: author_attach_external_id!
-- param: local_id: i64
-- param: authority: &str
-- param: value: &str
-- param: verified_at: i64
INSERT OR IGNORE INTO author_external_identifier(author_identity_id, authority, external_id, verified_at) VALUES(:local_id, :authority, :value, :verified_at)

-- name: author_reidentify_credit!
-- param: identity_local_id: i64
-- param: content_hash: &str
-- param: position: i64
UPDATE book_contributor SET author_identity_id=:identity_local_id WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND position=:position AND role=X'617574'

-- name: author_delete_identity!
-- param: local_id: i64
DELETE FROM author_identity
             WHERE id=:local_id AND is_provisional=1
               AND NOT EXISTS(SELECT 1 FROM book_contributor WHERE author_identity_id=:local_id)
               AND NOT EXISTS(SELECT 1 FROM author_external_identifier WHERE author_identity_id=:local_id)

-- name: author_rich_already_applied?
-- param: content_hash: &str
-- param: provider: &str
-- param: identifier: &str
SELECT EXISTS(SELECT 1 FROM external_metadata_attempt a JOIN book b ON b.row_id=a.book_row_id WHERE b.content_hash=:content_hash AND a.provider_id=:provider AND a.identifier=:identifier AND a.status='updated')

-- name: save_subject_pipeline_checkpoint!
-- param: content_hash: &str
-- param: phase: i64
-- param: revision: i64
-- param: complete: bool
-- param: retry_seconds: i64
-- param: expected: &[u8]
-- param: title: &str
-- param: empty_metadata: bool
INSERT INTO local_subject_pipeline(content_hash, phase, revision, metadata, title, complete, retry_at)
SELECT content_hash, :phase, :revision, book_metadata, COALESCE(title, ''), :complete,
       CASE WHEN :complete = 1 AND :retry_seconds = 0 THEN 0 ELSE CAST(strftime('%s', 'now') AS INTEGER) + :retry_seconds END
FROM book WHERE content_hash = :content_hash AND deleted_at IS NULL
  AND (book_metadata = :expected OR (:empty_metadata AND length(book_metadata) = 0))
  AND COALESCE(title, '') = :title
ON CONFLICT(content_hash) DO UPDATE SET phase = excluded.phase, revision = excluded.revision, metadata = excluded.metadata,
    title = excluded.title, complete = excluded.complete, retry_at = excluded.retry_at;

-- name: cache_isbn_response!
-- param: isbn: &str
-- param: response: &str
INSERT INTO local_isbn_response VALUES (:isbn, :response, CAST(strftime('%s', 'now') AS INTEGER) + 2592000)
ON CONFLICT(isbn) DO UPDATE SET response = excluded.response, expires_at = excluded.expires_at;

-- name: trim_isbn_response_cache!
DELETE FROM local_isbn_response
WHERE isbn IN (SELECT isbn FROM local_isbn_response ORDER BY expires_at DESC, isbn LIMIT -1 OFFSET 4096);

-- name: author_non_author_credits?
-- param: content_hash: &str
SELECT ai.stable_id,bc.name,bc.role FROM book_contributor bc JOIN author_identity ai ON ai.id=bc.author_identity_id WHERE bc.book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND bc.role<>X'617574' ORDER BY bc.position

-- name: audible_retry_job_count?
-- param: policy: &str
SELECT COUNT(*) FROM audible_enrichment_jobs j JOIN book b USING(content_hash)
WHERE j.policy=:policy AND j.status IN ('pending','resolving','unavailable')
AND b.deleted_at IS NULL AND b.hidden_at IS NULL;

-- name: audiobook_enrichment_status?
SELECT b.title,j.status,j.response_json,j.retry_at,j.updated_at
FROM audible_enrichment_jobs j JOIN book b USING(content_hash)
WHERE b.deleted_at IS NULL AND b.hidden_at IS NULL ORDER BY b.title;

-- name: refresh_audiobook_enrichment!
-- param: content_hash: &str
-- param: now: i64
-- param: policy: &str
UPDATE audible_enrichment_jobs SET status='pending',retry_at=0,policy=:policy,claim_token=NULL,lease_until=NULL
WHERE content_hash=:content_hash AND (status<>'resolving' OR lease_until<=:now)
AND content_hash IN (SELECT content_hash FROM book WHERE deleted_at IS NULL AND hidden_at IS NULL);

-- name: enrichment_audible_cover_identity?
-- param: hash: &str
SELECT json_extract(response_json,'$.selected_asin'),json_extract(response_json,'$.selected_region')
FROM audible_enrichment_jobs j JOIN book b USING(content_hash)
WHERE content_hash=:hash AND j.status='applied' AND b.hidden_at IS NULL AND b.deleted_at IS NULL AND lower(b.format) IN ('m4b','mp3folder')
AND json_extract(response_json,'$.status')='supported'
AND EXISTS(SELECT 1 FROM json_each(response_json,'$.candidates') c WHERE json_extract(c.value,'$.asin')=json_extract(response_json,'$.selected_asin') AND json_extract(c.value,'$.region')=json_extract(response_json,'$.selected_region'));

-- name: enrichment_audible_cover_candidates?
-- param: provider: &str
-- param: after: &str
-- param: scope: &str
SELECT b.content_hash,json_extract(j.response_json,'$.selected_asin'),json_extract(j.response_json,'$.selected_region')
FROM book b JOIN local_thumbnail_work w USING(content_hash) JOIN audible_enrichment_jobs j USING(content_hash)
LEFT JOIN external_metadata_attempt a ON a.book_row_id=b.row_id AND a.provider_id=:provider
WHERE b.content_hash>:after AND b.content_hash IN (SELECT value FROM json_each(:scope))
AND b.hidden_at IS NULL AND b.deleted_at IS NULL AND lower(b.format) IN ('m4b','mp3folder') AND w.state='no_cover'
AND j.status='applied' AND json_extract(j.response_json,'$.status')='supported'
AND json_extract(j.response_json,'$.selected_asin') IS NOT NULL AND json_extract(j.response_json,'$.selected_region') IS NOT NULL
AND (a.book_row_id IS NULL OR a.identifier<>json_extract(j.response_json,'$.selected_region')||':'||json_extract(j.response_json,'$.selected_asin') OR a.status='updated' OR (a.status='failed' AND a.attempted_at<=unixepoch()*1000-30000))
ORDER BY b.content_hash LIMIT 32;
