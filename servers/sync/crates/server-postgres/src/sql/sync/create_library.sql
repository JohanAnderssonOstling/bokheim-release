WITH changed AS (
    INSERT INTO libraries(id, user_id, name)
    VALUES ($1, $2, $3)
    ON CONFLICT (id) DO NOTHING
    RETURNING id, name
)
SELECT id, name, TRUE AS changed FROM changed
UNION ALL
SELECT id, name, FALSE AS changed
FROM libraries
WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM changed)
LIMIT 1;
