WITH owned AS MATERIALIZED (
    SELECT
        library.id,
        COALESCE(revision.current_revision, 0) AS current_sync_revision,
        COALESCE(revision.reading_revision, 0) AS reading_revision,
        COALESCE(revision.other_revision, 0) AS other_revision
    FROM libraries AS library
    LEFT JOIN sync_library_revision AS revision ON revision.library_id = library.id
    WHERE library.id = $1 AND library.user_id = $2 AND library.deleted_at IS NULL
),
changed AS (
    INSERT INTO sync_reading_state(
        library_id, kind, entity_key, entity_subkey, value, present, content_hash,
        changed_at, replica_id, replica_seq, version_rank, event_id
    )
    SELECT $1, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13
    FROM owned
    WHERE other_revision >= $14 AND reading_revision >= $15
    ON CONFLICT (library_id, kind, entity_key, entity_subkey) DO UPDATE SET
        value = EXCLUDED.value,
        present = EXCLUDED.present,
        content_hash = EXCLUDED.content_hash,
        changed_at = EXCLUDED.changed_at,
        replica_id = EXCLUDED.replica_id,
        replica_seq = EXCLUDED.replica_seq,
        version_rank = EXCLUDED.version_rank,
        event_id = EXCLUDED.event_id,
        server_seq = DEFAULT
    RETURNING server_seq, event_id
)
SELECT
    EXISTS(SELECT 1 FROM owned) AS authorized,
    COALESCE((SELECT other_revision FROM owned) < $14 OR (SELECT reading_revision FROM owned) < $15, FALSE) AS cursor_ahead,
    COALESCE((SELECT other_revision FROM owned) <= $14 AND (SELECT reading_revision FROM owned) <= $15, FALSE) AS caught_up,
    (SELECT reading_revision FROM owned) AS reading_revision,
    (SELECT other_revision FROM owned) AS other_revision,
    changed.server_seq,
    changed.event_id
FROM (VALUES (1)) AS sentinel(value)
LEFT JOIN changed ON TRUE;
