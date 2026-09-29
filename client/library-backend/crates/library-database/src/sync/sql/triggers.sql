-- name: schema_sync_triggers &

-- Every producer passes typed SQL values through the registered Rust encoder.
-- Invalid protocol values abort the surrounding transaction; valid values are
-- stored as versioned bincode without an intermediate JSON representation.
CREATE TRIGGER trig_dir_state_insert
AFTER INSERT ON dir
WHEN NEW.id != '00000000-0000-0000-0000-000000000000'
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('directory_name', NEW.id, COALESCE(NEW.intent_name,NEW.name)), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
    SELECT bokheim_accept_local(bokheim_mutation('directory_parent', NEW.id, COALESCE(NEW.intent_parent_id,NEW.parent_id)), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
    SELECT bokheim_accept_local(bokheim_mutation('directory_lifecycle', NEW.id, COALESCE(NEW.intent_lifecycle,0)), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER trig_dir_name_update
AFTER UPDATE OF intent_name ON dir
WHEN NEW.id != '00000000-0000-0000-0000-000000000000'
  AND OLD.intent_name IS NOT NEW.intent_name
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('directory_name', NEW.id, NEW.intent_name), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER trig_dir_parent_update
AFTER UPDATE OF intent_parent_id ON dir
WHEN NEW.id != '00000000-0000-0000-0000-000000000000' AND OLD.intent_parent_id IS NOT NEW.intent_parent_id
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('directory_parent', NEW.id, NEW.intent_parent_id), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER trig_dir_lifecycle_update
AFTER UPDATE OF intent_lifecycle ON dir
WHEN NEW.id != '00000000-0000-0000-0000-000000000000'
  AND OLD.intent_lifecycle IS NOT NEW.intent_lifecycle
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('directory_lifecycle', NEW.id, NEW.intent_lifecycle), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER trig_book_added
AFTER INSERT ON book
WHEN NEW.deleted_at IS NULL
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('book_lifecycle', NEW.content_hash, 0, COALESCE(NEW.added_at, 0), NEW.format, NULL), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

-- Initial metadata has its own register, including books with no contributors.
CREATE TRIGGER trig_book_metadata_insert
AFTER INSERT ON book
WHEN NEW.deleted_at IS NULL AND (NEW.title IS NOT NULL OR NEW.subtitle IS NOT NULL)
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('metadata', NEW.content_hash, COALESCE(NEW.title,''), NEW.subtitle, contributors_for_book(NEW.row_id), NEW.book_metadata), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER trig_book_added_reactivate
AFTER UPDATE OF deleted_at ON book
WHEN NEW.deleted_at IS NULL AND OLD.deleted_at IS NOT NULL
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('book_lifecycle', NEW.content_hash, 0, COALESCE(NEW.added_at, 0), NEW.format, NULL), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER trig_book_deleted
AFTER UPDATE OF deleted_at ON book
WHEN NEW.deleted_at IS NOT NULL AND OLD.deleted_at IS NULL
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('book_lifecycle', NEW.content_hash, 1, 0, '', NEW.trash_origin_dir_id), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER trig_book_dir_added_insert
AFTER INSERT ON book_dir
WHEN NEW.deleted_at IS NULL
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('placement', NEW.dir_id, (SELECT content_hash FROM book WHERE row_id=NEW.book_row_id), 1, NULL), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER trig_book_dir_added_reactivate
AFTER UPDATE OF deleted_at, dir_id, book_row_id ON book_dir
WHEN NEW.deleted_at IS NULL
  AND (OLD.deleted_at IS NOT NULL OR OLD.dir_id IS NOT NEW.dir_id OR OLD.book_row_id IS NOT NEW.book_row_id)
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('placement', NEW.dir_id, (SELECT content_hash FROM book WHERE row_id=NEW.book_row_id), 1, NULL), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER trig_book_dir_deleted
AFTER UPDATE OF deleted_at ON book_dir
WHEN NEW.deleted_at IS NOT NULL AND OLD.deleted_at IS NULL
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('placement', OLD.dir_id, (SELECT content_hash FROM book WHERE row_id=OLD.book_row_id), 0, NEW.trash_origin_dir_id), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER trig_book_read_pos
AFTER UPDATE OF read_pos, read_progress ON book
WHEN NEW.read_pos IS NOT NULL
  AND (COALESCE(OLD.read_pos, '') != COALESCE(NEW.read_pos, '') OR COALESCE(OLD.read_progress, 0) != COALESCE(NEW.read_progress, 0))
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('reading_position', NEW.content_hash, NEW.read_pos, COALESCE(NEW.read_progress, 0)), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER trig_annotation_state_insert
AFTER INSERT ON annotation
WHEN COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('annotation', NEW.id, (SELECT content_hash FROM book WHERE row_id=NEW.book_row_id), NEW.detail, NEW.modified_at, NEW.deleted_at IS NOT NULL), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

CREATE TRIGGER trig_annotation_state_update
AFTER UPDATE OF detail, modified_at, deleted_at ON annotation
WHEN COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('annotation', NEW.id, (SELECT content_hash FROM book WHERE row_id=NEW.book_row_id), NEW.detail, NEW.modified_at, NEW.deleted_at IS NOT NULL), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;

-- Navigation travels because a replica cannot always rebuild it: an audiobook's
-- chapters may have come from enrichment rather than from the file, and a
-- replica that has not downloaded a book has no file to rebuild from at all.
CREATE TRIGGER IF NOT EXISTS book_toc_insert AFTER INSERT ON book_toc
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('book_toc',NEW.content_hash,NEW.toc_json,COALESCE((SELECT duration_ms FROM audiobook_metadata WHERE content_hash=NEW.content_hash),-1),(SELECT tracks_json FROM audiobook_track_index WHERE content_hash=NEW.content_hash)), CAST((julianday('now')-2440587.5)*86400000 AS INTEGER)) WHERE COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1),'local')='local';
END;
CREATE TRIGGER IF NOT EXISTS book_toc_update AFTER UPDATE ON book_toc
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('book_toc',NEW.content_hash,NEW.toc_json,COALESCE((SELECT duration_ms FROM audiobook_metadata WHERE content_hash=NEW.content_hash),-1),(SELECT tracks_json FROM audiobook_track_index WHERE content_hash=NEW.content_hash)), CAST((julianday('now')-2440587.5)*86400000 AS INTEGER)) WHERE COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1),'local')='local';
END;

