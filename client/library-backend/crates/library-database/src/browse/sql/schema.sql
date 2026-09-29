-- name: schema_browse &
-- Rebuildable browse projections. The rebuild statements that maintain
-- them live with the browse module; only their tables are created here.
--
-- Rebuildable browse dimensions keep normalization and hierarchy projection
-- out of interactive facet queries.
CREATE TABLE IF NOT EXISTS book_browse_format(
    book_row_id INTEGER PRIMARY KEY REFERENCES book(row_id) ON DELETE CASCADE,
    format_category INTEGER NOT NULL CHECK(format_category IN (1, 2, 4))
);

CREATE TABLE IF NOT EXISTS book_browse_language(
    book_row_id INTEGER NOT NULL REFERENCES book(row_id) ON DELETE CASCADE,
    base_language TEXT NOT NULL,
    PRIMARY KEY(book_row_id, base_language)
);
CREATE INDEX IF NOT EXISTS idx_book_browse_language_value ON book_browse_language(base_language, book_row_id);

-- name: schema_browse_filters &
-- Maintain normalized browse filters on local and replicated metadata writes.
CREATE TRIGGER trig_book_browse_format_insert
AFTER INSERT ON book
BEGIN
    INSERT INTO book_browse_format(book_row_id,format_category)
    VALUES(NEW.row_id,CASE lower(NEW.format) WHEN 'pdf' THEN 2 WHEN 'm4b' THEN 4 WHEN 'mp3folder' THEN 4 ELSE 1 END)
    ON CONFLICT(book_row_id) DO UPDATE SET format_category=excluded.format_category;
END;

CREATE TRIGGER trig_book_browse_format_update
AFTER UPDATE OF format ON book
BEGIN
    INSERT INTO book_browse_format(book_row_id,format_category)
    VALUES(NEW.row_id,CASE lower(NEW.format) WHEN 'pdf' THEN 2 WHEN 'm4b' THEN 4 WHEN 'mp3folder' THEN 4 ELSE 1 END)
    ON CONFLICT(book_row_id) DO UPDATE SET format_category=excluded.format_category;
END;

CREATE TRIGGER trig_book_browse_language_insert
AFTER INSERT ON book_language
BEGIN
    DELETE FROM book_browse_language WHERE book_row_id=NEW.book_row_id;
    INSERT INTO book_browse_language(book_row_id,base_language)
    SELECT DISTINCT book_row_id,
        CASE WHEN instr(replace(lower(trim(language_tag)),'_','-'),'-')>0
            THEN substr(replace(lower(trim(language_tag)),'_','-'),1,instr(replace(lower(trim(language_tag)),'_','-'),'-')-1)
            ELSE replace(lower(trim(language_tag)),'_','-') END
    FROM book_language WHERE book_row_id=NEW.book_row_id AND trim(language_tag)!='';
END;

CREATE TRIGGER trig_book_browse_language_update
AFTER UPDATE OF language_tag,book_row_id ON book_language
BEGIN
    DELETE FROM book_browse_language WHERE book_row_id IN (OLD.book_row_id,NEW.book_row_id);
    INSERT OR IGNORE INTO book_browse_language(book_row_id,base_language)
    SELECT DISTINCT book_row_id,
        CASE WHEN instr(replace(lower(trim(language_tag)),'_','-'),'-')>0
            THEN substr(replace(lower(trim(language_tag)),'_','-'),1,instr(replace(lower(trim(language_tag)),'_','-'),'-')-1)
            ELSE replace(lower(trim(language_tag)),'_','-') END
    FROM book_language WHERE book_row_id IN (OLD.book_row_id,NEW.book_row_id) AND trim(language_tag)!='';
END;

CREATE TRIGGER trig_book_browse_language_delete
AFTER DELETE ON book_language
BEGIN
    DELETE FROM book_browse_language WHERE book_row_id=OLD.book_row_id;
    INSERT INTO book_browse_language(book_row_id,base_language)
    SELECT DISTINCT book_row_id,
        CASE WHEN instr(replace(lower(trim(language_tag)),'_','-'),'-')>0
            THEN substr(replace(lower(trim(language_tag)),'_','-'),1,instr(replace(lower(trim(language_tag)),'_','-'),'-')-1)
            ELSE replace(lower(trim(language_tag)),'_','-') END
    FROM book_language WHERE book_row_id=OLD.book_row_id AND trim(language_tag)!='';
