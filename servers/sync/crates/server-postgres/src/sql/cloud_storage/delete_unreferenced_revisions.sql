DELETE FROM user_book_revision AS revision WHERE user_id=$1 AND content_hash=ANY($2)
        AND NOT EXISTS(SELECT 1 FROM user_blob_reference WHERE user_id=$1 AND content_hash=revision.content_hash)
        AND NOT EXISTS(SELECT 1 FROM library_blob_claim WHERE user_id=$1 AND content_hash=revision.content_hash) RETURNING blob_hash;