CREATE TRIGGER IF NOT EXISTS audiobook_track_index_insert AFTER INSERT ON audiobook_track_index
BEGIN
    UPDATE book_toc SET toc_json=toc_json WHERE content_hash=NEW.content_hash
      AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton=1),'local')='local';
END;
CREATE TRIGGER IF NOT EXISTS audiobook_track_index_update AFTER UPDATE ON audiobook_track_index
BEGIN
    UPDATE book_toc SET toc_json=toc_json WHERE content_hash=NEW.content_hash
      AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton=1),'local')='local';
END;

CREATE TRIGGER trig_book_facts_insert AFTER INSERT ON book
WHEN NEW.format!='' AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton=1),'local')='local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('book_facts',NEW.content_hash,COALESCE(NEW.added_at,0),NEW.format),CAST((julianday('now')-2440587.5)*86400000 AS INTEGER));
END;
CREATE TRIGGER trig_book_facts_update AFTER UPDATE OF format,added_at ON book
WHEN NEW.format!='' AND (NEW.format IS NOT OLD.format OR NEW.added_at IS NOT OLD.added_at)
 AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton=1),'local')='local'
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('book_facts',NEW.content_hash,COALESCE(NEW.added_at,0),NEW.format),CAST((julianday('now')-2440587.5)*86400000 AS INTEGER));
END;
