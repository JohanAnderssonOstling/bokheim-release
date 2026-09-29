-- Historical selection oracle for the Rust presentation regression.
WITH credited AS (
    SELECT DISTINCT
           author_identity.stable_id AS author_id,
           assignment.concept_id AS concept_id,
           book.row_id AS book_row_id
    FROM author_identity
    JOIN author_credit ON author_credit.author_identity_id = author_identity.id
    JOIN book ON book.row_id = author_credit.book_row_id
    JOIN book_unified_concept AS assignment ON assignment.book_row_id = book.row_id
    WHERE book.deleted_at IS NULL
      AND book.hidden_at IS NULL
      AND TRIM(author_credit.credited_name) != ''
      AND EXISTS (
          SELECT 1 FROM book_dir placement
          WHERE placement.book_row_id = book.row_id AND placement.deleted_at IS NULL
      )
      AND (:downloaded_only = 0 OR EXISTS (
          SELECT 1 FROM book_dir available
          WHERE available.book_row_id = book.row_id
            AND available.deleted_at IS NULL
            AND available.is_downloaded = 1
      ))
),
routed AS (
    SELECT credited.author_id AS author_id,
           concept.preferred_label AS label,
           route.parent_route_id AS parent_route_id,
           COUNT(DISTINCT credited.book_row_id) AS book_count
    FROM credited
    JOIN curated.unified_concept_route AS route ON route.concept_id = credited.concept_id
    JOIN curated.concept AS concept ON concept.concept_id=route.concept_id
    WHERE TRIM(COALESCE(concept.preferred_label, '')) != ''
    GROUP BY credited.author_id, concept.preferred_label, route.parent_route_id
),
counted AS (
    SELECT author_id, label, parent_route_id,
           SUM(book_count) OVER (PARTITION BY author_id, label) AS label_count,
           ROW_NUMBER() OVER (PARTITION BY author_id, label ORDER BY book_count DESC, parent_route_id) AS route_position
    FROM routed
)
SELECT author_id, label, parent_label
FROM (
    SELECT counted.author_id AS author_id,
           counted.label AS label,
           COALESCE(parent_concept.preferred_label, '') AS parent_label,
           ROW_NUMBER() OVER (
               PARTITION BY counted.author_id
               ORDER BY counted.label_count DESC, normalize_search_text(counted.label), counted.label
           ) AS position
    FROM counted
    LEFT JOIN curated.unified_concept_route AS parent ON parent.route_id = counted.parent_route_id
    LEFT JOIN curated.concept AS parent_concept ON parent_concept.concept_id=parent.concept_id
    WHERE counted.route_position = 1
)
WHERE position <= :per_author
ORDER BY author_id, position;

