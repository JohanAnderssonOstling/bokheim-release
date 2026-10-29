-- Project canonical state into book, directory and reader tables.

-- name: projection_dirty_addresses ?
SELECT state_kind,state_key,state_subkey FROM sync_projection_dirty;

-- name: projection_directory_lifecycles_and_parents ?
SELECT body FROM sync_state_version WHERE state_kind IN ('directory_parent','directory_lifecycle') AND body IS NOT NULL;

-- name: projection_dirty_books ?
SELECT DISTINCT COALESCE(v.book_key,CASE WHEN d.state_kind IN ('book_projection','book_add') THEN d.state_key END),d.state_kind
        FROM sync_projection_dirty d LEFT JOIN sync_state_version v USING(state_kind,state_key,state_subkey)
        WHERE v.book_key IS NOT NULL OR d.state_kind IN ('book_projection','book_add') ORDER BY 1;

-- name: projection_missing_directory_books ?
SELECT DISTINCT v.state_key FROM sync_state_version v LEFT JOIN dir d ON d.id=v.state_subkey WHERE v.state_kind='placement' AND v.body IS NOT NULL AND d.id IS NULL;

-- name: projection_book_state ?
-- param: book_key: &SqlParam<'_>
-- param: full: bool
-- param: changed_kinds: &str
SELECT body,changed_at FROM sync_state_version
WHERE book_key=:book_key AND body IS NOT NULL
  AND (:full OR state_kind='book_lifecycle' OR state_kind IN (SELECT value FROM json_each(:changed_kinds)))
UNION ALL SELECT body,0 FROM sync_book_record WHERE state_kind='book_add' AND content_hash=:book_key
  AND (:full OR 'book_add' IN (SELECT value FROM json_each(:changed_kinds)))
UNION ALL SELECT body,0 FROM local_book_draft WHERE content_hash=:book_key
  AND (:full OR 'book_add' IN (SELECT value FROM json_each(:changed_kinds)));

-- name: projection_directory_state ?
SELECT body FROM sync_state_version WHERE state_kind IN ('directory_name','directory_parent','directory_lifecycle') AND body IS NOT NULL ORDER BY state_kind,state_key;

-- name: projection_clear_dirty !
DELETE FROM sync_projection_dirty;

-- name: projection_directory_rows ?
SELECT id,parent_id,name,deleted_at IS NOT NULL FROM dir;

-- name: projection_directory_intents ?
SELECT n.body,p.body,l.body FROM sync_state_version n
    JOIN sync_state_version p ON p.state_kind='directory_parent' AND p.state_key=n.state_key AND p.state_subkey=''
    JOIN sync_state_version l ON l.state_kind='directory_lifecycle' AND l.state_key=n.state_key AND l.state_subkey=''
    WHERE n.state_kind='directory_name' AND n.state_subkey='' AND n.body IS NOT NULL AND p.body IS NOT NULL AND l.body IS NOT NULL;

-- name: projection_detach_directories !
UPDATE dir SET parent_id='00000000-0000-0000-0000-000000000000' WHERE id!='00000000-0000-0000-0000-000000000000' AND parent_id!='00000000-0000-0000-0000-000000000000';

-- name: projection_queue_purged_directory_files !
-- param: dir_id: &SqlParam<'_>
INSERT OR IGNORE INTO local_file_work(operation,content_hash,relative_path)
         SELECT 'remove',content_hash,relative_path FROM local_file_projection WHERE dir_id=:dir_id;

-- name: projection_remove_directory !
-- param: id: &SqlParam<'_>
DELETE FROM dir WHERE id=:id AND id!='00000000-0000-0000-0000-000000000000';

-- name: projection_directory_path_retained ->
-- param: dir_id: &SqlParam<'_>
-- param: arg_2: &SqlParam<'_>
SELECT EXISTS(SELECT 1 FROM local_directory_projection WHERE dir_id=:dir_id AND ltrim(relative_path,'/')=:arg_2);

-- name: projection_book_exists ->
-- param: content_hash: &SqlParam<'_>
SELECT EXISTS(SELECT 1 FROM book WHERE content_hash=:content_hash);

-- name: projection_book_deleted ->
-- param: content_hash: &SqlParam<'_>
SELECT deleted_at IS NOT NULL FROM book WHERE content_hash=:content_hash;

