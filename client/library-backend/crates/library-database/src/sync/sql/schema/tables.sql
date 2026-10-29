-- name: schema_sync_tables &
-- Book rows, book metadata projection, and subject taxonomy.
create table if not exists book(
    row_id          INTEGER primary key,
    -- Portable identity used by sync and asset APIs. Local relationships use
    -- row_id so their indexes remain compact and integer joins stay cheap.
    content_hash    BLOB not null unique,
    title           TEXT,
    subtitle        TEXT,
    -- Browse text, normalized in Rust whenever the title or credits are
    -- written (browse/text.rs, contributors::write_credits).
    author          TEXT not null default '',
    search_title    TEXT not null default '',
    search_author   TEXT not null default '',
    -- Canonical synchronized bibliographic metadata. Dedicated tables below
    -- are disposable query projections rebuilt from this value.
    -- book_model::BookFormat code. Fixed for the life of the row: identity is
    -- derived from the bytes, so the format cannot change without becoming a
    -- different book. NULL only on a row that has never been observed on any
    -- device's disk.
    format          INTEGER,
    read_pos        TEXT                DEFAULT NULL,
    read_progress   REAL                DEFAULT 0,
    -- When the reading position last changed, on any device' || ':' || ' the time of its
    -- synchronized register. NULL for a book never opened.
    read_at         INTEGER,
    added_at        INTEGER,
    -- Browse projection of the immutable enrichment's rating count. REAL
    -- preserves the existing f64 ordering across the full u64 count range.
    -- NULL means unknown; zero is a known count.
    popularity_rating_count REAL CHECK(popularity_rating_count >= 0),
    deleted_at      INTEGER,
    trash_origin_dir_id TEXT,
    -- Navigation belongs to this immutable file identity and is read as a
    -- whole tree. NULL means no navigation document; [] records inspected
    -- files with no entries. Playback facts belong to the same document.
    entry_count     INTEGER check(entry_count >= 0),
    -- Historical column names/affinity; new projections store binary BLOBs.
    -- SQLite accepts both BLOBs and the JSON TEXT retained by older caches.
    toc_json        TEXT,
    duration_ms     INTEGER check(duration_ms >= 0),
    tracks_json     TEXT,
    check((toc_json IS NULL) = (entry_count IS NULL)),
    check(toc_json IS NOT NULL OR (duration_ms IS NULL AND tracks_json IS NULL))
);

-- Base languages (en-US and en-GB are both en), normalized in Rust.
create table if not exists book_language(
    book_row_id     INTEGER not null references book(row_id) on delete cascade,
    language        TEXT not null,
    primary key(book_row_id, language)
);

-- Merged relational contributor register. Every role (including "aut" =
-- author) shares one flat per-book position sequence. This replaces the
-- former separate book_author table. Queries that only want authors bind
-- book_model' || ':' || ':' || 'AUTHOR_MARC_RELATOR_CODE as a parameter rather than spelling
-- the role out in SQL.
create table if not exists book_contributor(
    book_row_id         INTEGER not null references book(row_id) on delete cascade,
    position        INTEGER not null check(position >= 0),
    author_identity_id INTEGER not null references author_identity(id) on delete cascade,
    name            TEXT not null,
    sort_name       TEXT,
    role            BLOB not null check(typeof(role) = 'blob' and length(role) = 3),
    primary key(book_row_id, position)
);

create table if not exists book_subject(
    book_row_id         INTEGER not null references book(row_id) on delete cascade,
    position        INTEGER not null check(position >= 0),
    name            TEXT not null,
    source          TEXT not null,
    authority       TEXT,
    code            TEXT,
    primary key(book_row_id, position)
);

-- Curated subjects and navigation live in the attached read-only taxonomy.
-- Library storage contains book assignments and their evidence only.
create table if not exists book_unified_concept(
    book_row_id     INTEGER not null references book(row_id) on delete cascade,
    concept_id INTEGER not null check(concept_id > 0),
    mapper_version  INTEGER not null check(mapper_version >= 0),
    primary key(book_row_id,concept_id)
);

create table if not exists book_unified_concept_evidence(
    book_row_id      INTEGER not null,
    concept_id INTEGER not null,
    subject_position INTEGER not null,
    source_system_id TEXT not null,
    source_code      TEXT not null,
    primary key(book_row_id,concept_id,subject_position,source_system_id,source_code),
    foreign key(book_row_id,concept_id) references book_unified_concept(book_row_id,concept_id) on delete cascade
);

