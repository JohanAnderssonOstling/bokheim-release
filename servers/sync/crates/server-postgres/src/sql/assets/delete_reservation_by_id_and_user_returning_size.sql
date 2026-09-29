DELETE FROM book_upload_reservation WHERE id=$1 AND user_id=$2 RETURNING size_bytes
