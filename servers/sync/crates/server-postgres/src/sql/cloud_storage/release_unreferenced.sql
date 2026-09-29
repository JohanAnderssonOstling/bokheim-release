WITH released AS (
 DELETE FROM user_blob_charge charge USING blob_object object
 WHERE charge.user_id=$1 AND object.content_hash=charge.content_hash
 AND NOT EXISTS(SELECT 1 FROM library_blob_claim claim WHERE claim.user_id=$1 AND claim.blob_hash=charge.content_hash)
 AND NOT EXISTS(SELECT 1 FROM user_blob_reference ref LEFT JOIN user_book_revision rev ON rev.user_id=$1 AND rev.content_hash=ref.content_hash
                WHERE ref.user_id=$1 AND COALESCE(rev.blob_hash,ref.content_hash)=charge.content_hash)
 RETURNING object.size_bytes
)
UPDATE user_storage_account SET used_bytes=used_bytes-COALESCE((SELECT SUM(size_bytes) FROM released),0) WHERE user_id=$1;
