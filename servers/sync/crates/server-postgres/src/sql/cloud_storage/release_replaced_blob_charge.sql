DELETE FROM user_blob_charge AS charge USING blob_object AS object
            WHERE charge.user_id=$1 AND charge.content_hash=$2 AND object.content_hash=charge.content_hash
            AND NOT EXISTS(SELECT 1 FROM library_blob_claim WHERE user_id=$1 AND blob_hash=$2)
            AND NOT EXISTS(SELECT 1 FROM user_book_revision WHERE user_id=$1 AND blob_hash=$2)
            AND NOT EXISTS(SELECT 1 FROM user_blob_reference AS reference WHERE reference.user_id=$1 AND reference.content_hash=$2
                AND NOT EXISTS(SELECT 1 FROM user_book_revision WHERE user_id=$1 AND content_hash=$2))
            RETURNING object.size_bytes;
