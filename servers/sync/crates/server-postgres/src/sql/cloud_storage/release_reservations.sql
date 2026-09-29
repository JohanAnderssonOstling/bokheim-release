WITH books AS (DELETE FROM book_upload_reservation WHERE library_id=$1 AND user_id=$2 RETURNING size_bytes)
UPDATE user_storage_account SET reserved_bytes=reserved_bytes-
COALESCE((SELECT SUM(size_bytes) FROM books),0)
WHERE user_id=$2;
