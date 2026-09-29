-- Typed local annotation persistence. Sync mutations are emitted by triggers.

-- name: get_annotations?
-- param: content_hash: &str
SELECT annotation.id, book.content_hash, anchor_kind, anchor_cfi, anchor_page, anchor_rects,
       anchor_fallback_cfi, exact_text, style, color, note,
       created_at, modified_at
FROM annotation JOIN book ON book.row_id=annotation.book_row_id
WHERE book.content_hash=:content_hash AND annotation.deleted_at IS NULL
ORDER BY modified_at DESC, annotation.id;

-- name: get_annotation_by_id?
-- param: id: &str
SELECT annotation.id, book.content_hash, anchor_kind, anchor_cfi, anchor_page, anchor_rects,
       anchor_fallback_cfi, exact_text, style, color, note,
       created_at, modified_at
FROM annotation JOIN book ON book.row_id=annotation.book_row_id
WHERE annotation.id=:id;

-- name: upsert_annotation!
-- param: id: &str
-- param: content_hash: &str
-- param: anchor_kind: &str
-- param: anchor_cfi: Option<&str>
-- param: anchor_page: Option<i64>
-- param: anchor_rects: Option<&[u8]>
-- param: anchor_fallback_cfi: Option<&str>
-- param: exact_text: &str
-- param: style: &str
-- param: color: &str
-- param: note: &str
-- param: created_at: i64
-- param: modified_at: i64
-- param: deleted_at: Option<i64>
INSERT INTO annotation(
    id,book_row_id,anchor_kind,anchor_cfi,anchor_page,anchor_rects,
    anchor_fallback_cfi,exact_text,style,color,note,
    created_at,modified_at,deleted_at
)
VALUES(
    :id,(SELECT row_id FROM book WHERE content_hash=:content_hash),:anchor_kind,:anchor_cfi,:anchor_page,:anchor_rects,
    :anchor_fallback_cfi,:exact_text,:style,:color,:note,
    :created_at,:modified_at,:deleted_at
)
ON CONFLICT(id) DO UPDATE SET
    book_row_id=excluded.book_row_id,
    anchor_kind=excluded.anchor_kind,
    anchor_cfi=excluded.anchor_cfi,
    anchor_page=excluded.anchor_page,
    anchor_rects=excluded.anchor_rects,
    anchor_fallback_cfi=excluded.anchor_fallback_cfi,
    exact_text=excluded.exact_text,
    style=excluded.style,
    color=excluded.color,
    note=excluded.note,
    created_at=excluded.created_at,
    modified_at=excluded.modified_at,
    deleted_at=excluded.deleted_at;

-- name: delete_annotation!
-- param: id: &str
-- param: modified_at: i64
UPDATE annotation
SET modified_at=:modified_at, deleted_at=:modified_at
WHERE id=:id;

-- name: schema_marks &
-- Annotations.
create table if not exists annotation(
    id              TEXT primary key,
    book_row_id         INTEGER not null references book(row_id) on delete cascade,
    anchor_kind     TEXT not null check(anchor_kind in ('epub_cfi', 'pdf')),
    anchor_cfi      TEXT,
    anchor_page     INTEGER,
    anchor_rects    BLOB,
    anchor_fallback_cfi TEXT,
    exact_text      TEXT not null,
    style           TEXT not null check(style in ('highlight', 'underline', 'squiggly', 'strikethrough')),
    color           TEXT not null,
    note            TEXT not null default '',
    created_at      INTEGER not null,
    modified_at     INTEGER not null,
    deleted_at      INTEGER,
    check(
        (anchor_kind='epub_cfi' AND anchor_cfi IS NOT NULL AND anchor_page IS NULL AND anchor_rects IS NULL AND anchor_fallback_cfi IS NULL)
        OR
        (anchor_kind='pdf' AND anchor_cfi IS NULL AND anchor_page IS NOT NULL AND anchor_page >= 0 AND anchor_rects IS NOT NULL)
    )
);

create index if not exists idx_annotation_book on annotation(book_row_id, deleted_at, modified_at);