END;

-- name: schema_views &
-- Browse views.
CREATE VIEW IF NOT EXISTS dir_paths AS
WITH RECURSIVE dir_path(id, path) AS (
    -- root produces empty path
    SELECT id, ''
    FROM dir
    WHERE parent_id = id
      AND deleted_at IS NULL

    UNION ALL

    -- children append normally
    SELECT
        d.id,
        CASE
            WHEN dp.path = ''
                THEN d.name
            ELSE dp.path || '/' || d.name
        END
    FROM dir d
    JOIN dir_path dp ON d.parent_id = dp.id
    WHERE d.deleted_at IS NULL
      AND d.id <> d.parent_id
)
SELECT id, path FROM dir_path;

-- Author-role subset of the merged contributor register. Kept as a view so
-- the `role = X'617574'` (MarcRelatorCode::AUTHOR) literal is not repeated
-- across every query that only cares about credited authors.
CREATE VIEW IF NOT EXISTS author_credit AS
SELECT book_row_id, position, author_identity_id, name AS credited_name, sort_name
FROM book_contributor
WHERE role = X'617574';

CREATE VIEW IF NOT EXISTS book_cards AS
SELECT
    book.row_id AS book_row_id,
    book.content_hash,
    book.title,
    COALESCE(book.added_at, 0) AS added_at,
    book.subtitle AS subtitle,
    text.author,text.search_title,text.search_author,text.sort_title,
    COALESCE((SELECT description FROM book_description WHERE book_row_id=book.row_id), '') AS description,
    COALESCE(progress.progress,0) AS progress,
    EXISTS (
        SELECT 1 FROM book_dir placement
        WHERE placement.book_row_id = book.row_id
          AND placement.deleted_at IS NULL
          AND placement.is_downloaded = 1
    ) AS downloaded,
    EXISTS (
        SELECT 1 FROM book_dir placement
        JOIN download_requests request
          ON request.target_kind = 'book'
         AND request.target_key = book.content_hash
        WHERE placement.book_row_id = book.row_id AND placement.deleted_at IS NULL
    ) AS download_requested,
    format.format_category,
    (
        SELECT metadata.duration_ms FROM book_dir placement
        JOIN audiobook_metadata metadata ON metadata.content_hash = book.content_hash
        WHERE placement.book_row_id = book.row_id AND placement.deleted_at IS NULL
        ORDER BY placement.rowid DESC LIMIT 1
    ) AS audiobook_duration_ms,
    (
        SELECT toc.entry_count FROM book_dir placement
        JOIN book_toc toc ON toc.content_hash = book.content_hash
        JOIN audiobook_metadata metadata ON metadata.content_hash = book.content_hash
        WHERE placement.book_row_id = book.row_id AND placement.deleted_at IS NULL
        ORDER BY placement.rowid DESC LIMIT 1
    ) AS audiobook_chapter_count
FROM book
JOIN book_browse_format format ON format.book_row_id=book.row_id
JOIN book_browse_text text ON text.book_row_id=book.row_id
LEFT JOIN book_browse_progress progress ON progress.book_row_id=book.row_id
WHERE book.deleted_at IS NULL
  AND book.hidden_at IS NULL
  AND EXISTS (
      SELECT 1 FROM book_dir placement
      WHERE placement.book_row_id = book.row_id AND placement.deleted_at IS NULL
  );

-- name: schema_browse_progress &
CREATE TABLE IF NOT EXISTS book_browse_progress(
    book_row_id INTEGER PRIMARY KEY REFERENCES book(row_id) ON DELETE CASCADE,
    progress REAL NOT NULL
);
CREATE TRIGGER IF NOT EXISTS browse_progress_insert AFTER INSERT ON book
BEGIN
    INSERT INTO book_browse_progress VALUES(NEW.row_id,browse_reading_progress(NEW.read_progress))
    ON CONFLICT(book_row_id) DO UPDATE SET progress=excluded.progress;
END;
CREATE TRIGGER IF NOT EXISTS browse_progress_update AFTER UPDATE OF read_pos,read_progress ON book
BEGIN
    INSERT INTO book_browse_progress VALUES(NEW.row_id,browse_reading_progress(NEW.read_progress))
    ON CONFLICT(book_row_id) DO UPDATE SET progress=excluded.progress;
