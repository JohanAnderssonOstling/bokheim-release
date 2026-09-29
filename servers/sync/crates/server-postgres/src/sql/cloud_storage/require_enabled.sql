SELECT cloud_storage_enabled FROM libraries WHERE id=$1 AND user_id=$2 AND deleted_at IS NULL FOR SHARE;
