-- name: schema_marks &
-- Annotations.
create table if not exists annotation(
    id              TEXT primary key,
    book_row_id         INTEGER not null references book(row_id) on delete cascade,
    toc_ordinal     INTEGER,
    progress        REAL,
    detail          BLOB not null,
    modified_at     INTEGER not null,
    deleted_at      INTEGER,
    check(toc_ordinal IS NULL OR toc_ordinal >= 0)
);

create index if not exists idx_annotation_book on annotation(book_row_id, deleted_at, toc_ordinal, progress);
