-- Sync cursor, inventory, and recovery primitives.
--
-- Each entry is one bounded read or one bounded write. Page- and
-- chunk-sized looping belongs to the orchestrator, never to storage.

-- name: sync_state_inventory_page?
-- param: kind: &str
-- param: key: &str
-- param: subkey: &str
SELECT state_kind,state_key,state_subkey FROM sync_state_version WHERE (state_kind,state_key,state_subkey)>(:kind,:key,:subkey) ORDER BY state_kind,state_key,state_subkey LIMIT 256;

-- name: sync_current_directory_name?
-- param: key: &str
SELECT bokheim_mutation('directory_name',id,COALESCE(intent_name,name)),0 FROM dir WHERE id=:key;

-- name: sync_current_directory_parent?
-- param: key: &str
SELECT bokheim_mutation('directory_parent',id,COALESCE(intent_parent_id,parent_id)),0 FROM dir WHERE id=:key;

-- name: sync_current_directory_lifecycle?
-- param: key: &str
SELECT bokheim_mutation('directory_lifecycle',id,COALESCE(intent_lifecycle,CASE WHEN purged_at IS NOT NULL THEN 2 WHEN deleted_at IS NOT NULL THEN 1 ELSE 0 END)),COALESCE(intent_lifecycle,CASE WHEN purged_at IS NOT NULL THEN 2 WHEN deleted_at IS NOT NULL THEN 1 ELSE 0 END) FROM dir WHERE id=:key;

-- name: sync_cursor_recovery_pending ->
SELECT cursor_recovery_pending FROM sync_metadata WHERE singleton = 1;

-- name: sync_begin_cursor_recovery!
UPDATE sync_metadata SET last_pull_state_seq = NULL, last_pull_reading_seq = NULL, cursor_recovery_pending = 1 WHERE singleton = 1;

-- name: sync_clear_cursor_recovery!
UPDATE sync_metadata SET cursor_recovery_pending = NULL WHERE singleton = 1;

-- name: sync_pull_cursor?
SELECT CAST(last_pull_state_seq AS TEXT), CAST(last_pull_reading_seq AS TEXT) FROM sync_metadata WHERE singleton = 1;

-- name: sync_inventory_checkpoint?
SELECT inventory_repair_kind, inventory_repair_key, inventory_repair_subkey FROM sync_metadata WHERE singleton = 1;

-- name: sync_save_inventory_checkpoint!
-- param: kind: &str
-- param: key: &str
-- param: subkey: &str
INSERT INTO sync_metadata(singleton, inventory_repair_kind, inventory_repair_key, inventory_repair_subkey) VALUES (1, :kind, :key, :subkey)
            ON CONFLICT(singleton) DO UPDATE SET inventory_repair_kind = excluded.inventory_repair_kind, inventory_repair_key = excluded.inventory_repair_key, inventory_repair_subkey = excluded.inventory_repair_subkey;

-- name: sync_clear_inventory_checkpoint!
UPDATE sync_metadata SET inventory_repair_kind = NULL, inventory_repair_key = NULL, inventory_repair_subkey = NULL WHERE singleton = 1;

-- name: cursor_state_upsert!
-- param: revision: &str
INSERT INTO sync_metadata(singleton, last_pull_state_seq) VALUES (1, :revision) ON CONFLICT(singleton) DO UPDATE SET last_pull_state_seq = excluded.last_pull_state_seq;

-- name: cursor_state_clear!
UPDATE sync_metadata SET last_pull_state_seq = NULL WHERE singleton = 1;

-- name: cursor_reading_upsert!
-- param: revision: &str
INSERT INTO sync_metadata(singleton, last_pull_reading_seq) VALUES (1, :revision) ON CONFLICT(singleton) DO UPDATE SET last_pull_reading_seq = excluded.last_pull_reading_seq;

-- name: cursor_reading_clear!
UPDATE sync_metadata SET last_pull_reading_seq = NULL WHERE singleton = 1;

-- name: syncmeta_cursor_recovery_pending?
SELECT CAST(cursor_recovery_pending AS TEXT) FROM sync_metadata WHERE singleton = 1;

-- name: syncmeta_cloud_storage_enabled?
SELECT CAST(cloud_storage_enabled AS TEXT) FROM sync_metadata WHERE singleton = 1;
