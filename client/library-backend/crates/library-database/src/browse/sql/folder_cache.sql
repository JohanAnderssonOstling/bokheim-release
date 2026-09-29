-- name: folder_cache_status?
SELECT dirty, EXISTS(SELECT 1 FROM folder_browse_dirty_book)
FROM browse_projection_state WHERE projection='folder_ownership';

-- name: folder_cache_rebuild &
DELETE FROM folder_book_ownership;
INSERT OR IGNORE INTO folder_browse_dirty_book SELECT DISTINCT book_row_id FROM book_dir;

-- name: folder_cache_remove_dirty &
DELETE FROM folder_book_ownership WHERE book_row_id IN (SELECT book_row_id FROM folder_browse_dirty_book);

-- name: folder_cache_directories?
SELECT id,parent_id,name FROM dir WHERE deleted_at IS NULL;

-- name: folder_cache_directory?
-- param: id: &str
SELECT parent_id,name FROM dir WHERE id=:id AND deleted_at IS NULL;

-- name: folder_cache_placements?
-- Keep the dirty set outermost: SQLite may otherwise scan all live placements.
SELECT p.book_row_id,p.dir_id,p.is_downloaded
FROM folder_browse_dirty_book dirty CROSS JOIN book_dir p ON p.book_row_id=dirty.book_row_id
WHERE p.deleted_at IS NULL;

-- name: folder_cache_insert!
-- param: folder: &str
-- param: book: i64
-- param: placements: i64
-- param: downloads: i64
-- param: visible: i64
-- param: visible_downloads: i64
-- param: source: &str
INSERT INTO folder_book_ownership VALUES(:folder,:book,:placements,:downloads,:visible,:visible_downloads,:source);

-- name: folder_cache_clean &
DELETE FROM folder_browse_dirty_book;
UPDATE browse_projection_state SET dirty=0 WHERE projection='folder_ownership';
