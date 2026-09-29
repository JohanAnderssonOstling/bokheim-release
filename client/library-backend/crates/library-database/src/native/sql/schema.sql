-- name: schema_jobs &
-- Device-local work queues and ingestion output. None of this is replicated.
CREATE TABLE IF NOT EXISTS local_file_work(
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        operation TEXT NOT NULL CHECK(operation IN ('trash','restore')),
        content_hash TEXT NOT NULL,
        relative_path TEXT NOT NULL,
        UNIQUE(operation,content_hash,relative_path)
    );
    CREATE TABLE IF NOT EXISTS local_directory_work(
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        dir_id TEXT NOT NULL UNIQUE
    );
    CREATE TABLE IF NOT EXISTS local_book_work(id INTEGER PRIMARY KEY AUTOINCREMENT, content_hash TEXT NOT NULL UNIQUE);
    CREATE TABLE IF NOT EXISTS local_purge_work(id INTEGER PRIMARY KEY AUTOINCREMENT, content_hash TEXT NOT NULL UNIQUE);
    CREATE TABLE IF NOT EXISTS local_file_projection(
        content_hash TEXT NOT NULL, dir_id TEXT NOT NULL, relative_path TEXT NOT NULL,
        PRIMARY KEY(content_hash,dir_id)
    );
    CREATE INDEX IF NOT EXISTS idx_local_file_projection_path ON local_file_projection(relative_path);
    CREATE TABLE IF NOT EXISTS local_directory_projection(
        dir_id TEXT PRIMARY KEY,relative_path TEXT NOT NULL
    );

CREATE TABLE IF NOT EXISTS local_thumbnail_work(
        content_hash TEXT PRIMARY KEY REFERENCES book(content_hash) ON DELETE CASCADE,
        state TEXT NOT NULL CHECK(state IN ('pending','ready','no_cover')),
        retry_after INTEGER NOT NULL DEFAULT 0
    );

-- Byte provenance, separate from recovery state: ready does not mean authored here.
CREATE TABLE local_thumbnail_source(
    content_hash TEXT PRIMARY KEY NOT NULL REFERENCES book(content_hash) ON DELETE CASCADE,
    origin TEXT NOT NULL CHECK(origin IN ('local','remote'))
) STRICT;

CREATE TABLE local_thumbnail_sidecar(
    content_hash TEXT PRIMARY KEY NOT NULL REFERENCES book(content_hash) ON DELETE CASCADE
) STRICT;

CREATE TABLE IF NOT EXISTS local_import_inspection(
        content_hash TEXT PRIMARY KEY REFERENCES book(content_hash) ON DELETE CASCADE,
        format TEXT NOT NULL,
        version INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS local_subject_pipeline(
        content_hash TEXT PRIMARY KEY REFERENCES book(content_hash) ON DELETE CASCADE,
        phase INTEGER NOT NULL, revision INTEGER NOT NULL,
        metadata BLOB NOT NULL, title TEXT NOT NULL,
        complete INTEGER NOT NULL DEFAULT 0, retry_at INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS local_isbn_response(
        isbn TEXT PRIMARY KEY, response TEXT NOT NULL, expires_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS local_isbn_response_expiry ON local_isbn_response(expires_at DESC,isbn);
    

CREATE TABLE IF NOT EXISTS local_upload_preparation (
    content_hash TEXT PRIMARY KEY REFERENCES book(content_hash) ON DELETE CASCADE,
    upload_id INTEGER
);

CREATE TABLE IF NOT EXISTS audible_enrichment_jobs(
        content_hash TEXT PRIMARY KEY REFERENCES audiobook_metadata(content_hash) ON DELETE CASCADE,
        policy TEXT NOT NULL,
        request_json TEXT NOT NULL,
        status TEXT NOT NULL CHECK(status IN ('pending','resolving','applied','no_match','unavailable','ambiguous')),
        response_json TEXT,
        claim_token TEXT,
        lease_until INTEGER,
        retry_at INTEGER NOT NULL DEFAULT 0,
        updated_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS audible_enrichment_due ON audible_enrichment_jobs(status,retry_at,lease_until);
    CREATE VIEW IF NOT EXISTS pending_audible_enrichment AS
        SELECT content_hash FROM audible_enrichment_jobs WHERE status IN ('pending','resolving');

CREATE TABLE IF NOT EXISTS pdf_reader_metadata (
        content_hash TEXT NOT NULL REFERENCES book(content_hash) ON DELETE CASCADE,
        checksum TEXT NOT NULL, metadata BLOB NOT NULL, PRIMARY KEY(content_hash,checksum)
    );

CREATE TRIGGER IF NOT EXISTS pdf_reader_metadata_insert AFTER INSERT ON pdf_reader_metadata
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('pdf_reader_metadata',NEW.content_hash,NEW.metadata), CAST((julianday('now')-2440587.5)*86400000 AS INTEGER)) WHERE COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1),'local')='local';
END;
CREATE TRIGGER IF NOT EXISTS pdf_reader_metadata_update AFTER UPDATE ON pdf_reader_metadata
BEGIN
    SELECT bokheim_accept_local(bokheim_mutation('pdf_reader_metadata',NEW.content_hash,NEW.metadata), CAST((julianday('now')-2440587.5)*86400000 AS INTEGER)) WHERE COALESCE((SELECT change_origin FROM sync_metadata WHERE singleton = 1),'local')='local';
END;

-- name: schema_activity &
CREATE TABLE IF NOT EXISTS local_scan_failure(path TEXT PRIMARY KEY, error TEXT NOT NULL);
CREATE TRIGGER IF NOT EXISTS thumbnail_cancel_deleted_book AFTER UPDATE OF deleted_at ON book
WHEN NEW.deleted_at IS NOT NULL
BEGIN DELETE FROM local_thumbnail_work WHERE content_hash=NEW.content_hash AND state='pending'; END;
CREATE TRIGGER IF NOT EXISTS thumbnail_restore_book AFTER UPDATE OF deleted_at ON book
WHEN OLD.deleted_at IS NOT NULL AND NEW.deleted_at IS NULL
BEGIN INSERT INTO local_thumbnail_work(content_hash,state) VALUES(NEW.content_hash,'pending') ON CONFLICT(content_hash) DO NOTHING; END;
CREATE TRIGGER IF NOT EXISTS thumbnail_reject_deleted_insert AFTER INSERT ON local_thumbnail_work
WHEN NEW.state='pending' AND EXISTS(SELECT 1 FROM book WHERE content_hash=NEW.content_hash AND deleted_at IS NOT NULL)
BEGIN DELETE FROM local_thumbnail_work WHERE content_hash=NEW.content_hash; END;
CREATE TRIGGER IF NOT EXISTS thumbnail_reject_deleted_update AFTER UPDATE ON local_thumbnail_work
WHEN NEW.state='pending' AND EXISTS(SELECT 1 FROM book WHERE content_hash=NEW.content_hash AND deleted_at IS NOT NULL)
BEGIN DELETE FROM local_thumbnail_work WHERE content_hash=NEW.content_hash; END;
DELETE FROM local_thumbnail_work WHERE state='pending' AND content_hash IN (SELECT content_hash FROM book WHERE deleted_at IS NOT NULL);
