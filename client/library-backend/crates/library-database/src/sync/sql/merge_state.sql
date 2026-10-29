-- Inspect admission and publication state together under the caller's writer.
-- Reading body presence avoids loading a value merely to reject its echo.
SELECT v.changed_at,v.conflict_rank,v.replica_id,v.replica_seq,
       v.book_key,v.body IS NOT NULL,
       EXISTS(SELECT 1 FROM sync_outbox q
              WHERE q.state_kind=?1 AND q.state_key=?2 AND q.state_subkey=?3),
       (?1='book_lifecycle' OR ?4 IS NULL
        OR EXISTS(SELECT 1 FROM book WHERE content_hash=?4))
FROM (SELECT 1) input
LEFT JOIN sync_state_version v
  ON v.state_kind=?1 AND v.state_key=?2 AND v.state_subkey=?3;
