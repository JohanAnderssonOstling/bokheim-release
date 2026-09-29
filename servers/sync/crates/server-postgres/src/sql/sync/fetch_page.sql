WITH selected AS (
SELECT kind, entity_key, entity_subkey, value, present, content_hash,
       changed_at, replica_id, replica_seq, version_rank, event_id, server_seq
FROM (
    SELECT kind, entity_key, entity_subkey, value, present, content_hash,
           changed_at, replica_id, replica_seq, version_rank, event_id, server_seq
    FROM sync_state
    WHERE library_id = $1 AND server_seq > $2 AND server_seq <= $4
    UNION ALL
    SELECT kind, entity_key, entity_subkey, value, present, content_hash,
           changed_at, replica_id, replica_seq, version_rank, event_id, server_seq
    FROM sync_reading_state
    WHERE library_id = $1 AND server_seq > $3 AND server_seq <= $5
) page
ORDER BY server_seq
LIMIT $6
)
SELECT selected.*,
       creation.kind AS creation_kind,
       creation.entity_key AS creation_entity_key,
       creation.entity_subkey AS creation_entity_subkey,
       creation.value AS creation_value,
       creation.present AS creation_present,
       creation.content_hash AS creation_content_hash,
       creation.changed_at AS creation_changed_at,
       creation.replica_id AS creation_replica_id,
       creation.replica_seq AS creation_replica_seq,
       creation.version_rank AS creation_version_rank,
       creation.event_id AS creation_event_id,
       creation.server_seq AS creation_server_seq
FROM selected
LEFT JOIN sync_state creation ON creation.library_id=$1
    AND creation.kind='book_lifecycle' AND creation.entity_subkey=''
    AND creation.present IS TRUE AND creation.content_hash=creation.entity_key
    AND creation.entity_key=CASE
        WHEN selected.kind='annotation' THEN selected.content_hash
        WHEN selected.kind IN ('book_lifecycle','book_facts','placement','reading_position','metadata','description','pdf_reader_metadata','book_toc') THEN selected.entity_key
    END
ORDER BY selected.server_seq;
