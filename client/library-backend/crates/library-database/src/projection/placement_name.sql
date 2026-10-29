-- Read the retained filename and its live conflict with one prepared query.
WITH requested(file_name) AS MATERIALIZED (
    SELECT COALESCE((SELECT file_name FROM book_dir
                     WHERE dir_id=?1 AND book_row_id=(SELECT row_id FROM book WHERE content_hash=?2)),
                    'Untitled')
)
SELECT file_name,
       EXISTS(SELECT 1 FROM book_dir
              WHERE dir_id=?1 AND deleted_at IS NULL
                AND portable_name_key(file_name)=portable_name_key(requested.file_name)
                AND book_row_id!=(SELECT row_id FROM book WHERE content_hash=?2))
FROM requested;
