INSERT INTO library_registry(library_id,library_name,storage_path) VALUES(?1,?2,?3)
ON CONFLICT(library_id) DO UPDATE SET library_name=excluded.library_name,storage_path=excluded.storage_path
