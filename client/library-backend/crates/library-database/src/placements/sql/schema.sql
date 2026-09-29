-- name: schema_placements &
-- Directory tree, the root directory, and book placement within it.
create table if not exists dir(
    id              TEXT primary key,
    parent_id       TEXT not null references dir(id) on delete cascade,
    name            TEXT not null,
    name_key        TEXT generated always as (portable_name_key(name)) stored,
    -- Authoritative synchronized register. The parent/name/deleted_at columns
    -- above are the derived, filesystem-safe projection and may temporarily
    -- suppress an otherwise-present intent to break cycles/path collisions.
    intent_parent_id TEXT,
    intent_name      TEXT,
    -- 0 = present, 1 = Trash, 2 = permanently purged tombstone.
    intent_lifecycle INTEGER,
    deleted_at      INTEGER,
    purged_at       INTEGER,
    CHECK (purged_at IS NULL OR deleted_at IS NOT NULL),
    CHECK (id != '00000000-0000-0000-0000-000000000000' OR (deleted_at IS NULL AND purged_at IS NULL))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_dir_live_path
ON dir(parent_id, name_key) WHERE deleted_at IS NULL AND id != '00000000-0000-0000-0000-000000000000';

-- Recursive folder traversal starts from parent_id but cannot use
-- idx_dir_live_path when SQLite cannot prove its extra root exclusion.
CREATE INDEX IF NOT EXISTS idx_dir_live_parent
ON dir(parent_id) WHERE deleted_at IS NULL;

insert into dir(id, parent_id, name, intent_parent_id, intent_name, intent_lifecycle, deleted_at, purged_at)
values ('00000000-0000-0000-0000-000000000000', '00000000-0000-0000-0000-000000000000', '', '00000000-0000-0000-0000-000000000000', '', 0, NULL, NULL)
on conflict(id) do update set parent_id = excluded.parent_id, name = excluded.name, deleted_at = NULL, purged_at = NULL;

create table if not exists book_dir(
    dir_id          TEXT not null references dir(id) on delete cascade,
    book_row_id         INTEGER not null references book(row_id) on delete cascade,
    file_name       TEXT not null,
    -- Per-machine filesystem fingerprint (nanosecond mtime/size/inode/dev). Cheap to
    -- compute on every scan so unchanged files can be skipped without re-reading
    -- their contents (see get_book_dir_by_hash). Not unique and not portable
    -- across devices, so it is an attribute, not the key.
    local_hash      TEXT not null,
    is_downloaded   INTEGER not null default 0,
    -- Monotonic scan generation in which this placement was last seen on disk.
    -- After a full scan, locally-present rows (is_downloaded = 1) not stamped
    -- with the current generation are reconciled as vanished. Remote placements
    -- never downloaded keep 0 and are excluded via the is_downloaded filter.
    last_scan       INTEGER not null default 0,
    deleted_at      INTEGER,
    trash_origin_dir_id TEXT,
    -- A row is a file *placement*: one book in one directory.
    -- Same-path collisions remain local filesystem invariants enforced by
    -- idx_book_dir_live_path.
    PRIMARY KEY     (dir_id, book_row_id)
);

-- local_hash lost its implicit PK index when it stopped being the key; the
-- scan-skip lookup queries by it on every file, so index it explicitly.
CREATE INDEX IF NOT EXISTS idx_book_dir_local_hash ON book_dir(local_hash);

-- The composite primary key starts with dir_id and cannot efficiently answer
-- the common "all placements for this book" lookup by itself.
CREATE INDEX IF NOT EXISTS idx_book_dir_book
ON book_dir(book_row_id, deleted_at, is_downloaded);

CREATE UNIQUE INDEX IF NOT EXISTS idx_book_dir_live_path
ON book_dir(dir_id, file_name) WHERE deleted_at IS NULL;


CREATE INDEX IF NOT EXISTS idx_book_dir_live_portable_name ON book_dir(dir_id,portable_name_key(file_name)) WHERE deleted_at IS NULL;
