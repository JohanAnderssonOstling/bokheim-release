-- name: schema_subjects &
-- Subject projection, placement triggers, incremental maintenance, download state.
-- Local derived state only: never synchronized and never stored in the taxonomy.
CREATE TABLE library_subject(route_id INTEGER PRIMARY KEY);
CREATE TABLE subject_browse_state(
    id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL, tree_revision INTEGER NOT NULL, built_revision INTEGER NOT NULL, taxonomy_revision TEXT NOT NULL DEFAULT ''
);
INSERT INTO subject_browse_state VALUES(1,0,0,-1,'');
CREATE TRIGGER subject_browse_book_insert AFTER INSERT ON book
BEGIN UPDATE subject_browse_state SET revision=revision+1, tree_revision=tree_revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_update AFTER UPDATE OF content_hash,title,format,hidden_at,read_pos,read_progress,added_at,deleted_at ON book
WHEN NEW.content_hash IS NOT OLD.content_hash OR NEW.title IS NOT OLD.title OR NEW.format IS NOT OLD.format OR NEW.hidden_at IS NOT OLD.hidden_at OR NEW.read_pos IS NOT OLD.read_pos OR NEW.read_progress IS NOT OLD.read_progress OR NEW.added_at IS NOT OLD.added_at OR NEW.deleted_at IS NOT OLD.deleted_at
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_delete AFTER DELETE ON book
BEGIN UPDATE subject_browse_state SET revision=revision+1, tree_revision=tree_revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_dir_update AFTER UPDATE OF book_row_id,file_name,deleted_at,is_downloaded ON book_dir
WHEN NEW.book_row_id IS NOT OLD.book_row_id OR NEW.file_name IS NOT OLD.file_name OR NEW.deleted_at IS NOT OLD.deleted_at OR NEW.is_downloaded IS NOT OLD.is_downloaded
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_unified_concept_insert AFTER INSERT ON book_unified_concept
BEGIN UPDATE subject_browse_state SET revision=revision+1, tree_revision=tree_revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_unified_concept_update AFTER UPDATE OF book_row_id,concept_id ON book_unified_concept
WHEN NEW.book_row_id IS NOT OLD.book_row_id OR NEW.concept_id IS NOT OLD.concept_id
BEGIN UPDATE subject_browse_state SET revision=revision+1, tree_revision=tree_revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_unified_concept_delete AFTER DELETE ON book_unified_concept
BEGIN UPDATE subject_browse_state SET revision=revision+1, tree_revision=tree_revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_language_insert AFTER INSERT ON book_language
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_language_update AFTER UPDATE ON book_language
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_language_delete AFTER DELETE ON book_language
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_contributor_insert AFTER INSERT ON book_contributor
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_contributor_update AFTER UPDATE ON book_contributor
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_contributor_delete AFTER DELETE ON book_contributor
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_author_identity_insert AFTER INSERT ON author_identity
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_author_identity_update AFTER UPDATE ON author_identity
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_author_identity_delete AFTER DELETE ON author_identity
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_audiobook_metadata_insert AFTER INSERT ON audiobook_metadata
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_audiobook_metadata_update AFTER UPDATE ON audiobook_metadata
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_audiobook_metadata_delete AFTER DELETE ON audiobook_metadata
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
-- The card's chapter count is a navigation entry count, so the browse
-- projection follows book_toc as well as the duration beside it.
CREATE TRIGGER subject_browse_book_toc_insert AFTER INSERT ON book_toc
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_toc_update AFTER UPDATE ON book_toc
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_toc_delete AFTER DELETE ON book_toc
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_sync_state_version_insert AFTER INSERT ON sync_state_version
WHEN NEW.state_kind='reading_position'
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_sync_state_version_update AFTER UPDATE ON sync_state_version
WHEN NEW.state_kind='reading_position' OR OLD.state_kind='reading_position'
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_sync_state_version_delete AFTER DELETE ON sync_state_version
WHEN OLD.state_kind='reading_position'
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_download_requests_insert AFTER INSERT ON download_requests
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_download_requests_update AFTER UPDATE ON download_requests
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_download_requests_delete AFTER DELETE ON download_requests
BEGIN UPDATE subject_browse_state SET revision=revision+1 WHERE id=1; END;
CREATE TRIGGER subject_browse_book_visibility AFTER UPDATE OF hidden_at,deleted_at ON book
WHEN NEW.hidden_at IS NOT OLD.hidden_at OR NEW.deleted_at IS NOT OLD.deleted_at
BEGIN UPDATE subject_browse_state SET tree_revision=tree_revision+1 WHERE id=1; END;

