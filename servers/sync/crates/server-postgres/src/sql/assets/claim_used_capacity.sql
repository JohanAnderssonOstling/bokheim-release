UPDATE user_storage_account
SET used_bytes = used_bytes + $2
WHERE user_id = $1
  AND used_bytes + reserved_bytes + $2 <= quota_bytes
RETURNING quota_bytes, used_bytes, reserved_bytes;
