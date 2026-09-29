-- name: schema_books &
-- Book rows, book metadata projection, and subject taxonomy.
create table if not exists book(
    row_id          INTEGER primary key,
    -- Portable identity used by sync and asset APIs. Local relationships use
    -- row_id so their indexes remain compact and integer joins stay cheap.
    content_hash    TEXT not null unique,
    title           TEXT,
    subtitle        TEXT,
    description_scanned INTEGER not null default 0,
    -- Canonical synchronized bibliographic metadata. Dedicated tables below
    -- are disposable query projections rebuilt from this value.
    book_metadata BLOB not null default X'',
    -- Canonical extension of the book's format ('epub', 'pdf', ...). Fixed for
    -- the life of the row: identity is derived from the bytes, so the format
    -- cannot change without becoming a different book. Empty only on a row that
    -- has never been observed on any device's disk.
    format          TEXT not null default '',
    -- Device-local library visibility. This is intentionally not synchronized.
    hidden_at       INTEGER,
    read_pos        TEXT                DEFAULT NULL,
    read_progress   REAL                DEFAULT 0,
    added_at        INTEGER,
    deleted_at      INTEGER,
    trash_origin_dir_id TEXT
);


create table if not exists book_identifier(
    book_row_id         INTEGER not null references book(row_id) on delete cascade,
    position        INTEGER not null check(position >= 0),
    scheme          TEXT not null,
    value           TEXT not null,
    canonical_value TEXT,
    scope           TEXT not null default 'book' check(scope in ('book','edition','work')),
    primary key(book_row_id, position)
);
create index if not exists idx_book_identifier_lookup on book_identifier(scheme, value, book_row_id);
create table if not exists book_language(
    book_row_id         INTEGER not null references book(row_id) on delete cascade,
    position        INTEGER not null check(position >= 0),
    language_tag    TEXT not null,
    primary key(book_row_id, position)
);
create index if not exists idx_book_language_tag on book_language(language_tag, book_row_id);
-- Merged relational contributor register. Every role (including "aut" =
-- author) shares one flat per-book position sequence. This replaces the
-- former separate book_author table; author_credit (see views.sql) exposes
-- the author-only subset for call sites that only care about authorship.
create table if not exists book_contributor(
    book_row_id         INTEGER not null references book(row_id) on delete cascade,
    position        INTEGER not null check(position >= 0),
    author_identity_id INTEGER not null references author_identity(id) on delete cascade,
    name            TEXT not null,
    sort_name       TEXT,
    role            BLOB not null check(typeof(role) = 'blob' and length(role) = 3),
    primary key(book_row_id, position)
);
create index if not exists idx_book_contributor_identity on book_contributor(author_identity_id, book_row_id);
create table if not exists book_subject(
    book_row_id         INTEGER not null references book(row_id) on delete cascade,
    position        INTEGER not null check(position >= 0),
    name            TEXT not null,
    source          TEXT not null,
    authority       TEXT,
    code            TEXT,
    primary key(book_row_id, position)
);
create index if not exists idx_book_subject_name on book_subject(name, book_row_id);
create index if not exists idx_book_subject_authority on book_subject(authority, book_row_id);
create table if not exists book_subject_code(
    book_row_id     INTEGER not null,
    subject_position INTEGER not null,
    system_id       TEXT not null,
    code            TEXT not null,
    primary key(book_row_id, subject_position, system_id, code),
    foreign key(book_row_id, subject_position) references book_subject(book_row_id, position) on delete cascade
);
create index if not exists idx_book_subject_code_lookup on book_subject_code(system_id, code, book_row_id);

