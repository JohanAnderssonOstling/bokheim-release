-- Write ordered contributor credits and the browse author projection.

-- name: contributors_insert_name_key !
-- param: name_key: &str
-- param: author_identity_id: i64
INSERT OR IGNORE INTO author_name_key(name_key,author_identity_id) VALUES(:name_key,:author_identity_id);

-- name: contributors_identity_by_name_key ?
-- param: name_key: &str
SELECT identity.id,identity.stable_id
FROM author_name_key keys JOIN author_identity identity ON identity.id=keys.author_identity_id
WHERE keys.name_key=:name_key AND identity.is_provisional=1
ORDER BY keys.author_identity_id LIMIT 1;

-- name: contributors_existing_names ?
SELECT id,name FROM author_identity;

-- name: contributors_upsert_credit !
-- param: content_hash: &SqlParam<'_>
-- param: author_identity_id: &SqlParam<'_>
-- param: position: &SqlParam<'_>
-- param: name: &SqlParam<'_>
-- param: role: &SqlParam<'_>
INSERT INTO book_contributor(book_row_id, author_identity_id, position, name, role)
SELECT (SELECT row_id FROM book WHERE content_hash=:content_hash), :author_identity_id, :position, :name, :role
WHERE NOT EXISTS(SELECT 1 FROM book_contributor WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash)
 AND position=:position AND author_identity_id=:author_identity_id AND name=:name AND role=:role)
ON CONFLICT(book_row_id, position) DO UPDATE SET
    author_identity_id=excluded.author_identity_id,
    name=excluded.name,
    role=excluded.role
WHERE book_contributor.author_identity_id IS NOT excluded.author_identity_id
   OR book_contributor.name IS NOT excluded.name
   OR book_contributor.role IS NOT excluded.role;

-- name: contributors_trim_credits !
-- param: content_hash: &SqlParam<'_>
-- param: position: &SqlParam<'_>
DELETE FROM book_contributor WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND position>=:position;

-- name: contributors_set_author !
-- param: content_hash: &SqlParam<'_>
-- param: author: &SqlParam<'_>
-- param: search_author: &SqlParam<'_>
UPDATE book SET author=:author,search_author=:search_author WHERE content_hash=:content_hash;
