WITH candidates AS MATERIALIZED (
    SELECT *, octet_length(value)::bigint + octet_length(kind) + octet_length(entity_key) + octet_length(entity_subkey) + $7::bigint AS row_bytes
    FROM sync_state
    WHERE library_id=$1 AND kind='book_lifecycle' AND server_seq>$2 AND server_seq<=$3
    ORDER BY server_seq LIMIT $4
), sized AS (
    SELECT *, ROW_NUMBER() OVER(ORDER BY server_seq) AS page_row,
           COALESCE(SUM(row_bytes) OVER(ORDER BY server_seq ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING),0)::bigint AS prior_bytes
    FROM candidates
)
SELECT kind, entity_key, entity_subkey,
       CASE WHEN prior_bytes+row_bytes<=$5 OR ($6 AND prior_bytes=0) THEN value END AS value,
       content_hash, changed_at, replica_id, replica_seq, version_rank, server_seq, row_bytes
FROM sized WHERE prior_bytes<=$5 OR ($6 AND page_row<=2)
ORDER BY server_seq;