-- Controlled taxonomies and disposable automatic assignments. The raw
-- declarations above remain the evidence and synchronization boundary.
create table if not exists subject_system(
    id              TEXT primary key,
    version         TEXT not null,
    matcher_version INTEGER not null check(matcher_version >= 0)
);
create table if not exists subject_node(
    system_id       TEXT not null references subject_system(id) on delete cascade,
    path            TEXT not null,
    parent_path     TEXT not null,
    name            TEXT not null,
    code            TEXT,
    assignable      INTEGER not null check(assignable in (0, 1)),
    primary key(system_id, path),
    unique(system_id, code)
);
create index if not exists idx_subject_node_parent on subject_node(system_id, parent_path, name);
create table if not exists book_subject_assignment(
    book_row_id     INTEGER not null references book(row_id) on delete cascade,
    system_id       TEXT not null,
    subject_path    TEXT not null,
    matcher_version INTEGER not null check(matcher_version >= 0),
    primary key(book_row_id, system_id, subject_path),
    foreign key(system_id, subject_path) references subject_node(system_id, path) on delete cascade
);
create index if not exists idx_book_subject_assignment_subject on book_subject_assignment(system_id, subject_path, book_row_id);
create table if not exists book_subject_assignment_evidence(
    book_row_id     INTEGER not null,
    system_id       TEXT not null,
    subject_path    TEXT not null,
    subject_position INTEGER not null,
    primary key(book_row_id, system_id, subject_path, subject_position),
    foreign key(book_row_id, system_id, subject_path) references book_subject_assignment(book_row_id, system_id, subject_path) on delete cascade,
    foreign key(book_row_id, subject_position) references book_subject(book_row_id, position) on delete cascade
);
create table if not exists subject_code_mapping(
    source_system_id TEXT not null,
    source_code      TEXT not null,
    unified_path     TEXT not null,
    mapper_version   INTEGER not null check(mapper_version >= 0),
    primary key(source_system_id, source_code, unified_path)
);
create index if not exists idx_subject_code_mapping_path on subject_code_mapping(unified_path, source_system_id, source_code);

-- Curated subjects and navigation live in the attached read-only taxonomy.
-- Library storage contains book assignments and their evidence only.
create table if not exists source_subject(
    system_id       TEXT not null,
    source_version  TEXT not null,
    code            TEXT not null,
    official_label  TEXT,
    primary key(system_id,source_version,code)
);
create table if not exists source_concept_mapping(
    source_system_id TEXT not null,
    source_version   TEXT not null,
    source_code      TEXT not null,
    concept_id INTEGER not null check(concept_id > 0),
    mapping_type     TEXT not null check(mapping_type in ('exact','broader','narrower','related')),
    mapper_version   INTEGER not null check(mapper_version >= 0),
    primary key(source_system_id,source_version,source_code,concept_id),
    foreign key(source_system_id,source_version,source_code) references source_subject(system_id,source_version,code) on delete cascade
);
create index if not exists idx_source_concept_mapping_concept on source_concept_mapping(concept_id,source_system_id,source_code);
create table if not exists book_unified_concept(
    book_row_id     INTEGER not null references book(row_id) on delete cascade,
    concept_id INTEGER not null check(concept_id > 0),
    mapper_version  INTEGER not null check(mapper_version >= 0),
    primary key(book_row_id,concept_id)
);
create index if not exists idx_book_unified_concept_concept on book_unified_concept(concept_id,book_row_id);
create table if not exists book_unified_concept_evidence(
    book_row_id      INTEGER not null,
    concept_id INTEGER not null,
    subject_position INTEGER not null,
    source_system_id TEXT not null,
    source_code      TEXT not null,
    primary key(book_row_id,concept_id,subject_position,source_system_id,source_code),
    foreign key(book_row_id,concept_id) references book_unified_concept(book_row_id,concept_id) on delete cascade,
    foreign key(book_row_id,subject_position,source_system_id,source_code) references book_subject_code(book_row_id,subject_position,system_id,code) on delete cascade
);

-- Local bookkeeping for explicit external enrichment. Provider vocabularies
-- are never cached here; this records only whether one book was attempted.

-- name: schema_authors &
-- Author identity and external identifiers.
create table if not exists author_identity(
    id              INTEGER primary key autoincrement,
    stable_id       TEXT not null unique,
    preferred_name  TEXT not null,
    -- Name matching only selects an unresolved candidate. stable_id and
    -- authority identifiers remain the identity boundary for real people.
    normalized_name TEXT not null,
    is_provisional  INTEGER not null default 1 check(is_provisional in (0, 1)),
    created_at      INTEGER not null
);

create unique index if not exists idx_author_identity_stable_id
on author_identity(stable_id);

