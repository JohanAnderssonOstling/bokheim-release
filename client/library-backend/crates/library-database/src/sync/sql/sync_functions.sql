-- name: function_contributors?
-- param: book_row_id: i64
SELECT author_identity_id, name, role FROM book_contributor WHERE book_row_id = :book_row_id ORDER BY position;

-- name: function_author_stable_id?
-- param: author_identity_id: i64
SELECT stable_id FROM author_identity WHERE id = :author_identity_id;