-- A subject view depends on having a live copy, not its folder.
CREATE TRIGGER subject_browse_book_dir_insert AFTER INSERT ON book_dir
BEGIN
    UPDATE subject_browse_state SET revision=revision+1,
        tree_revision=tree_revision + CASE WHEN NEW.deleted_at IS NULL AND (
            NOT EXISTS(SELECT 1 FROM book_dir p WHERE p.book_row_id=NEW.book_row_id AND p.dir_id!=NEW.dir_id AND p.deleted_at IS NULL)
        ) THEN 1 ELSE 0 END WHERE id=1;
END;
CREATE TRIGGER subject_browse_book_dir_delete AFTER DELETE ON book_dir
BEGIN
    UPDATE subject_browse_state SET revision=revision+1,
        tree_revision=tree_revision + CASE WHEN OLD.deleted_at IS NULL AND (
            NOT EXISTS(SELECT 1 FROM book_dir p WHERE p.book_row_id=OLD.book_row_id AND p.deleted_at IS NULL)
        ) THEN 1 ELSE 0 END WHERE id=1;
END;
CREATE TRIGGER subject_browse_placement_membership AFTER UPDATE OF book_row_id,deleted_at ON book_dir
WHEN NEW.book_row_id IS NOT OLD.book_row_id
    OR ((NEW.deleted_at IS NULL) != (OLD.deleted_at IS NULL)
        AND NOT EXISTS(SELECT 1 FROM book_dir p WHERE p.book_row_id=NEW.book_row_id AND p.dir_id!=NEW.dir_id AND p.deleted_at IS NULL))
BEGIN UPDATE subject_browse_state SET tree_revision=tree_revision+1 WHERE id=1; END;

