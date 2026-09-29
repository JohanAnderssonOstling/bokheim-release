-- name: book_routes?
-- param: book_row_id: i64
WITH RECURSIVE routes(route_id,parent_route_id,direct) AS (
    SELECT r.route_id,r.parent_route_id,1 FROM book_unified_concept a
    JOIN book b ON b.row_id=a.book_row_id JOIN curated.unified_concept_route r ON r.concept_id=a.concept_id
    WHERE b.row_id=:book_row_id AND b.deleted_at IS NULL AND b.hidden_at IS NULL
      AND EXISTS(SELECT 1 FROM book_dir p WHERE p.book_row_id=b.row_id AND p.deleted_at IS NULL)
    UNION
    SELECT r.route_id,r.parent_route_id,0 FROM routes child
    JOIN curated.unified_concept_route r ON r.route_id=child.parent_route_id
) SELECT route_id,MAX(direct) FROM routes GROUP BY route_id;

-- name: subject_cache_remove_book!
-- param: book_row_id: i64
DELETE FROM subject_browse_member WHERE book_row_id=:book_row_id;

-- name: subject_cache_clean&
DELETE FROM subject_browse_dirty_book;
UPDATE subject_browse_state SET built_revision=tree_revision WHERE id=1;

-- name: subject_cache_rebuild&
INSERT OR IGNORE INTO subject_browse_dirty_book SELECT row_id FROM book;
DELETE FROM library_subject;
DELETE FROM subject_browse_member;
DELETE FROM subject_browse_visible_route;
DELETE FROM subject_browse_placement;

-- name: subject_cache_insert_route!
-- param: route_id: i64
INSERT OR IGNORE INTO library_subject(route_id) VALUES(:route_id);

-- name: subject_cache_insert_member!
-- param: route_id: i64
-- param: book_row_id: i64
-- param: direct: i64
INSERT INTO subject_browse_member VALUES(:route_id,:book_row_id,:direct);

-- name: subject_cache_dirty_books?
SELECT book_row_id FROM subject_browse_dirty_book;

-- name: subject_cache_status?
-- param: taxonomy_revision: &str
SELECT built_revision<0 OR taxonomy_revision!=:taxonomy_revision
    OR EXISTS(SELECT 1 FROM subject_browse_member m LEFT JOIN library_subject r ON r.route_id=m.route_id WHERE r.route_id IS NULL)
    OR (built_revision!=tree_revision AND NOT EXISTS(SELECT 1 FROM subject_browse_dirty_book)),
    EXISTS(SELECT 1 FROM subject_browse_dirty_book)
FROM subject_browse_state WHERE id=1;

-- name: subject_cache_prune &
DELETE FROM library_subject WHERE NOT EXISTS(SELECT 1 FROM subject_browse_member m WHERE m.route_id=library_subject.route_id);

-- name: subject_cache_revision!
-- param: taxonomy_revision: &str
UPDATE subject_browse_state SET taxonomy_revision=:taxonomy_revision WHERE id=1;
