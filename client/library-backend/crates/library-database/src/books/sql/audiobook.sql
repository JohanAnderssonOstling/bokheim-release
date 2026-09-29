-- Cached audiobook playback metadata: chapters, duration, and recording narrator.

-- name: audiobook_playback_title_author?
-- param: content_hash: &str
SELECT COALESCE(title,'Untitled'), COALESCE(author,'') FROM book_cards WHERE content_hash = :content_hash;

-- name: audiobook_cached_metadata?
-- param: content_hash: &str
SELECT a.duration_ms, toc.toc_json
FROM audiobook_metadata a
JOIN book_toc toc ON toc.content_hash = a.content_hash
WHERE a.content_hash = :content_hash AND a.duration_ms > 0;

-- name: audiobook_recording_evidence?
-- param: content_hash: &str
SELECT request_json FROM audible_enrichment_jobs WHERE content_hash = :content_hash;

-- name: audiobook_applied_request?
-- param: content_hash: &str
SELECT request_json FROM audible_enrichment_jobs WHERE content_hash = :content_hash AND status = 'applied';

-- name: audiobook_discover_job!
-- param: content_hash: &str
-- param: policy: &str
-- param: request_json: &str
-- param: now: i64
INSERT INTO audible_enrichment_jobs(content_hash,policy,request_json,status,updated_at)
VALUES(:content_hash,:policy,:request_json,'pending',:now)
ON CONFLICT(content_hash) DO UPDATE SET policy=excluded.policy,request_json=excluded.request_json,status='pending',response_json=NULL,
claim_token=NULL,lease_until=NULL,retry_at=0,updated_at=excluded.updated_at
WHERE audible_enrichment_jobs.policy<>excluded.policy OR audible_enrichment_jobs.request_json<>excluded.request_json;

-- name: audiobook_chapters_select?
-- param: content_hash: &str
SELECT toc.toc_json, metadata.duration_ms FROM book_toc toc
JOIN audiobook_metadata metadata ON metadata.content_hash = toc.content_hash
WHERE toc.content_hash=:content_hash;

-- name: audiobook_upsert_metadata!
-- param: content_hash: &str
-- param: duration_ms: i64
INSERT INTO audiobook_metadata(content_hash,duration_ms)
VALUES(:content_hash,:duration_ms)
ON CONFLICT(content_hash) DO UPDATE SET
    duration_ms=excluded.duration_ms;

-- name: audiobook_upsert_tracks!
-- param: content_hash: &str
-- param: tracks_json: &str
INSERT INTO audiobook_track_index(content_hash,tracks_json)
VALUES(:content_hash,:tracks_json)
ON CONFLICT(content_hash) DO UPDATE SET tracks_json=excluded.tracks_json;

-- name: audiobook_track_index_select?
-- param: content_hash: &str
SELECT tracks_json FROM audiobook_track_index WHERE content_hash=:content_hash;