-- Dirty book IDs survive deletion so their old subject paths can be removed.
CREATE TABLE IF NOT EXISTS subject_browse_dirty_book(book_row_id INTEGER PRIMARY KEY);
CREATE TABLE IF NOT EXISTS subject_browse_member(
    route_id INTEGER NOT NULL, book_row_id INTEGER NOT NULL, direct INTEGER NOT NULL,
    PRIMARY KEY(route_id,book_row_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS subject_browse_member_book ON subject_browse_member(book_row_id,route_id);
CREATE TABLE IF NOT EXISTS subject_browse_visible_route(route_id INTEGER PRIMARY KEY,parent_route_id INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS subject_browse_placement(
    route_id INTEGER NOT NULL, book_row_id INTEGER NOT NULL, source_branch_id INTEGER NOT NULL,
    PRIMARY KEY(route_id,book_row_id,source_branch_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS subject_browse_placement_branch ON subject_browse_placement(source_branch_id,route_id,book_row_id);
CREATE TRIGGER IF NOT EXISTS subject_dirty_book_insert AFTER INSERT ON book

BEGIN
    INSERT INTO subject_browse_dirty_book SELECT NEW.row_id WHERE NOT EXISTS(SELECT 1 FROM subject_browse_dirty_book WHERE book_row_id=NEW.row_id);
END;
CREATE TRIGGER IF NOT EXISTS subject_dirty_book_delete AFTER DELETE ON book

BEGIN
    INSERT INTO subject_browse_dirty_book SELECT OLD.row_id WHERE NOT EXISTS(SELECT 1 FROM subject_browse_dirty_book WHERE book_row_id=OLD.row_id);
END;
CREATE TRIGGER IF NOT EXISTS subject_dirty_book_visibility AFTER UPDATE OF hidden_at,deleted_at ON book
WHEN NEW.hidden_at IS NOT OLD.hidden_at OR NEW.deleted_at IS NOT OLD.deleted_at
BEGIN
    INSERT INTO subject_browse_dirty_book SELECT NEW.row_id WHERE NOT EXISTS(SELECT 1 FROM subject_browse_dirty_book WHERE book_row_id=NEW.row_id);
END;
CREATE TRIGGER IF NOT EXISTS subject_dirty_assignment_insert AFTER INSERT ON book_unified_concept

BEGIN
    INSERT INTO subject_browse_dirty_book SELECT NEW.book_row_id WHERE NOT EXISTS(SELECT 1 FROM subject_browse_dirty_book WHERE book_row_id=NEW.book_row_id);
END;
CREATE TRIGGER IF NOT EXISTS subject_dirty_assignment_delete AFTER DELETE ON book_unified_concept

BEGIN
    INSERT INTO subject_browse_dirty_book SELECT OLD.book_row_id WHERE NOT EXISTS(SELECT 1 FROM subject_browse_dirty_book WHERE book_row_id=OLD.book_row_id);
END;
CREATE TRIGGER IF NOT EXISTS subject_dirty_assignment_update AFTER UPDATE OF book_row_id,concept_id ON book_unified_concept
WHEN NEW.book_row_id IS NOT OLD.book_row_id OR NEW.concept_id IS NOT OLD.concept_id
BEGIN
    INSERT INTO subject_browse_dirty_book SELECT OLD.book_row_id WHERE NOT EXISTS(SELECT 1 FROM subject_browse_dirty_book WHERE book_row_id=OLD.book_row_id);
    INSERT INTO subject_browse_dirty_book SELECT NEW.book_row_id WHERE NOT EXISTS(SELECT 1 FROM subject_browse_dirty_book WHERE book_row_id=NEW.book_row_id);
END;
CREATE TRIGGER IF NOT EXISTS subject_dirty_placement_insert AFTER INSERT ON book_dir
WHEN NEW.deleted_at IS NULL
BEGIN
    INSERT INTO subject_browse_dirty_book SELECT NEW.book_row_id WHERE NOT EXISTS(SELECT 1 FROM subject_browse_dirty_book WHERE book_row_id=NEW.book_row_id);
END;
CREATE TRIGGER IF NOT EXISTS subject_dirty_placement_delete AFTER DELETE ON book_dir
WHEN OLD.deleted_at IS NULL
BEGIN
    INSERT INTO subject_browse_dirty_book SELECT OLD.book_row_id WHERE NOT EXISTS(SELECT 1 FROM subject_browse_dirty_book WHERE book_row_id=OLD.book_row_id);
END;
CREATE TRIGGER IF NOT EXISTS subject_dirty_placement_update AFTER UPDATE OF book_row_id,deleted_at ON book_dir
WHEN NEW.book_row_id IS NOT OLD.book_row_id OR NEW.deleted_at IS NOT OLD.deleted_at
BEGIN
    INSERT INTO subject_browse_dirty_book SELECT OLD.book_row_id WHERE NOT EXISTS(SELECT 1 FROM subject_browse_dirty_book WHERE book_row_id=OLD.book_row_id);
    INSERT INTO subject_browse_dirty_book SELECT NEW.book_row_id WHERE NOT EXISTS(SELECT 1 FROM subject_browse_dirty_book WHERE book_row_id=NEW.book_row_id);
END;
UPDATE subject_browse_state SET built_revision=-1 WHERE id=1;
