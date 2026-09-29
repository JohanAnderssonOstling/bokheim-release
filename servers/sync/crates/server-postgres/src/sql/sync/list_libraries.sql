SELECT id, name
FROM libraries
WHERE user_id = $1 AND deleted_at IS NULL
ORDER BY lower(name), id;
