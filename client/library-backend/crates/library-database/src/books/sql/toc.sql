-- Table-of-contents reads.

-- name: book_toc_select?
-- param: content_hash: &str
SELECT toc_json FROM book_toc WHERE content_hash=:content_hash;

-- name: reading_entry_select?
-- param: content_hash: &str
SELECT target FROM book_reading_entry WHERE content_hash=:content_hash;
