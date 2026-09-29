-- A scan re-committing an already-known placement (a resumed or repeated
-- pass) hits this same (dir_id, book_row_id) primary key again; explicit
-- import never does, so the conflict branch is simply unreached there.
INSERT INTO book_dir (dir_id, book_row_id, file_name, local_hash, last_scan, is_downloaded, deleted_at)
VALUES (:dir_id,(SELECT row_id FROM book WHERE content_hash=:content_hash),:file_name,:local_hash,:last_scan,:is_downloaded,NULL)
ON CONFLICT(dir_id,book_row_id) DO UPDATE SET file_name=excluded.file_name,local_hash=excluded.local_hash,last_scan=excluded.last_scan,is_downloaded=excluded.is_downloaded,deleted_at=NULL;
