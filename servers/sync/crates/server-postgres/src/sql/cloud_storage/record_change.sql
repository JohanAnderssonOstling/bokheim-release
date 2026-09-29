INSERT INTO cloud_storage_change(library_id,change_id) VALUES($1,$2) ON CONFLICT DO NOTHING RETURNING change_id;