create index if not exists idx_author_identity_normalized_name
on author_identity(normalized_name, is_provisional);

create table if not exists author_external_identifier(
    author_identity_id INTEGER not null references author_identity(id) on delete cascade,
    authority          TEXT not null check(authority in ('wikidata', 'open_library', 'viaf', 'isni', 'orcid', 'library_of_congress')),
    external_id        TEXT not null,
    verified_at        INTEGER not null,
    primary key(authority, external_id)
);

-- name: schema_descriptions &
-- Canonical description storage, kept off the frequently read book row.
CREATE TABLE IF NOT EXISTS book_description(
    book_row_id INTEGER PRIMARY KEY REFERENCES book(row_id) ON DELETE CASCADE,
    description TEXT NOT NULL
);

-- name: schema_description_triggers &
-- Description change triggers.
CREATE TRIGGER book_description_insert AFTER INSERT ON book_description

BEGIN
    UPDATE subject_browse_state SET revision=revision+1 WHERE id=1;
    SELECT bokheim_accept_local(bokheim_mutation('description',b.content_hash,NEW.description), CAST((julianday('now')-2440587.5)*86400000 AS INTEGER)) FROM book b WHERE b.row_id=NEW.book_row_id
      AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1),'local')='local';
END;
CREATE TRIGGER book_description_update AFTER UPDATE OF description ON book_description
WHEN OLD.description IS NOT NEW.description
BEGIN
    UPDATE subject_browse_state SET revision=revision+1 WHERE id=1;
    SELECT bokheim_accept_local(bokheim_mutation('description',b.content_hash,NEW.description), CAST((julianday('now')-2440587.5)*86400000 AS INTEGER)) FROM book b WHERE b.row_id=NEW.book_row_id
      AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1),'local')='local';
END;
CREATE TRIGGER book_description_delete AFTER DELETE ON book_description

BEGIN
    UPDATE subject_browse_state SET revision=revision+1 WHERE id=1;
    SELECT bokheim_accept_local(bokheim_mutation('description',b.content_hash,''), CAST((julianday('now')-2440587.5)*86400000 AS INTEGER)) FROM book b WHERE b.row_id=OLD.book_row_id
      AND COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1),'local')='local';
END;

-- name: schema_contents &
-- Book navigation, reading entry, and audiobook metadata.
-- The book's own navigation document, whatever the format called it:
-- a nav document, an outline, or a chapter list. Stored so the library can draw
-- a book's contents without opening the file — which may be remote, and which
-- the reader would otherwise re-parse on every visit. One row per book, holding
-- the whole tree: it is always read whole and nothing joins against its
-- entries, which is why it is JSON rather than a row per entry. An existing row
-- with no entries records that the file was inspected and carries no
-- navigation, so it is not parsed again.
--
-- Keyed by the book's identity, not by the bytes it was read from: an identity
-- survives a revision, and so do the targets, because a PDF's pagination and an
-- audiobook's offsets are both fixed by what the identity is derived from.
create table if not exists book_toc(
    content_hash    TEXT primary key not null,
    entry_count     INTEGER not null check(entry_count >= 0),
    toc_json        TEXT not null
);

-- Which navigation entry the reader was inside when it last saved a position.
-- Derived from the position and the book's own navigation, so it is local and
-- never synchronized: another device recomputes it when it opens the book.
-- Stored as the entry's target rather than its title, so a re-parsed navigation
-- document supplies the wording.
create table if not exists book_reading_entry(
    content_hash    TEXT primary key not null,
    target          TEXT not null
);

-- What an audiobook is, beyond its navigation. The chapters themselves live in
-- book_toc with every other format's; the duration stays here because it is not
-- navigation, and because it is what turns a chapter's start into its end.
create table if not exists audiobook_metadata(
    content_hash    TEXT primary key not null,
    duration_ms     INTEGER not null check(duration_ms >= 0)
);

create table if not exists audiobook_track_index(
    content_hash TEXT primary key not null,
    tracks_json TEXT not null
);

