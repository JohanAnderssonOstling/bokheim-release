-- name: count_all_books?
-- Counts only books with a live placement, matching what the carousels display.
SELECT COUNT(*) FROM book
WHERE deleted_at IS NULL
  AND hidden_at IS NULL
  AND EXISTS (SELECT 1 FROM book_dir bd WHERE bd.book_row_id = book.row_id AND bd.deleted_at IS NULL);

-- name: library_format_counts_select?
SELECT format.format_category,COUNT(*)
FROM book_browse_format format
JOIN book ON book.row_id=format.book_row_id
WHERE book.deleted_at IS NULL AND book.hidden_at IS NULL
  AND EXISTS(SELECT 1 FROM book_dir placement
             WHERE placement.book_row_id=book.row_id AND placement.deleted_at IS NULL)
GROUP BY format.format_category
ORDER BY format.format_category;
