UPDATE user_storage_account SET reserved_bytes=reserved_bytes+$2 WHERE user_id=$1
