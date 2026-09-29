SELECT content_hash FROM sync_state WHERE library_id=$1 AND kind='book_lifecycle' AND present AND content_hash IS NOT NULL;
