-- Replace one book's local source subjects, taxonomy assignments and evidence.

-- name: subject_projection_clear_subjects !
-- param: book_id: i64
DELETE FROM book_subject WHERE book_row_id=:book_id;

-- name: subject_projection_clear_concepts !
-- param: book_id: i64
DELETE FROM book_unified_concept WHERE book_row_id=:book_id;

-- name: subject_projection_insert_subject !
-- param: book_id: i64
-- param: position: i64
-- param: name: &str
-- param: source: &str
-- param: authority: Option<&str>
-- param: code: Option<&str>
INSERT INTO book_subject(book_row_id,position,name,source,authority,code)
VALUES(:book_id,:position,:name,:source,:authority,:code);

-- name: subject_projection_insert_concept !
-- param: book_id: i64
-- param: concept_id: i64
-- param: mapper_version: i64
INSERT INTO book_unified_concept(book_row_id,concept_id,mapper_version)
VALUES(:book_id,:concept_id,:mapper_version)
ON CONFLICT(book_row_id,concept_id) DO NOTHING;

-- name: subject_projection_insert_evidence !
-- param: book_id: i64
-- param: concept_id: i64
-- param: subject_position: i64
-- param: source_system: &str
-- param: source_code: &str
INSERT INTO book_unified_concept_evidence(book_row_id,concept_id,subject_position,source_system_id,source_code)
VALUES(:book_id,:concept_id,:subject_position,:source_system,:source_code)
ON CONFLICT(book_row_id,concept_id,subject_position,source_system_id,source_code) DO NOTHING;

-- name: subject_projection_retained_manual_concepts ?
-- param: book_id: i64
-- param: subject_position: i64
-- param: source_code: &str
SELECT DISTINCT e.concept_id
FROM book_unified_concept_evidence e
WHERE e.book_row_id=:book_id AND e.subject_position=:subject_position
  AND e.source_system_id='unified' AND e.source_code=:source_code
  AND EXISTS(SELECT 1 FROM curated.unified_concept_route r WHERE r.concept_id=e.concept_id);
