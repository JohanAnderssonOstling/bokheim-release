INSERT INTO library_blob_claim(library_id,user_id,content_hash,blob_hash) VALUES($1,$2,$3,$4) ON CONFLICT(library_id,content_hash) DO UPDATE SET blob_hash=excluded.blob_hash;
