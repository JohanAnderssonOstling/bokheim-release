-- name: schema_folder_cache &
-- One row in folder_book_ownership means a book has at least one live placement
-- in this folder's live subtree. Multiplicities preserve exact distinct-book and
-- downloaded counts when the same book is placed in several descendant folders.
CREATE TABLE IF NOT EXISTS folder_book_ownership(
    folder_id                    TEXT NOT NULL REFERENCES dir(id) ON DELETE CASCADE,
    book_row_id                  INTEGER NOT NULL REFERENCES book(row_id) ON DELETE CASCADE,
    placement_count              INTEGER NOT NULL CHECK(placement_count > 0),
    downloaded_placement_count   INTEGER NOT NULL CHECK(downloaded_placement_count >= 0 AND downloaded_placement_count <= placement_count),
    visible_placement_count      INTEGER NOT NULL CHECK(visible_placement_count >= 0 AND visible_placement_count <= placement_count),
    visible_downloaded_count     INTEGER NOT NULL CHECK(visible_downloaded_count >= 0 AND visible_downloaded_count <= visible_placement_count),
    source_directory             TEXT,
    PRIMARY KEY(folder_id, book_row_id)
);

-- Foreign-key cascades and book-scoped diagnostics need the reverse lookup.
CREATE INDEX IF NOT EXISTS idx_folder_book_ownership_book
ON folder_book_ownership(book_row_id, folder_id);

CREATE TABLE IF NOT EXISTS folder_browse_dirty_book(book_row_id INTEGER PRIMARY KEY);
CREATE TABLE IF NOT EXISTS browse_projection_state(
    projection TEXT PRIMARY KEY,
    dirty INTEGER NOT NULL CHECK(dirty IN (0,1))
);
INSERT OR IGNORE INTO browse_projection_state(projection,dirty) VALUES('folder_ownership',1);

-- Placement writes only invalidate; Rust rebuilds membership once per batch.
CREATE TRIGGER IF NOT EXISTS folder_dirty_placement_insert AFTER INSERT ON book_dir
WHEN NEW.deleted_at IS NULL
BEGIN
    INSERT INTO folder_browse_dirty_book(book_row_id)
    SELECT NEW.book_row_id WHERE NOT EXISTS(SELECT 1 FROM folder_browse_dirty_book WHERE book_row_id=NEW.book_row_id);
END;
CREATE TRIGGER IF NOT EXISTS folder_dirty_placement_delete AFTER DELETE ON book_dir
WHEN OLD.deleted_at IS NULL
BEGIN
    INSERT INTO folder_browse_dirty_book(book_row_id)
    SELECT OLD.book_row_id WHERE NOT EXISTS(SELECT 1 FROM folder_browse_dirty_book WHERE book_row_id=OLD.book_row_id);
END;
CREATE TRIGGER IF NOT EXISTS folder_dirty_placement_update AFTER UPDATE OF dir_id,book_row_id,deleted_at,is_downloaded ON book_dir
WHEN OLD.dir_id IS NOT NEW.dir_id OR OLD.book_row_id IS NOT NEW.book_row_id OR OLD.deleted_at IS NOT NEW.deleted_at OR OLD.is_downloaded IS NOT NEW.is_downloaded
BEGIN
    INSERT INTO folder_browse_dirty_book(book_row_id)
    SELECT OLD.book_row_id WHERE NOT EXISTS(SELECT 1 FROM folder_browse_dirty_book WHERE book_row_id=OLD.book_row_id);
    INSERT INTO folder_browse_dirty_book(book_row_id)
    SELECT NEW.book_row_id WHERE NOT EXISTS(SELECT 1 FROM folder_browse_dirty_book WHERE book_row_id=NEW.book_row_id);
END;
CREATE TRIGGER IF NOT EXISTS folder_dirty_directory_update AFTER UPDATE OF parent_id,name,deleted_at,purged_at ON dir
WHEN OLD.parent_id IS NOT NEW.parent_id OR (OLD.name LIKE '.%') IS NOT (NEW.name LIKE '.%') OR OLD.deleted_at IS NOT NEW.deleted_at OR OLD.purged_at IS NOT NEW.purged_at
BEGIN UPDATE browse_projection_state SET dirty=1 WHERE projection='folder_ownership'; END;
CREATE TRIGGER IF NOT EXISTS folder_dirty_directory_delete AFTER DELETE ON dir
BEGIN UPDATE browse_projection_state SET dirty=1 WHERE projection='folder_ownership'; END;

