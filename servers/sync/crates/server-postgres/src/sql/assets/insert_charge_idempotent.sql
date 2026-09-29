INSERT INTO user_blob_charge(user_id, content_hash) VALUES ($1, $2)
ON CONFLICT (user_id, content_hash) DO NOTHING
