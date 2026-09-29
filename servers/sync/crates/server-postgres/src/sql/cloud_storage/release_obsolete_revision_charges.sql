WITH released AS (DELETE FROM user_blob_charge AS charge USING blob_object AS object
            WHERE charge.user_id=$1 AND charge.content_hash=ANY($2) AND object.content_hash=charge.content_hash
            AND NOT EXISTS(SELECT 1 FROM library_blob_claim WHERE user_id=$1 AND blob_hash=charge.content_hash)
            AND NOT EXISTS(SELECT 1 FROM user_book_revision WHERE user_id=$1 AND blob_hash=charge.content_hash)
            AND NOT EXISTS(SELECT 1 FROM user_blob_reference WHERE user_id=$1 AND content_hash=charge.content_hash
                AND NOT EXISTS(SELECT 1 FROM user_book_revision WHERE user_id=$1 AND content_hash=charge.content_hash))
            RETURNING object.size_bytes)
            UPDATE user_storage_account SET used_bytes=used_bytes-COALESCE((SELECT SUM(size_bytes) FROM released),0) WHERE user_id=$1;
