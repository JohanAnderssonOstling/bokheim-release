-- name: get_folder_language_facets?
-- param: dir_id: &str
-- param: normalized_query: &str
-- param: file_types: i32
-- param: downloaded_only: i32
SELECT language.base_language,COUNT(DISTINCT ownership.book_row_id)
FROM folder_book_ownership AS ownership
JOIN book_cards AS card ON card.book_row_id=ownership.book_row_id
JOIN book_browse_format AS format ON format.book_row_id=ownership.book_row_id
JOIN book_browse_language AS language ON language.book_row_id=ownership.book_row_id
WHERE ownership.folder_id=:dir_id
  AND (:downloaded_only=0 OR card.downloaded)
  AND (:file_types=0 OR (:file_types & format.format_category) != 0)
  AND (
      :normalized_query=''
      OR instr(card.search_title,:normalized_query)>0
      OR instr(card.search_author,:normalized_query)>0
      OR EXISTS(
          SELECT 1
          FROM dir AS candidate
          JOIN dir_paths AS candidate_path ON candidate_path.id=candidate.id
          JOIN dir_paths AS current_path ON current_path.id=:dir_id
          JOIN folder_book_ownership AS candidate_ownership
            ON candidate_ownership.folder_id=candidate.id
           AND candidate_ownership.book_row_id=ownership.book_row_id
          WHERE candidate.id!=:dir_id
            AND (current_path.path='' OR substr(candidate_path.path,1,length(current_path.path)+1)=current_path.path || '/')
            AND instr(normalize_search_text(COALESCE(candidate.name,'')),:normalized_query)>0
      )
  )
GROUP BY language.base_language ORDER BY language.base_language;

-- Format facets apply the current folder, search and language constraints,
-- while deliberately ignoring the format selection itself.
-- name: get_folder_format_facets?
-- param: dir_id: &str
-- param: normalized_query: &str
-- param: languages: &str
-- param: downloaded_only: i32
SELECT format.format_category,COUNT(DISTINCT ownership.book_row_id)
FROM folder_book_ownership AS ownership
JOIN book_cards AS card ON card.book_row_id=ownership.book_row_id
JOIN book_browse_format AS format ON format.book_row_id=ownership.book_row_id
WHERE ownership.folder_id=:dir_id
  AND (:downloaded_only=0 OR card.downloaded)
  AND (:languages='' OR EXISTS(SELECT 1 FROM book_browse_language AS language WHERE language.book_row_id=ownership.book_row_id AND instr(:languages,',' || language.base_language || ',')>0))
  AND (
      :normalized_query=''
      OR instr(card.search_title,:normalized_query)>0
      OR instr(card.search_author,:normalized_query)>0
      OR EXISTS(
          SELECT 1
          FROM dir AS candidate
          JOIN dir_paths AS candidate_path ON candidate_path.id=candidate.id
          JOIN dir_paths AS current_path ON current_path.id=:dir_id
          JOIN folder_book_ownership AS candidate_ownership
            ON candidate_ownership.folder_id=candidate.id
           AND candidate_ownership.book_row_id=ownership.book_row_id
          WHERE candidate.id!=:dir_id
            AND (current_path.path='' OR substr(candidate_path.path,1,length(current_path.path)+1)=current_path.path || '/')
            AND instr(normalize_search_text(COALESCE(candidate.name,'')),:normalized_query)>0
      )
  )
GROUP BY format.format_category ORDER BY format.format_category;

