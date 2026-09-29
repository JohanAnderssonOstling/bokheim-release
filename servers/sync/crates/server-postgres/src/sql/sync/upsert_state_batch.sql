WITH input AS (
    SELECT *
    FROM UNNEST(
        $2::text[], $3::text[], $4::text[], $5::bytea[], $6::boolean[],
        $7::text[], $8::bigint[], $9::bigint[], $10::smallint[], $11::text[], $12::text[]
    ) AS rows(kind, entity_key, entity_subkey, value, present, content_hash, changed_at, replica_seq, version_rank, event_id, replica_id)
), reduced AS (
    SELECT DISTINCT ON (kind, entity_key, entity_subkey) *
    FROM input
    ORDER BY kind, entity_key, entity_subkey, changed_at DESC, version_rank DESC, replica_id COLLATE "C" DESC, replica_seq DESC, event_id COLLATE "C" DESC
)
INSERT INTO {{TABLE}}(library_id,kind,entity_key,entity_subkey,value,present,content_hash,changed_at,replica_id,replica_seq,version_rank,event_id)
SELECT $1,kind,entity_key,entity_subkey,value,present,content_hash,changed_at,replica_id,replica_seq,version_rank,event_id
FROM reduced
ON CONFLICT(library_id,kind,entity_key,entity_subkey) DO UPDATE SET
    value=EXCLUDED.value, present=EXCLUDED.present, content_hash=EXCLUDED.content_hash,
    changed_at=EXCLUDED.changed_at, replica_id=EXCLUDED.replica_id, replica_seq=EXCLUDED.replica_seq,
    version_rank=EXCLUDED.version_rank, event_id=EXCLUDED.event_id, server_seq=DEFAULT;
