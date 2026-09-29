WITH input AS MATERIALIZED (
    SELECT * FROM UNNEST($2::TEXT[], $3::BIGINT[]) AS value(content_hash, delta)
),
valid AS MATERIALIZED (
    SELECT COALESCE(BOOL_AND(input.delta > 0 OR COALESCE(reference.reference_count, 0) >= -input.delta), TRUE) AS ok
    FROM input
    LEFT JOIN user_blob_reference AS reference
      ON reference.user_id=$1 AND reference.content_hash=input.content_hash
),
positive AS (
    INSERT INTO user_blob_reference(user_id, content_hash, reference_count)
    SELECT $1, content_hash, delta FROM input
    WHERE delta > 0 AND (SELECT ok FROM valid)
    ON CONFLICT (user_id, content_hash)
    DO UPDATE SET reference_count=user_blob_reference.reference_count+EXCLUDED.reference_count
    RETURNING content_hash
),
reduced AS (
    UPDATE user_blob_reference AS reference
    SET reference_count=reference.reference_count+input.delta
    FROM input
    WHERE reference.user_id=$1
      AND reference.content_hash=input.content_hash
      AND input.delta < 0
      AND reference.reference_count > -input.delta
      AND (SELECT ok FROM valid)
    RETURNING reference.content_hash
),
removed AS (
    DELETE FROM user_blob_reference AS reference
    USING input
    WHERE reference.user_id=$1
      AND reference.content_hash=input.content_hash
      AND input.delta < 0
      AND reference.reference_count = -input.delta
      AND (SELECT ok FROM valid)
    RETURNING reference.content_hash
),
released AS MATERIALIZED (
    DELETE FROM user_blob_charge AS charge
    USING blob_object AS object, removed
    WHERE charge.user_id=$1
      AND charge.content_hash=removed.content_hash
      AND object.content_hash=charge.content_hash
      AND NOT EXISTS(SELECT 1 FROM library_blob_claim WHERE user_id=$1 AND blob_hash=charge.content_hash)
      AND NOT EXISTS(SELECT 1 FROM user_book_revision AS revision WHERE revision.user_id=$1 AND revision.blob_hash=charge.content_hash)
    RETURNING object.size_bytes
),
account AS (
    UPDATE user_storage_account
    SET used_bytes=GREATEST(used_bytes-COALESCE((SELECT SUM(size_bytes) FROM released), 0), 0)
    WHERE user_id=$1 AND (SELECT ok FROM valid)
    RETURNING user_id
)
SELECT (SELECT ok FROM valid) AS valid,
       COALESCE((SELECT SUM(size_bytes) FROM released), 0)::BIGINT AS released,
       (SELECT COUNT(*) FROM positive)+(SELECT COUNT(*) FROM reduced)+
       (SELECT COUNT(*) FROM removed)+(SELECT COUNT(*) FROM account) AS applied;