-- name: projection_set_book_facts !
-- param: content_hash: &BookId
-- param: format: Option<i64>
-- param: added_at: Option<i64>
UPDATE book SET format=:format,added_at=:added_at
WHERE content_hash=:content_hash AND (format,added_at) IS NOT (:format,:added_at);

-- name: projection_set_book_popularity !
-- param: content_hash: &BookId
-- param: rating_count: Option<f64>
UPDATE book SET popularity_rating_count=:rating_count
WHERE content_hash=:content_hash AND popularity_rating_count IS NOT :rating_count;

-- name: projection_set_book_lifecycle !
-- param: content_hash: &BookId
-- param: deleted_at: Option<i64>
-- param: origin: Option<&str>
UPDATE book SET deleted_at=:deleted_at,trash_origin_dir_id=:origin
WHERE content_hash=:content_hash AND (deleted_at,trash_origin_dir_id) IS NOT (:deleted_at,:origin);

-- name: projection_set_book_reading !
-- param: content_hash: &BookId
-- param: position: Option<&str>
-- param: progress: f32
-- param: read_at: Option<i64>
UPDATE book SET read_pos=:position,read_progress=:progress,read_at=:read_at
WHERE content_hash=:content_hash AND (read_pos,read_progress,read_at) IS NOT (:position,:progress,:read_at);

-- name: projection_cancel_deleted_cover_work !
-- param: content_hash: &SqlParam<'_>
UPDATE book_transfer SET cover_state=NULL WHERE content_hash=:content_hash AND cover_state='pending';

-- name: projection_restore_cover_work !
-- param: arg_1: &SqlParam<'_>
INSERT INTO book_transfer(content_hash,cover_state) VALUES(:arg_1,'pending') ON CONFLICT(content_hash) DO UPDATE SET cover_state='pending' WHERE cover_state IS NULL;

-- name: projection_clear_deleted_downloads !
-- param: content_hash: &SqlParam<'_>
UPDATE book_dir SET is_downloaded=0 WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND is_downloaded!=0;

-- name: projection_clear_title !
-- param: content_hash: &SqlParam<'_>
-- param: search_title: &SqlParam<'_>
UPDATE book SET title=NULL,subtitle=NULL,search_title=:search_title WHERE content_hash=:content_hash;

-- name: projection_clear_description !
-- param: content_hash: &SqlParam<'_>
DELETE FROM book_description WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash);

-- name: projection_clear_navigation !
-- param: content_hash: &SqlParam<'_>
UPDATE book SET entry_count=NULL, toc_json=NULL, duration_ms=NULL, tracks_json=NULL WHERE content_hash=:content_hash AND toc_json IS NOT NULL;

-- name: projection_trim_annotations !
-- param: content_hash: &SqlParam<'_>
DELETE FROM annotation WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND id NOT IN
       (SELECT state_key FROM sync_state_version WHERE state_kind='annotation' AND book_key=:content_hash AND body IS NOT NULL);

-- name: projection_trim_placements !
-- param: content_hash: &SqlParam<'_>
DELETE FROM book_dir WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND dir_id NOT IN
       (SELECT state_subkey FROM sync_state_version WHERE state_kind='placement' AND book_key=:content_hash AND body IS NOT NULL);

-- name: projection_other_placement_names ?
-- param: dir_id: &SqlParam<'_>
-- param: content_hash: &SqlParam<'_>
SELECT file_name FROM book_dir WHERE dir_id=:dir_id AND deleted_at IS NULL AND book_row_id!=(SELECT row_id FROM book WHERE content_hash=:content_hash);

-- name: projection_clear_recording !
-- param: content_hash: &SqlParam<'_>
UPDATE book_local_state SET recording_json=NULL WHERE content_hash=:content_hash AND recording_json IS NOT NULL;

-- name: projection_clear_pdf_metadata !
-- param: content_hash: &SqlParam<'_>
DELETE FROM pdf_reader_metadata WHERE content_hash=:content_hash;

-- name: projection_clear_languages !
-- param: book_row_id: &SqlParam<'_>
DELETE FROM book_language WHERE book_row_id=:book_row_id;

-- name: projection_upsert_placement !
-- param: arg_1: &SqlParam<'_>
-- param: content_hash: &SqlParam<'_>
-- param: arg_3: &SqlParam<'_>
INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash)
         VALUES(:arg_1,(SELECT row_id FROM book WHERE content_hash=:content_hash),:arg_3,'')
         ON CONFLICT(dir_id,book_row_id) DO UPDATE SET file_name=excluded.file_name;
