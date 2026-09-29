SELECT id, cloud_storage_enabled FROM libraries WHERE user_id=$1 AND deleted_at IS NULL ORDER BY id;
