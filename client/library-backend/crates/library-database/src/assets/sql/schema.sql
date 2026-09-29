-- name: schema_assets &
-- Remote asset presence, download requests, transfer work, upload intent.
create table if not exists remote_asset(
    kind    TEXT not null check(kind in ('book', 'thumbnail')),
    hash    TEXT not null,
    primary key (kind, hash)
);

CREATE TABLE IF NOT EXISTS remote_thumbnail_revision(
    content_hash TEXT PRIMARY KEY NOT NULL REFERENCES book(content_hash) ON DELETE CASCADE,
    revision TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS local_thumbnail_sync_pending(
    content_hash TEXT PRIMARY KEY NOT NULL REFERENCES book(content_hash) ON DELETE CASCADE,
    action TEXT NOT NULL CHECK(action IN ('put', 'delete')),
    generation INTEGER NOT NULL DEFAULT 1
) STRICT;

-- Durable download intent for content-addressed assets.
create table if not exists download_requests(
    target_kind     TEXT not null check(target_kind in ('book', 'thumbnail')),
    target_key      TEXT not null,
    origin          TEXT not null default 'background' check(origin in ('background', 'user_initiated')),
    created_at      INTEGER not null,
    primary key (target_kind, target_key)
);

CREATE TABLE asset_work(id INTEGER PRIMARY KEY AUTOINCREMENT, content_hash TEXT NOT NULL UNIQUE);

CREATE TABLE IF NOT EXISTS local_book_versions(
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        content_hash TEXT NOT NULL REFERENCES book(content_hash) ON DELETE CASCADE,
        checksum TEXT NOT NULL,
        size_bytes INTEGER NOT NULL CHECK(size_bytes>0),
        origin TEXT NOT NULL CHECK(origin IN ('local','remote')),
        UNIQUE(content_hash,checksum)
    );
    CREATE TABLE IF NOT EXISTS local_book_upload(
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        content_hash TEXT NOT NULL UNIQUE REFERENCES book(content_hash) ON DELETE CASCADE,
        checksum TEXT NOT NULL,
        size_bytes INTEGER NOT NULL CHECK(size_bytes>0)
    );
    CREATE TABLE IF NOT EXISTS local_book_current(
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        dir_id TEXT NOT NULL,
        content_hash TEXT NOT NULL REFERENCES book(content_hash) ON DELETE CASCADE,
        checksum TEXT NOT NULL,
        origin TEXT NOT NULL CHECK(origin IN ('local','remote')),
        UNIQUE(dir_id,content_hash)
    );

CREATE TABLE IF NOT EXISTS rejected_asset_upload(
            kind TEXT NOT NULL CHECK(kind IN ('book','thumbnail')),
            hash TEXT NOT NULL,
            reason TEXT NOT NULL,
            rejected_at INTEGER NOT NULL,
            PRIMARY KEY(kind, hash)
        );

-- name: schema_asset_triggers &
-- Triggers maintaining durable transfer work.
-- Durable asset candidates, independent of metadata exchange scheduling.
CREATE TRIGGER asset_work_book_insert AFTER INSERT ON book
BEGIN
    INSERT INTO asset_work(content_hash) SELECT NEW.content_hash WHERE NEW.content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_book_delete AFTER DELETE ON book
BEGIN
    INSERT INTO asset_work(content_hash) SELECT OLD.content_hash WHERE OLD.content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_book_dir_insert AFTER INSERT ON book_dir
BEGIN
    INSERT INTO asset_work(content_hash) SELECT content_hash FROM book WHERE row_id=NEW.book_row_id AND content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_book_dir_update AFTER UPDATE ON book_dir
BEGIN
    INSERT INTO asset_work(content_hash) SELECT content_hash FROM book WHERE row_id=NEW.book_row_id AND content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_book_dir_delete AFTER DELETE ON book_dir
BEGIN
    INSERT INTO asset_work(content_hash) SELECT content_hash FROM book WHERE row_id=OLD.book_row_id AND content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_remote_asset_insert AFTER INSERT ON remote_asset
BEGIN
    INSERT INTO asset_work(content_hash) SELECT NEW.hash WHERE NEW.hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_remote_asset_update AFTER UPDATE ON remote_asset
BEGIN
    INSERT INTO asset_work(content_hash) SELECT NEW.hash WHERE NEW.hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_remote_asset_delete AFTER DELETE ON remote_asset
BEGIN
    INSERT INTO asset_work(content_hash) SELECT OLD.hash WHERE OLD.hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_local_thumbnail_work_insert AFTER INSERT ON local_thumbnail_work
BEGIN
    INSERT INTO asset_work(content_hash) SELECT NEW.content_hash WHERE NEW.content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_local_thumbnail_work_update AFTER UPDATE ON local_thumbnail_work
BEGIN
    INSERT INTO asset_work(content_hash) SELECT NEW.content_hash WHERE NEW.content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_local_thumbnail_work_delete AFTER DELETE ON local_thumbnail_work
BEGIN
    INSERT INTO asset_work(content_hash) SELECT OLD.content_hash WHERE OLD.content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_download_requests_insert AFTER INSERT ON download_requests
BEGIN
    INSERT INTO asset_work(content_hash) SELECT NEW.target_key WHERE NEW.target_kind='book'
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_download_requests_update AFTER UPDATE ON download_requests
BEGIN
    INSERT INTO asset_work(content_hash) SELECT NEW.target_key WHERE NEW.target_kind='book'
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_download_requests_delete AFTER DELETE ON download_requests
BEGIN
    INSERT INTO asset_work(content_hash) SELECT OLD.target_key WHERE OLD.target_kind='book'
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_rejected_asset_upload_insert AFTER INSERT ON rejected_asset_upload
BEGIN
    INSERT INTO asset_work(content_hash) SELECT NEW.hash WHERE NEW.hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_rejected_asset_upload_update AFTER UPDATE ON rejected_asset_upload
BEGIN
    INSERT INTO asset_work(content_hash) SELECT NEW.hash WHERE NEW.hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_rejected_asset_upload_delete AFTER DELETE ON rejected_asset_upload
BEGIN
    INSERT INTO asset_work(content_hash) SELECT OLD.hash WHERE OLD.hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_directory_update AFTER UPDATE OF parent_id,name,deleted_at ON dir
BEGIN
    INSERT INTO asset_work(content_hash) WITH RECURSIVE affected(id) AS (
        SELECT NEW.id UNION ALL SELECT d.id FROM dir d JOIN affected a ON d.parent_id=a.id WHERE d.id!=d.parent_id
    ) SELECT DISTINCT b.content_hash FROM affected a JOIN book_dir bd ON bd.dir_id=a.id JOIN book b ON b.row_id=bd.book_row_id WHERE b.content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
CREATE TRIGGER asset_work_directory_delete AFTER DELETE ON dir
BEGIN
    INSERT INTO asset_work(content_hash) WITH RECURSIVE affected(id) AS (
        SELECT OLD.id UNION ALL SELECT d.id FROM dir d JOIN affected a ON d.parent_id=a.id WHERE d.id!=d.parent_id
    ) SELECT DISTINCT b.content_hash FROM affected a JOIN book_dir bd ON bd.dir_id=a.id JOIN book b ON b.row_id=bd.book_row_id WHERE b.content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;

CREATE TRIGGER asset_work_directory_insert AFTER INSERT ON dir
BEGIN
    INSERT INTO asset_work(content_hash) WITH RECURSIVE affected(id) AS (
        SELECT NEW.id UNION ALL SELECT d.id FROM dir d JOIN affected a ON d.parent_id=a.id WHERE d.id!=d.parent_id
    ) SELECT DISTINCT b.content_hash FROM affected a JOIN book_dir bd ON bd.dir_id=a.id JOIN book b ON b.row_id=bd.book_row_id WHERE b.content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;

CREATE TRIGGER asset_work_old_placement AFTER UPDATE OF book_row_id ON book_dir
WHEN OLD.book_row_id!=NEW.book_row_id
BEGIN
    INSERT INTO asset_work(content_hash) SELECT content_hash FROM book WHERE row_id=OLD.book_row_id AND content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;

CREATE TRIGGER asset_work_book_update AFTER UPDATE OF content_hash,format,deleted_at,hidden_at ON book
WHEN OLD.content_hash IS NOT NEW.content_hash OR OLD.format IS NOT NEW.format
  OR OLD.deleted_at IS NOT NEW.deleted_at
  OR OLD.hidden_at IS NOT NEW.hidden_at
BEGIN
    INSERT INTO asset_work(content_hash) SELECT NEW.content_hash WHERE NEW.content_hash!=''
    ON CONFLICT(content_hash) DO UPDATE SET id=excluded.id;
END;
