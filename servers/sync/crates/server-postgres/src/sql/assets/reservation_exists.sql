SELECT EXISTS(SELECT 1 FROM book_upload_reservation WHERE user_id=$1 AND content_hash=$2)
