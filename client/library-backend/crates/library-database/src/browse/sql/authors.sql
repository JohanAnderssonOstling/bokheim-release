-- name: get_authors?
-- param: downloaded_only: i32
-- Authors file under their display name.
SELECT
    author_identity.stable_id AS author_id,
    MIN(author_credit.credited_name) AS display_name,
    COUNT(DISTINCT book.row_id) AS book_count
FROM author_identity
JOIN author_credit ON author_credit.author_identity_id = author_identity.id
JOIN book ON book.row_id = author_credit.book_row_id
JOIN book_dir ON book_dir.book_row_id = book.row_id
WHERE book.deleted_at IS NULL
  AND book.hidden_at IS NULL
  AND book_dir.deleted_at IS NULL
  AND TRIM(author_credit.credited_name) != ''
  AND (:downloaded_only = 0 OR EXISTS (
      SELECT 1 FROM book_dir available
      WHERE available.book_row_id = book.row_id
        AND available.deleted_at IS NULL
        AND available.is_downloaded = 1
  ))
GROUP BY author_identity.id, author_identity.stable_id
ORDER BY LOWER(display_name);

-- name: get_author_book_cards?
-- param: downloaded_only: i32
-- param: limit: i32
-- The books an author's shelf carries, most recently read first, then by title,
-- capped per author. Whole cards rather than cover hashes: the shelf is the
-- same carousel Home draws, so it needs everything a book card renders. One
-- statement for the whole index rather than a correlated lookup per row, so the
-- cost does not scale with the list.
WITH credited AS (
    -- DISTINCT, not GROUP BY: one book credited twice to the same identity —
    -- two spellings that resolved together — is still one book, and every
    -- column here comes from that one card row.
    SELECT DISTINCT
           author_identity.stable_id AS author_id,
           card.book_row_id AS book_row_id,
           card.content_hash, card.title, card.author, card.description, card.progress,
           card.downloaded, card.download_requested, card.format_category,
           card.audiobook_duration_ms, card.audiobook_chapter_count,
           (SELECT reading.changed_at FROM sync_state_version reading
             WHERE reading.state_kind = 'reading_position'
               AND reading.state_key = card.content_hash) AS read_at, card.added_at, card.subtitle
    FROM author_identity
    JOIN author_credit ON author_credit.author_identity_id = author_identity.id
    JOIN book_cards card ON card.book_row_id = author_credit.book_row_id
    WHERE (:downloaded_only = 0 OR card.downloaded)
)
SELECT content_hash, title, author, description, progress,
       downloaded, download_requested, format_category,
       audiobook_duration_ms, audiobook_chapter_count,
       added_at,
       -- The author index is placement-neutral, so a card carries no folder.
       NULL AS source_directory,
       author_id, subtitle
FROM (
    SELECT *,
           ROW_NUMBER() OVER (
               PARTITION BY author_id
               ORDER BY COALESCE(read_at, 0) DESC, LOWER(COALESCE(title, '')), content_hash
           ) AS position
    FROM credited
)
WHERE position <= :limit
ORDER BY author_id, position;

-- name: get_author_subject_counts?
-- param: downloaded_only: i32
-- SQL counts distinct books per label/parent; Rust chooses and ranks the chips.
SELECT identity.stable_id, concept.preferred_label, route.parent_route_id,
       COALESCE(parent_concept.preferred_label, ''), COUNT(DISTINCT card.book_row_id)
FROM author_identity identity
JOIN author_credit credit ON credit.author_identity_id=identity.id
JOIN book_cards card ON card.book_row_id=credit.book_row_id
JOIN book_unified_concept assignment ON assignment.book_row_id=card.book_row_id
JOIN curated.unified_concept_route route ON route.concept_id=assignment.concept_id
JOIN curated.concept concept ON concept.concept_id=route.concept_id
LEFT JOIN curated.unified_concept_route parent ON parent.route_id=route.parent_route_id
LEFT JOIN curated.concept parent_concept ON parent_concept.concept_id=parent.concept_id
WHERE trim(credit.credited_name)!='' AND trim(concept.preferred_label)!=''
  AND (:downloaded_only=0 OR card.downloaded)
GROUP BY identity.stable_id, concept.preferred_label, route.parent_route_id;
