CREATE TABLE snapshot(
                singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                schema_version INTEGER NOT NULL,
                dump_date TEXT NOT NULL,
                imported_at_ms INTEGER NOT NULL,
                edition_records INTEGER NOT NULL,
                work_records INTEGER NOT NULL,
                author_records INTEGER NOT NULL
             ) STRICT;
             CREATE TABLE import_setting(
                singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                record_limit INTEGER
             ) STRICT;
             CREATE TABLE import_input(
                name TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                modified_ms INTEGER NOT NULL
             ) WITHOUT ROWID;
             CREATE TABLE import_checkpoint(
                phase TEXT PRIMARY KEY,
                records_read INTEGER NOT NULL DEFAULT 0,
                completed INTEGER NOT NULL DEFAULT 0 CHECK(completed IN (0,1))
             ) WITHOUT ROWID;
             CREATE TABLE metadata_metric(
                name TEXT PRIMARY KEY,
                value INTEGER NOT NULL
             ) WITHOUT ROWID;
             CREATE TABLE edition(
                edition_id INTEGER PRIMARY KEY,
                work_id INTEGER
             ) STRICT;
             CREATE TABLE edition_isbn(
                isbn13 INTEGER NOT NULL,
                edition_id INTEGER NOT NULL,
                PRIMARY KEY(isbn13,edition_id)
             ) WITHOUT ROWID;
             CREATE INDEX edition_isbn_by_edition ON edition_isbn(edition_id,isbn13);
             CREATE TABLE edition_bibliography(
                edition_id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                normalized_title TEXT NOT NULL,
                book_year INTEGER
             ) STRICT;
             CREATE INDEX edition_bibliography_by_title ON edition_bibliography(normalized_title,edition_id);
             CREATE TABLE work_bibliography(
                work_id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                normalized_title TEXT NOT NULL
             ) STRICT;
             CREATE INDEX work_bibliography_by_title ON work_bibliography(normalized_title,work_id);
             CREATE TABLE work_description(
                work_id INTEGER PRIMARY KEY,
                description TEXT NOT NULL
             ) STRICT;
             CREATE TABLE edition_publisher(
                edition_id INTEGER NOT NULL,
                position INTEGER NOT NULL,
                name TEXT NOT NULL,
                normalized_name TEXT NOT NULL,
                PRIMARY KEY(edition_id,position)
             ) WITHOUT ROWID;
             CREATE TABLE edition_classification(
                edition_id INTEGER NOT NULL,
                scheme INTEGER NOT NULL CHECK(scheme IN (1,2)),
                notation TEXT NOT NULL,
                PRIMARY KEY(edition_id,scheme,notation)
             ) WITHOUT ROWID;
             CREATE TABLE isbn_classification(
                isbn13 INTEGER NOT NULL,
                scheme INTEGER NOT NULL CHECK(scheme IN (1,2)),
                notation TEXT NOT NULL,
                PRIMARY KEY(isbn13,scheme,notation)
             ) WITHOUT ROWID;
             CREATE INDEX isbn_classification_by_code ON isbn_classification(scheme,notation,isbn13);
             CREATE TABLE title_author_classification(
                normalized_title TEXT NOT NULL,
                normalized_subtitle TEXT NOT NULL,
                normalized_author TEXT NOT NULL,
                book_year INTEGER NOT NULL,
                normalized_publisher TEXT NOT NULL,
                scheme INTEGER NOT NULL CHECK(scheme IN (1,2)),
                notation TEXT NOT NULL,
                PRIMARY KEY(normalized_title,normalized_subtitle,normalized_author,book_year,normalized_publisher,scheme,notation)
             ) WITHOUT ROWID;
             CREATE INDEX title_author_classification_lookup ON title_author_classification(normalized_title,normalized_subtitle,normalized_author,book_year,normalized_publisher);
             CREATE TABLE edition_work_classification(
                work_id INTEGER NOT NULL,
                scheme INTEGER NOT NULL CHECK(scheme IN (1,2)),
                notation TEXT NOT NULL,
                PRIMARY KEY(work_id,scheme,notation)
             ) WITHOUT ROWID;
             CREATE TABLE work_classification(
                work_id INTEGER NOT NULL,
                scheme INTEGER NOT NULL CHECK(scheme IN (1,2)),
                notation TEXT NOT NULL,
                PRIMARY KEY(work_id,scheme,notation)
             ) WITHOUT ROWID;
             CREATE TABLE edition_author(
                edition_id INTEGER NOT NULL,
                position INTEGER NOT NULL,
                author_id INTEGER NOT NULL,
                PRIMARY KEY(edition_id,position)
             ) WITHOUT ROWID;
             CREATE TABLE work_author(
                work_id INTEGER NOT NULL,
                position INTEGER NOT NULL,
                author_id INTEGER NOT NULL,
                PRIMARY KEY(work_id,position)
             ) WITHOUT ROWID;
             CREATE TABLE author(
                author_id INTEGER PRIMARY KEY,
                name TEXT
             ) STRICT;
             CREATE TABLE author_identifier(
                author_id INTEGER NOT NULL,
                authority TEXT NOT NULL,
                external_id TEXT NOT NULL,
                PRIMARY KEY(author_id,authority,external_id)
             ) WITHOUT ROWID;
             -- Import staging tables intentionally have no indexes. Dump rows
             -- are appended cheaply and then sorted/deduplicated into the
             -- query schema once per phase.
             CREATE TABLE edition_stage(edition_id INTEGER NOT NULL, work_id INTEGER);
             CREATE TABLE edition_isbn_stage(isbn13 INTEGER NOT NULL, edition_id INTEGER NOT NULL);
             CREATE TABLE edition_bibliography_stage(edition_id INTEGER NOT NULL, title TEXT NOT NULL, normalized_title TEXT NOT NULL, book_year INTEGER);
             CREATE TABLE edition_publisher_stage(edition_id INTEGER NOT NULL, position INTEGER NOT NULL, name TEXT NOT NULL, normalized_name TEXT NOT NULL);
             CREATE TABLE edition_classification_stage(edition_id INTEGER NOT NULL, scheme INTEGER NOT NULL, notation TEXT NOT NULL);
             CREATE TABLE edition_work_classification_stage(work_id INTEGER NOT NULL, scheme INTEGER NOT NULL, notation TEXT NOT NULL);
             CREATE TABLE edition_author_stage(edition_id INTEGER NOT NULL, position INTEGER NOT NULL, author_id INTEGER NOT NULL);
             CREATE TABLE work_classification_stage(work_id INTEGER NOT NULL, scheme INTEGER NOT NULL, notation TEXT NOT NULL);
             CREATE TABLE work_bibliography_stage(work_id INTEGER NOT NULL, title TEXT NOT NULL, normalized_title TEXT NOT NULL);
             CREATE TABLE work_description_stage(work_id INTEGER NOT NULL, description TEXT NOT NULL);
             CREATE TABLE work_author_stage(work_id INTEGER NOT NULL, position INTEGER NOT NULL, author_id INTEGER NOT NULL);
             CREATE TABLE author_stage(author_id INTEGER NOT NULL, name TEXT);
             CREATE TABLE author_identifier_stage(author_id INTEGER NOT NULL, authority TEXT NOT NULL, external_id TEXT NOT NULL);
             INSERT INTO metadata_metric(name,value) VALUES('builder_schema_version',9);

-- Optional subtitle indexes preserve compatibility with older query snapshots.
CREATE TABLE IF NOT EXISTS edition_subtitle(edition_id INTEGER PRIMARY KEY, subtitle TEXT NOT NULL, normalized_title TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS edition_subtitle_by_title ON edition_subtitle(normalized_title,edition_id);
CREATE TABLE IF NOT EXISTS work_subtitle(work_id INTEGER PRIMARY KEY, subtitle TEXT NOT NULL, normalized_title TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS work_subtitle_by_title ON work_subtitle(normalized_title,work_id);
