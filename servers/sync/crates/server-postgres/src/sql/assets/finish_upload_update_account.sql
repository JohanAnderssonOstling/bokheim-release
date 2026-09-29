UPDATE user_storage_account
SET reserved_bytes=reserved_bytes-$2,
    used_bytes=used_bytes+CASE WHEN $3 THEN $2 ELSE 0 END
WHERE user_id=$1
