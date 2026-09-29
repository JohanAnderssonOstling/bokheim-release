DELETE FROM book_contributor WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND position>=:position;