-- Local bookkeeping for explicit external enrichment. Provider vocabularies
-- are never cached here; this records only whether one book was attempted.

-- Author identities and their credited names.
create table if not exists author_identity(
    id              INTEGER primary key autoincrement,
    stable_id       TEXT not null unique,
    -- Import supplies the name equivalence rule for unresolved candidates.
    -- Stable IDs identify people; names alone do not.
    name TEXT not null,
    is_provisional  INTEGER not null default 1 check(is_provisional in (0, 1))
);

-- Canonical description storage, kept off the frequently read book row.
CREATE TABLE IF NOT EXISTS book_description(
    book_row_id INTEGER PRIMARY KEY REFERENCES book(row_id) ON DELETE CASCADE,
    description TEXT NOT NULL
);

-- Durable synchronization outbox, winning versions, and replica metadata.
-- A pending row identifies a register. Payload columns are NULL until a
-- publication snapshot is prepared, then immutable until superseded/acknowledged.
create table if not exists sync_outbox(
    replica_seq INTEGER PRIMARY KEY,
    state_kind TEXT NOT NULL,
    state_key BLOB NOT NULL,
    state_subkey TEXT NOT NULL DEFAULT '',
    body BLOB,
    changed_at INTEGER,
    winning_version BLOB,
    UNIQUE(state_kind,state_key,state_subkey),
    CHECK ((body IS NULL) = (changed_at IS NULL))
);

CREATE TABLE IF NOT EXISTS sync_clock(singleton INTEGER PRIMARY KEY CHECK(singleton=1), sequence INTEGER NOT NULL);

-- Immutable book records use only an identity, payload and transport token.
-- The token preserves the original publication during retries and recovery;
-- none of its fields participate in timestamp-based conflict resolution.
create table if not exists sync_book_record(
    state_kind TEXT not null check(state_kind = 'book_add'),
    content_hash BLOB not null references book(content_hash) on delete cascade,
    body BLOB not null,
    publication BLOB not null,
    primary key(state_kind,content_hash)
);

-- Offline additions are local drafts, never inventory or outbox entries.
CREATE TABLE IF NOT EXISTS local_book_draft(
    content_hash BLOB PRIMARY KEY NOT NULL REFERENCES book(content_hash) ON DELETE CASCADE,
    body BLOB NOT NULL,
    retry_at INTEGER
) STRICT;

-- Canonical winning mutable registers. Domain tables are disposable projections.
create table if not exists sync_state_version(
    state_kind      TEXT not null check(state_kind != 'book_add'),
    state_key       BLOB not null,
    state_subkey    TEXT not null default '',
    changed_at      INTEGER not null,
    conflict_rank   INTEGER not null check(conflict_rank in (0, 1, 2)),
    replica_id      TEXT not null,
    replica_seq     INTEGER not null,
    body BLOB,
    book_key BLOB,
    primary key(state_kind, state_key, state_subkey)
);

CREATE TABLE IF NOT EXISTS sync_projection_dirty(
    state_kind TEXT NOT NULL, state_key BLOB NOT NULL, state_subkey TEXT NOT NULL,
    PRIMARY KEY(state_kind,state_key,state_subkey)
);

-- Replica metadata lives in one singleton row with one explicit column per
-- value. There is intentionally no key/value pair: every durable value must
-- be declared here so its storage type is visible in the schema.
create table if not exists sync_metadata(
    singleton                   INTEGER primary key check(singleton = 1),
    replica_id                  TEXT,
    pull_generation             TEXT,
    last_successful_sync_at     INTEGER check(last_successful_sync_at is null or last_successful_sync_at >= 0),
    last_pull_lifecycle_seq         INTEGER check(last_pull_lifecycle_seq is null or last_pull_lifecycle_seq >= 0),
    last_pull_field_seq       INTEGER check(last_pull_field_seq is null or last_pull_field_seq >= 0),
    cursor_recovery_pending     INTEGER check(cursor_recovery_pending is null or cursor_recovery_pending in (0, 1)),
    scan_seq                    INTEGER not null default 0 check(scan_seq >= 0),
    cloud_storage_enabled       INTEGER check(cloud_storage_enabled is null or cloud_storage_enabled in (0, 1)),
    remote_library_name         TEXT,
    server_library_created      TEXT
);