END;
INSERT INTO book_browse_progress SELECT row_id,browse_reading_progress(read_progress) FROM book WHERE true
ON CONFLICT(book_row_id) DO UPDATE SET progress=excluded.progress;

-- name: schema_browse_text &
-- Rebuildable display/search data. The scalar normalizer is implemented in
-- Rust; triggers run it at metadata-write time, including remote mutations.
CREATE TABLE IF NOT EXISTS book_browse_text(
    book_row_id INTEGER PRIMARY KEY REFERENCES book(row_id) ON DELETE CASCADE,
    author TEXT NOT NULL,
    search_title TEXT NOT NULL,
    search_author TEXT NOT NULL,
    sort_title TEXT NOT NULL
);

-- Only projection writes read this view. Interactive readers use the table.
CREATE VIEW IF NOT EXISTS book_browse_text_source AS
SELECT book_row_id,author,normalize_search_text(title || subtitle) AS search_title,
       normalize_search_text(author) AS search_author,normalize_search_text(sort_title) AS sort_title
FROM (
    SELECT book.row_id AS book_row_id,
           COALESCE(book.title,'Untitled (' || book.content_hash || ')') AS title,
           COALESCE(book.title,'') AS sort_title,
           COALESCE(': ' || book.subtitle,'') AS subtitle,
           COALESCE((SELECT group_concat(credited_name, ', ') FROM (
               SELECT name AS credited_name FROM book_contributor
               WHERE book_row_id=book.row_id AND role=X'617574' ORDER BY position
           )), '') AS author
    FROM book
);
CREATE TRIGGER IF NOT EXISTS browse_text_book_insert AFTER INSERT ON book
BEGIN
    INSERT INTO book_browse_text SELECT * FROM book_browse_text_source WHERE book_row_id IN (NEW.row_id)
    ON CONFLICT(book_row_id) DO UPDATE SET author=excluded.author,search_title=excluded.search_title,
        search_author=excluded.search_author,sort_title=excluded.sort_title;
END;
CREATE TRIGGER IF NOT EXISTS browse_text_book_update AFTER UPDATE OF title,subtitle,content_hash ON book
BEGIN
    INSERT INTO book_browse_text SELECT * FROM book_browse_text_source WHERE book_row_id IN (NEW.row_id)
    ON CONFLICT(book_row_id) DO UPDATE SET author=excluded.author,search_title=excluded.search_title,
        search_author=excluded.search_author,sort_title=excluded.sort_title;
END;
CREATE TRIGGER IF NOT EXISTS browse_text_credit_insert AFTER INSERT ON book_contributor
BEGIN
    INSERT INTO book_browse_text SELECT * FROM book_browse_text_source WHERE book_row_id IN (NEW.book_row_id)
    ON CONFLICT(book_row_id) DO UPDATE SET author=excluded.author,search_title=excluded.search_title,
        search_author=excluded.search_author,sort_title=excluded.sort_title;
END;
CREATE TRIGGER IF NOT EXISTS browse_text_credit_delete AFTER DELETE ON book_contributor
BEGIN
    INSERT INTO book_browse_text SELECT * FROM book_browse_text_source WHERE book_row_id IN (OLD.book_row_id)
    ON CONFLICT(book_row_id) DO UPDATE SET author=excluded.author,search_title=excluded.search_title,
        search_author=excluded.search_author,sort_title=excluded.sort_title;
END;
CREATE TRIGGER IF NOT EXISTS browse_text_credit_update AFTER UPDATE OF book_row_id,position,name,role ON book_contributor
BEGIN
    INSERT INTO book_browse_text SELECT * FROM book_browse_text_source WHERE book_row_id IN (OLD.book_row_id,NEW.book_row_id)
    ON CONFLICT(book_row_id) DO UPDATE SET author=excluded.author,search_title=excluded.search_title,
        search_author=excluded.search_author,sort_title=excluded.sort_title;
END;
INSERT INTO book_browse_text SELECT * FROM book_browse_text_source WHERE true
ON CONFLICT(book_row_id) DO UPDATE SET author=excluded.author,search_title=excluded.search_title,
    search_author=excluded.search_author,sort_title=excluded.sort_title;
