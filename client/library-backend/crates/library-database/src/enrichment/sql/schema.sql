-- name: schema_external_metadata &
-- External metadata attempts.
create table if not exists external_metadata_attempt(
    book_row_id     INTEGER not null references book(row_id) on delete cascade,
    provider_id     TEXT not null,
    identifier      TEXT not null,
    status          TEXT not null check(status in ('updated', 'no_match', 'ambiguous', 'failed')),
    detail          TEXT,
    attempted_at    INTEGER not null,
    primary key(book_row_id, provider_id)
);
create index if not exists idx_external_metadata_attempt_status on external_metadata_attempt(provider_id, status, book_row_id);
create table if not exists book_value(
    book_row_id         INTEGER not null references book(row_id) on delete cascade,
    kind            TEXT not null,
    position        INTEGER not null check(position >= 0),
    value           TEXT not null,
    qualifier       TEXT,
    secondary_value TEXT,
    primary key(book_row_id, kind, position)
);
create index if not exists idx_book_value_lookup on book_value(kind, value, book_row_id);

-- name: schema_metadata_batch &
-- Metadata coalescing triggers.
-- Version 4: description-independent metadata; connection-local batching mode; explicit upgrade definitions.
CREATE TABLE IF NOT EXISTS pending_metadata_payload(book_row_id INTEGER PRIMARY KEY REFERENCES book(row_id) ON DELETE CASCADE);
CREATE TRIGGER trig_book_metadata
AFTER UPDATE OF title, subtitle, book_metadata ON book
WHEN (OLD.title IS NOT NEW.title OR OLD.subtitle IS NOT NEW.subtitle OR OLD.book_metadata IS NOT NEW.book_metadata)
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
 AND metadata_batch_flag(NULL)=0
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('metadata', NEW.content_hash, COALESCE(NEW.title, ''), NEW.subtitle, contributors_for_book(NEW.row_id), NEW.book_metadata), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;
CREATE TRIGGER trig_book_metadata_queued
AFTER UPDATE OF title, subtitle, book_metadata ON book
WHEN (OLD.title IS NOT NEW.title OR OLD.subtitle IS NOT NEW.subtitle OR OLD.book_metadata IS NOT NEW.book_metadata)
  AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
 AND metadata_batch_flag(NULL)=1
BEGIN
 INSERT INTO pending_metadata_payload SELECT row_id FROM book WHERE row_id=NEW.row_id ON CONFLICT(book_row_id) DO NOTHING;
END;
CREATE TRIGGER trig_book_contributor_insert
AFTER INSERT ON book_contributor
WHEN COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
 AND metadata_batch_flag(NULL)=0
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('metadata', book.content_hash, COALESCE(book.title, ''), book.subtitle, contributors_for_book(book.row_id), book.book_metadata), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)) FROM book WHERE book.row_id = NEW.book_row_id;
END;
CREATE TRIGGER trig_book_contributor_insert_queued
AFTER INSERT ON book_contributor
WHEN COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
 AND metadata_batch_flag(NULL)=1
BEGIN
 INSERT INTO pending_metadata_payload SELECT row_id FROM book WHERE row_id=NEW.book_row_id ON CONFLICT(book_row_id) DO NOTHING;
END;
CREATE TRIGGER trig_book_contributor_update
AFTER UPDATE ON book_contributor
WHEN COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
 AND metadata_batch_flag(NULL)=0
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('metadata', book.content_hash, COALESCE(book.title, ''), book.subtitle, contributors_for_book(book.row_id), book.book_metadata), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)) FROM book WHERE book.row_id = NEW.book_row_id;
END;
CREATE TRIGGER trig_book_contributor_update_queued
AFTER UPDATE ON book_contributor
WHEN COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
 AND metadata_batch_flag(NULL)=1
BEGIN
 INSERT INTO pending_metadata_payload SELECT row_id FROM book WHERE row_id=NEW.book_row_id ON CONFLICT(book_row_id) DO NOTHING;
END;
CREATE TRIGGER trig_book_contributor_delete
AFTER DELETE ON book_contributor
WHEN COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
  AND EXISTS (SELECT 1 FROM book WHERE book.row_id = OLD.book_row_id)
 AND metadata_batch_flag(NULL)=0
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('metadata', book.content_hash, COALESCE(book.title, ''), book.subtitle, contributors_for_book(book.row_id), book.book_metadata), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)) FROM book WHERE book.row_id = OLD.book_row_id;
END;
CREATE TRIGGER trig_book_contributor_delete_queued
AFTER DELETE ON book_contributor
WHEN COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1), 'local') = 'local'
  AND EXISTS (SELECT 1 FROM book WHERE book.row_id = OLD.book_row_id)
 AND metadata_batch_flag(NULL)=1
BEGIN
 INSERT INTO pending_metadata_payload SELECT row_id FROM book WHERE row_id=OLD.book_row_id ON CONFLICT(book_row_id) DO NOTHING;
END;
