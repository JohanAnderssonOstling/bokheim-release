-- Adopt the deployed v14 baseline with cloud-storage support without rewriting
-- account, library, blob or synchronized state rows. Executed transactionally.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM bokheim_schema_migration WHERE version = '14-baseline')
       OR to_regclass('public.user_book_revision') IS NULL
       OR to_regclass('public.library_blob_claim') IS NULL
       OR to_regclass('public.cloud_storage_change') IS NULL
       OR NOT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid='public.libraries'::regclass
                      AND attname='cloud_storage_enabled' AND NOT attisdropped) THEN
        RAISE EXCEPTION 'Database does not match the supported pre-release baseline';
    END IF;
END $$;

-- Revision-only conflict repair must not be discarded as an unchanged value.
DROP TRIGGER lww_guard ON sync_state;
CREATE TRIGGER lww_guard BEFORE UPDATE OF library_id, kind, entity_key, entity_subkey, value, present, content_hash, changed_at, replica_id, replica_seq, version_rank, event_id ON sync_state FOR EACH ROW EXECUTE FUNCTION enforce_lww_order();
DROP TRIGGER lww_guard ON sync_reading_state;
CREATE TRIGGER lww_guard BEFORE UPDATE OF library_id, kind, entity_key, entity_subkey, value, present, content_hash, changed_at, replica_id, replica_seq, version_rank, event_id ON sync_reading_state FOR EACH ROW EXECUTE FUNCTION enforce_lww_order();

CREATE TABLE bokheim_schema_version (version integer PRIMARY KEY CHECK (version = 1));
INSERT INTO bokheim_schema_version(version) VALUES (1);
