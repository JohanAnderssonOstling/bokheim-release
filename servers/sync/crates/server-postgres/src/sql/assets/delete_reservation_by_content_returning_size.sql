DELETE FROM book_upload_reservation WHERE user_id=$1 AND content_hash=$2 RETURNING size_bytes
