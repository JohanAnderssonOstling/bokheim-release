WITH input AS MATERIALIZED (
    SELECT content_hash, size_bytes
    FROM UNNEST($2::TEXT[], $3::BIGINT[]) AS value(content_hash, size_bytes)
),
claimed AS (
    INSERT INTO user_blob_charge(user_id, content_hash)
    SELECT $1, content_hash FROM input
    ON CONFLICT (user_id, content_hash) DO NOTHING
    RETURNING content_hash
),
claimed_bytes AS MATERIALIZED (
    SELECT COALESCE(SUM(input.size_bytes), 0)::BIGINT AS total
    FROM input JOIN claimed USING (content_hash)
),
account AS (
    UPDATE user_storage_account
    SET used_bytes = used_bytes + (SELECT total FROM claimed_bytes)
    WHERE user_id = $1
      AND used_bytes + reserved_bytes + (SELECT total FROM claimed_bytes) <= quota_bytes
    RETURNING user_id
)
SELECT (SELECT COUNT(*) FROM claimed)::BIGINT AS claimed_count,
       (SELECT total FROM claimed_bytes) AS claimed_bytes,
       EXISTS(SELECT 1 FROM account) AS accepted;
