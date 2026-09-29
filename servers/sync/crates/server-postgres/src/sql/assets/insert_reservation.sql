INSERT INTO book_upload_reservation(id, user_id, library_id, content_hash, size_bytes, expires_at)
VALUES ($1, $2, $3, $4, $5, now()+make_interval(secs => $6))
