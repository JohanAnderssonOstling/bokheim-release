WITH expired_books AS (
    DELETE FROM book_upload_reservation
    WHERE user_id=$1 AND expires_at <= now()
    RETURNING size_bytes
)
SELECT COALESCE(SUM(size_bytes), 0)::BIGINT AS total_released FROM expired_books
