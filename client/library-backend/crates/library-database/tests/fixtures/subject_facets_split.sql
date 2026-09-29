-- Subject facets use the same derived dimensions and cross-filter rules as
-- folder facets; refreshed membership already includes descendant assignments.
-- name: get_subject_language_facets?
-- param: route_id: i64
-- param: normalized_query: &str
-- param: file_types: i32
-- param: downloaded_only: i32
WITH RECURSIVE descendant_routes(route_id,concept_id) AS (
    SELECT route_id,concept_id FROM curated.unified_concept_route WHERE parent_route_id=:route_id
    UNION ALL
    SELECT child.route_id,child.concept_id FROM descendant_routes parent
    JOIN curated.unified_concept_route child ON child.parent_route_id=parent.route_id
)
SELECT language.base_language,COUNT(*)
FROM subject_browse_member AS member
JOIN book_browse_text AS text ON text.book_row_id=member.book_row_id
JOIN book_browse_format AS format ON format.book_row_id=member.book_row_id
JOIN book_browse_language AS language ON language.book_row_id=member.book_row_id
WHERE member.route_id=:route_id
  AND (:downloaded_only=0 OR EXISTS(SELECT 1 FROM book_dir placement
       WHERE placement.book_row_id=member.book_row_id AND placement.deleted_at IS NULL AND placement.is_downloaded=1))
  AND (:file_types=0 OR (:file_types & format.format_category) != 0)
  AND (
      :normalized_query=''
      OR instr(text.search_title,:normalized_query)>0
      OR instr(text.search_author,:normalized_query)>0
      OR EXISTS(
          SELECT 1
          FROM subject_browse_member AS candidate_member
          JOIN descendant_routes AS candidate ON candidate.route_id=candidate_member.route_id
          JOIN curated.concept AS concept ON concept.concept_id=candidate.concept_id
          WHERE candidate_member.book_row_id=member.book_row_id
            AND instr(normalize_search_text(COALESCE(concept.preferred_label,'')),:normalized_query)>0
      )
  )
GROUP BY language.base_language ORDER BY language.base_language;

-- name: get_subject_format_facets?
-- param: route_id: i64
-- param: normalized_query: &str
-- param: languages: &str
-- param: downloaded_only: i32
WITH RECURSIVE descendant_routes(route_id,concept_id) AS (
    SELECT route_id,concept_id FROM curated.unified_concept_route WHERE parent_route_id=:route_id
    UNION ALL
    SELECT child.route_id,child.concept_id FROM descendant_routes parent
    JOIN curated.unified_concept_route child ON child.parent_route_id=parent.route_id
)
-- The refreshed membership cache contains only visible, placed books and is
-- unique per route/book. All outer joins are one-to-one, so COUNT(*) is enough.
SELECT format.format_category,COUNT(*)
FROM subject_browse_member AS member
JOIN book_browse_text AS text ON text.book_row_id=member.book_row_id
JOIN book_browse_format AS format ON format.book_row_id=member.book_row_id
WHERE member.route_id=:route_id
  AND (:downloaded_only=0 OR EXISTS(
      SELECT 1 FROM book_dir placement WHERE placement.book_row_id=member.book_row_id
        AND placement.deleted_at IS NULL AND placement.is_downloaded=1))
  AND (:languages='' OR EXISTS(SELECT 1 FROM book_browse_language AS language WHERE language.book_row_id=member.book_row_id AND instr(:languages,',' || language.base_language || ',')>0))
  AND (
      :normalized_query=''
      OR instr(text.search_title,:normalized_query)>0
      OR instr(text.search_author,:normalized_query)>0
      OR EXISTS(
          SELECT 1
          FROM subject_browse_member AS candidate_member
          JOIN descendant_routes AS candidate ON candidate.route_id=candidate_member.route_id
          JOIN curated.concept AS concept ON concept.concept_id=candidate.concept_id
          WHERE candidate_member.book_row_id=member.book_row_id
            AND instr(normalize_search_text(COALESCE(concept.preferred_label,'')),:normalized_query)>0
      )
  )
GROUP BY format.format_category ORDER BY format.format_category;

