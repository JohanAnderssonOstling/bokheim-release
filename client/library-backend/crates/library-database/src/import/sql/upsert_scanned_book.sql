INSERT INTO book(content_hash,title,subtitle,description_scanned,book_metadata,added_at,format,deleted_at)
VALUES(:content_hash,:title,:subtitle,1,:book_metadata,:added_at,:format,NULL)
ON CONFLICT(content_hash) DO UPDATE SET
    title=excluded.title,
    subtitle=excluded.subtitle,
    format=excluded.format,
    description_scanned=1,
    book_metadata=excluded.book_metadata,
    -- Metadata backfills must not make an existing book look newly added.
    added_at=COALESCE(book.added_at, excluded.added_at),
    deleted_at=NULL;
