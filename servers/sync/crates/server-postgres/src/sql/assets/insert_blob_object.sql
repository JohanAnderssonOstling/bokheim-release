INSERT INTO blob_object(content_hash, size_bytes) VALUES ($1, $2)
ON CONFLICT (content_hash) DO UPDATE SET size_bytes=blob_object.size_bytes
WHERE blob_object.size_bytes=EXCLUDED.size_bytes
RETURNING content_hash