-- name: schema_sync &
-- Durable synchronization outbox, winning versions, and replica metadata.
-- A pending row identifies a register. Payload columns are NULL until a
-- publication snapshot is prepared, then immutable until superseded/acknowledged.
create table if not exists sync_outbox(
    replica_seq INTEGER PRIMARY KEY,
    mutation_id TEXT NOT NULL UNIQUE,
    state_kind TEXT NOT NULL,
    state_key TEXT NOT NULL,
    state_subkey TEXT NOT NULL DEFAULT '',
    body BLOB,
    changed_at INTEGER,
    origin BLOB,
    UNIQUE(state_kind,state_key,state_subkey),
    CHECK ((body IS NULL) = (changed_at IS NULL))
);
CREATE TABLE IF NOT EXISTS sync_clock(singleton INTEGER PRIMARY KEY CHECK(singleton=1), sequence INTEGER NOT NULL);
INSERT OR IGNORE INTO sync_clock VALUES(1,0);

-- Canonical winning registers. Domain tables are disposable projections.
create table if not exists sync_state_version(
    state_kind      TEXT not null,
    state_key       TEXT not null,
    state_subkey    TEXT not null default '',
    changed_at      INTEGER not null,
    conflict_rank   INTEGER not null check(conflict_rank in (0, 1, 2)),
    replica_id      TEXT not null,
    replica_seq     INTEGER not null,
    mutation_id     TEXT not null,
    body BLOB,
    book_key TEXT,
    primary key(state_kind, state_key, state_subkey)
);
CREATE INDEX IF NOT EXISTS sync_state_book ON sync_state_version(book_key);
CREATE TABLE IF NOT EXISTS sync_projection_dirty(
    state_kind TEXT NOT NULL, state_key TEXT NOT NULL, state_subkey TEXT NOT NULL,
    PRIMARY KEY(state_kind,state_key,state_subkey)
);

-- Replica metadata lives in one singleton row with one explicit column per
-- value. There is intentionally no key/value pair: every durable value must
-- be declared here so its storage type is visible in the schema.
create table if not exists sync_metadata(
    singleton                   INTEGER primary key check(singleton = 1),
    replica_id                  TEXT,
    change_origin               TEXT not null default 'local' check(change_origin in ('local', 'remote')),
    last_successful_sync_at     INTEGER check(last_successful_sync_at is null or last_successful_sync_at >= 0),
    last_pull_state_seq         INTEGER check(last_pull_state_seq is null or last_pull_state_seq >= 0),
    last_pull_reading_seq       INTEGER check(last_pull_reading_seq is null or last_pull_reading_seq >= 0),
    inventory_repair_kind       TEXT,
    inventory_repair_key        TEXT,
    inventory_repair_subkey     TEXT,
    cursor_recovery_pending     INTEGER check(cursor_recovery_pending is null or cursor_recovery_pending in (0, 1)),
    scan_seq                    INTEGER not null default 0 check(scan_seq >= 0),
    operation_id                TEXT,
    operation_kind              TEXT check(operation_kind is null or operation_kind in ('Import', 'Sync')),
    operation_scan_complete     INTEGER check(operation_scan_complete is null or operation_scan_complete in (0, 1)),
    operation_scan_failed       INTEGER check(operation_scan_failed is null or operation_scan_failed in (0, 1)),
    operation_discovered        INTEGER check(operation_discovered is null or operation_discovered >= 0),
    operation_failures          INTEGER check(operation_failures is null or operation_failures >= 0),
    operation_requires_sync     INTEGER check(operation_requires_sync is null or operation_requires_sync in (0, 1)),
    operation_completed         INTEGER check(operation_completed is null or operation_completed in (0, 1)),
    operation_sync_unfinished   INTEGER check(operation_sync_unfinished is null or operation_sync_unfinished in (0, 1)),
    cloud_storage_enabled       INTEGER check(cloud_storage_enabled is null or cloud_storage_enabled in (0, 1)),
    remote_library_name         TEXT,
    server_library_created      TEXT
);
insert or ignore into sync_metadata(singleton) values(1);

-- Session-local UI history is not synchronization metadata. It has one
-- durable value per library and therefore has its own table.
create table if not exists local_transfer_history(
    singleton    INTEGER primary key check(singleton = 1),
    entries_json TEXT not null
);

-- Assets confirmed to exist remotely. Absence means unknown, not missing.
