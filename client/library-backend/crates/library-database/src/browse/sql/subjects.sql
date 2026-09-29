-- name: get_subject_facets?
-- param: route_id: i64
-- param: normalized_query: &str
-- param: file_types: i32
-- param: languages: &str
-- param: downloaded_only: i32
WITH RECURSIVE descendant_routes(route_id,concept_id) AS (
    SELECT route_id,concept_id FROM curated.unified_concept_route WHERE parent_route_id=:route_id
    UNION ALL
    SELECT child.route_id,child.concept_id FROM descendant_routes parent
    JOIN curated.unified_concept_route child ON child.parent_route_id=parent.route_id
), scope AS NOT MATERIALIZED (
    SELECT member.book_row_id,format.format_category
    FROM subject_browse_member member
    JOIN book_browse_format format ON format.book_row_id=member.book_row_id
    WHERE member.route_id=:route_id
      AND (:downloaded_only=0 OR EXISTS(SELECT 1 FROM book_dir placement
           WHERE placement.book_row_id=member.book_row_id AND placement.deleted_at IS NULL AND placement.is_downloaded=1))
), search_matches AS MATERIALIZED (
    SELECT scope.book_row_id
    FROM scope
    JOIN book_browse_text text ON text.book_row_id=scope.book_row_id
    WHERE :normalized_query!=''
      AND (instr(text.search_title,:normalized_query)>0
           OR instr(text.search_author,:normalized_query)>0
           OR EXISTS(
               SELECT 1 FROM subject_browse_member candidate_member
               JOIN descendant_routes candidate ON candidate.route_id=candidate_member.route_id
               JOIN curated.concept concept ON concept.concept_id=candidate.concept_id
               WHERE candidate_member.book_row_id=scope.book_row_id
                 AND instr(normalize_search_text(COALESCE(concept.preferred_label,'')),:normalized_query)>0
           ))
), eligible AS NOT MATERIALIZED (
    SELECT * FROM scope
    WHERE :normalized_query='' OR book_row_id IN (SELECT book_row_id FROM search_matches)
)

SELECT 0 AS kind,format_category AS value,COUNT(*) AS book_count
FROM eligible
WHERE :languages='' OR EXISTS(SELECT 1 FROM book_browse_language language
      WHERE language.book_row_id=eligible.book_row_id AND instr(:languages,',' || language.base_language || ',')>0)
GROUP BY format_category
UNION ALL
SELECT 1,language.base_language,COUNT(*)
FROM eligible JOIN book_browse_language language ON language.book_row_id=eligible.book_row_id
WHERE :file_types=0 OR (:file_types & eligible.format_category)!=0
GROUP BY language.base_language
ORDER BY kind,value;

-- name: subject_matching_books?
-- param: route_id: i64
-- param: downloaded_only: i32
-- param: file_types: i32
-- param: languages: &str
-- param: normalized_query: &str
SELECT card.book_row_id, card.downloaded,
       instr(card.search_title,:normalized_query)>0 OR instr(card.search_author,:normalized_query)>0,
       card.progress
FROM subject_browse_member member
JOIN book_cards card ON card.book_row_id=member.book_row_id
WHERE member.route_id=:route_id
  AND (:downloaded_only=0 OR card.downloaded)
  AND (:file_types=0 OR (:file_types & card.format_category)!=0)
  AND (:languages='' OR EXISTS(SELECT 1 FROM book_browse_language language
       WHERE language.book_row_id=card.book_row_id AND instr(:languages,',' || language.base_language || ',')>0));

-- name: subject_displayed_cards?
-- param: book_ids: &str
-- param: book_sort: i32
SELECT card.*, NULL AS source_directory
FROM json_each(:book_ids) selected
JOIN book_cards card ON card.book_row_id=selected.value
ORDER BY CASE WHEN :book_sort=1 THEN (
    SELECT MAX(reading.changed_at) FROM sync_state_version reading
    WHERE reading.state_kind='reading_position' AND reading.state_key=card.content_hash
) END DESC,card.sort_title,card.title,card.content_hash;

-- name: get_descendant_subject_content_hashes?
-- param: route_id: i64
SELECT book.content_hash FROM subject_browse_member member
JOIN book ON book.row_id=member.book_row_id
WHERE member.route_id=:route_id
ORDER BY book.content_hash;

-- name: subject_cached_members?
-- param: route_id: i64
WITH RECURSIVE routes(route_id,parent_route_id,label) AS (
    SELECT 0,0,'Subject' WHERE :route_id=0
    UNION ALL
    SELECT route.route_id,visible.parent_route_id,concept.preferred_label
    FROM subject_browse_visible_route visible
    JOIN curated.unified_concept_route route USING(route_id)
    JOIN curated.concept concept USING(concept_id)
    WHERE route.route_id=:route_id
    UNION ALL
    SELECT child.route_id,child.parent_route_id,concept.preferred_label
    FROM routes parent JOIN subject_browse_visible_route child ON child.parent_route_id=parent.route_id
    JOIN curated.unified_concept_route route ON route.route_id=child.route_id
    JOIN curated.concept concept ON concept.concept_id=route.concept_id
)
SELECT route.route_id,route.parent_route_id,route.label,member.book_row_id,
       EXISTS(SELECT 1 FROM subject_browse_placement placement
              WHERE placement.route_id=route.route_id AND placement.book_row_id=member.book_row_id)
FROM routes route JOIN subject_browse_member member ON member.route_id=route.route_id;
