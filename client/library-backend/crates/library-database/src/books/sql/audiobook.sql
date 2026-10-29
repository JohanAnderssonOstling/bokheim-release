-- Cached audiobook playback metadata: chapters, duration, and recording narrator.

-- name: audiobook_playback_title_author?
-- param: content_hash: &BookId
SELECT COALESCE(title,'Untitled'), author FROM book WHERE content_hash = :content_hash;

-- name: audiobook_cached_metadata?
-- param: content_hash: &BookId
SELECT duration_ms, CAST(toc_json AS BLOB) FROM book
WHERE content_hash = :content_hash AND duration_ms > 0;

-- name: audiobook_record_recording!
-- param: content_hash: &BookId
-- param: request_json: &str
INSERT INTO book_local_state(content_hash, recording_json) SELECT content_hash, :request_json FROM book WHERE content_hash = :content_hash
ON CONFLICT(content_hash) DO UPDATE SET recording_json = excluded.recording_json WHERE book_local_state.recording_json IS NOT excluded.recording_json;

-- name: audiobook_chapters_select?
-- param: content_hash: &BookId
SELECT CAST(toc_json AS BLOB), duration_ms FROM book
WHERE content_hash=:content_hash AND duration_ms IS NOT NULL;

-- name: audiobook_track_index_select?
-- param: content_hash: &BookId
SELECT CAST(tracks_json AS BLOB) FROM book WHERE content_hash=:content_hash AND tracks_json IS NOT NULL;
