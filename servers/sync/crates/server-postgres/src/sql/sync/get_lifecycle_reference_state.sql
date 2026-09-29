SELECT present, content_hash, changed_at, version_rank, replica_id, replica_seq, event_id
FROM sync_state
WHERE library_id = $1
  AND kind = 'book_lifecycle'
  AND entity_key = $2
  AND entity_subkey = $3
