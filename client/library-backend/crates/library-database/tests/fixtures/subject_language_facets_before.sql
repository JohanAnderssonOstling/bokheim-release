-- Reference query before the membership-based simplification.
-- name: get_subject_language_facets?
-- param: route_id: i64
-- param: normalized_query: &str
-- param: file_types: i32
-- param: downloaded_only: i32
-- Reconstruct the removed ownership cache independently from assignments.
WITH RECURSIVE assigned_routes(route_id,book_row_id,parent_route_id) AS (
    SELECT route.route_id,assignment.book_row_id,route.parent_route_id
    FROM book_unified_concept assignment JOIN curated.unified_concept_route route USING(concept_id)
    UNION
    SELECT parent.route_id,child.book_row_id,parent.parent_route_id
    FROM assigned_routes child JOIN curated.unified_concept_route parent ON parent.route_id=child.parent_route_id
), subject_book_ownership(route_id,book_row_id) AS (
    SELECT route_id,book_row_id FROM assigned_routes
    UNION SELECT 0,book_row_id FROM assigned_routes
), descendant_routes(route_id,concept_id) AS (
    SELECT route_id,concept_id FROM curated.unified_concept_route WHERE parent_route_id=:route_id
    UNION ALL
    SELECT child.route_id,child.concept_id FROM descendant_routes parent
    JOIN curated.unified_concept_route child ON child.parent_route_id=parent.route_id
)
SELECT language.base_language,COUNT(DISTINCT ownership.book_row_id)
FROM subject_book_ownership AS ownership
JOIN book_cards AS card ON card.book_row_id=ownership.book_row_id
JOIN book_browse_format AS format ON format.book_row_id=ownership.book_row_id
JOIN book_browse_language AS language ON language.book_row_id=ownership.book_row_id
WHERE ownership.route_id=:route_id
  AND (:downloaded_only=0 OR card.downloaded)
  AND (:file_types=0 OR (:file_types & format.format_category)!=0)
  AND (
      :normalized_query=''
      OR instr(card.search_title,:normalized_query)>0
      OR instr(card.search_author,:normalized_query)>0
      OR EXISTS(
          SELECT 1
          FROM subject_book_ownership AS candidate_ownership
          JOIN descendant_routes AS candidate ON candidate.route_id=candidate_ownership.route_id
          JOIN curated.concept AS concept ON concept.concept_id=candidate.concept_id
          WHERE candidate_ownership.book_row_id=ownership.book_row_id
            AND instr(normalize_search_text(COALESCE(concept.preferred_label,'')),:normalized_query)>0
      )
  )
GROUP BY language.base_language ORDER BY language.base_language;

