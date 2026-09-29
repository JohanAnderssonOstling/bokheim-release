-- name: get_immediate_sub_dirs?
-- param: dir_id: &str
-- param: downloaded_only: bool
-- param: chip_sort: i32
SELECT directory.id,directory.name,COUNT(book.row_id) AS book_count,
       COUNT(CASE WHEN ownership.visible_downloaded_count>0 THEN book.row_id END) AS downloaded_book_count,
       NULL AS path
FROM dir parent JOIN dir directory ON directory.parent_id=parent.id AND directory.id!=parent.id
LEFT JOIN folder_book_ownership ownership ON ownership.folder_id=directory.id AND ownership.visible_placement_count>0
LEFT JOIN book ON book.row_id=ownership.book_row_id
  AND book.deleted_at IS NULL AND book.hidden_at IS NULL
WHERE parent.id=:dir_id AND parent.deleted_at IS NULL
  AND directory.deleted_at IS NULL AND directory.name NOT LIKE '.%'
GROUP BY directory.id,directory.name
HAVING NOT :downloaded_only OR downloaded_book_count>0
ORDER BY CASE WHEN :chip_sort=1 THEN CASE WHEN :downloaded_only THEN downloaded_book_count ELSE book_count END END DESC,
         normalize_search_text(directory.name),directory.name,directory.id;

-- name: get_filtered_immediate_sub_dirs?
-- param: dir_id: &str
-- param: downloaded_only: bool
-- param: file_types: i32
-- param: chip_sort: i32
-- param: languages: &str
SELECT directory.id,directory.name,COUNT(book.row_id) AS book_count,
       COUNT(CASE WHEN ownership.visible_downloaded_count>0 THEN book.row_id END) AS downloaded_book_count,
       NULL AS path
FROM dir parent JOIN dir directory ON directory.parent_id=parent.id AND directory.id!=parent.id
JOIN folder_book_ownership ownership ON ownership.folder_id=directory.id AND ownership.visible_placement_count>0
JOIN book ON book.row_id=ownership.book_row_id
  AND book.deleted_at IS NULL AND book.hidden_at IS NULL
WHERE parent.id=:dir_id AND parent.deleted_at IS NULL
  AND directory.deleted_at IS NULL AND directory.name NOT LIKE '.%'
  AND (:file_types=0 OR EXISTS(SELECT 1 FROM book_browse_format format
       WHERE format.book_row_id=book.row_id AND (:file_types & format.format_category)!=0))
  AND (:languages='' OR EXISTS(SELECT 1 FROM book_browse_language language
       WHERE language.book_row_id=book.row_id AND instr(:languages,',' || language.base_language || ',')>0))
GROUP BY directory.id,directory.name
HAVING NOT :downloaded_only OR downloaded_book_count>0
ORDER BY CASE WHEN :chip_sort=1 THEN CASE WHEN :downloaded_only THEN downloaded_book_count ELSE book_count END END DESC,
         normalize_search_text(directory.name),directory.name,directory.id;

-- name: get_sub_dirs?
-- param: dir_id: &str
-- param: downloaded_only: bool
-- param: normalized_query: &str
-- param: file_types: i32
-- param: chip_sort: i32
-- param: languages: &str
WITH RECURSIVE scope(id) AS (
    SELECT id FROM dir WHERE id=:dir_id AND deleted_at IS NULL
    UNION
    SELECT child.id FROM scope parent JOIN dir child ON child.parent_id=parent.id
    WHERE child.id!=parent.id AND child.deleted_at IS NULL AND child.name NOT LIKE '.%'
      AND (:normalized_query!='' OR parent.id=:dir_id)
)
SELECT directory.id,directory.name,COUNT(card.book_row_id) AS book_count,
       COUNT(CASE WHEN ownership.visible_downloaded_count>0 THEN card.book_row_id END) AS downloaded_book_count,
       CASE WHEN :normalized_query='' THEN NULL ELSE parent_path.path END AS path
FROM scope JOIN dir directory ON directory.id=scope.id
LEFT JOIN dir_paths parent_path ON parent_path.id=directory.parent_id
JOIN folder_book_ownership ownership ON ownership.folder_id=directory.id AND ownership.visible_placement_count>0
JOIN book_cards card ON card.book_row_id=ownership.book_row_id
WHERE directory.id!=:dir_id
  AND ((:normalized_query='' AND directory.parent_id=:dir_id)
       OR (:normalized_query!='' AND instr(normalize_search_text(directory.name),:normalized_query)>0))
  AND (:file_types=0 OR (:file_types & card.format_category)!=0)
  AND (:languages='' OR EXISTS(SELECT 1 FROM book_browse_language language
       WHERE language.book_row_id=card.book_row_id AND instr(:languages,',' || language.base_language || ',')>0))
GROUP BY directory.id,directory.name,parent_path.path
HAVING NOT :downloaded_only OR downloaded_book_count>0
ORDER BY CASE WHEN :chip_sort=1 THEN CASE WHEN :downloaded_only THEN downloaded_book_count ELSE book_count END END DESC,
         normalize_search_text(directory.name),directory.name,directory.id;

-- name: get_folder_facets?
-- param: dir_id: &str
-- param: normalized_query: &str
-- param: file_types: i32
-- param: languages: &str
-- param: downloaded_only: i32
WITH RECURSIVE descendants(id,name) AS MATERIALIZED (
    SELECT id,name FROM dir WHERE id=:dir_id AND deleted_at IS NULL
    UNION
    SELECT child.id,child.name FROM dir child
    JOIN descendants parent ON child.parent_id=parent.id
    WHERE child.deleted_at IS NULL AND child.id!=child.parent_id
), scope AS NOT MATERIALIZED (
    SELECT member.book_row_id,format.format_category
    FROM folder_book_ownership member
    JOIN book ON book.row_id=member.book_row_id AND book.deleted_at IS NULL AND book.hidden_at IS NULL
    JOIN book_browse_format format ON format.book_row_id=member.book_row_id
    WHERE member.folder_id=:dir_id
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
               SELECT 1 FROM descendants candidate
               JOIN folder_book_ownership candidate_ownership
                 ON candidate_ownership.folder_id=candidate.id AND candidate_ownership.book_row_id=scope.book_row_id
               WHERE candidate.id!=:dir_id
                 AND instr(normalize_search_text(COALESCE(candidate.name,'')),:normalized_query)>0
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

-- name: get_book_cards_in_dir?
-- param: dir_id: &str
-- param: downloaded_only: i32
-- param: file_types: i32
-- param: book_sort: i32
-- param: languages: &str
SELECT card.content_hash,card.title,card.author,card.description,card.progress,
       card.downloaded,card.download_requested,card.format_category,
       card.audiobook_duration_ms,card.audiobook_chapter_count,
       card.added_at,placement.dir_id AS source_directory,card.subtitle
FROM book_dir placement
JOIN dir directory ON directory.id=placement.dir_id AND directory.deleted_at IS NULL
JOIN book_cards card ON card.book_row_id=placement.book_row_id
WHERE placement.dir_id=:dir_id AND placement.deleted_at IS NULL
  AND (:downloaded_only=0 OR card.downloaded)
  AND (:file_types=0 OR (:file_types & card.format_category)!=0)
  AND (:languages='' OR EXISTS(SELECT 1 FROM book_browse_language language
       WHERE language.book_row_id=card.book_row_id AND instr(:languages,',' || language.base_language || ',')>0))
ORDER BY CASE WHEN :book_sort=1 THEN (
    SELECT MAX(reading.changed_at) FROM sync_state_version reading
    WHERE reading.state_kind='reading_position' AND reading.state_key=card.content_hash
) END DESC, card.sort_title,card.title,card.content_hash;

-- name: search_book_cards_in_dir?
-- param: dir_id: &str
-- param: downloaded_only: i32
-- param: normalized_query: &str
-- param: file_types: i32
-- param: book_sort: i32
-- param: languages: &str
SELECT card.content_hash,card.title,card.author,card.description,card.progress,
       card.downloaded,card.download_requested,card.format_category,
       card.audiobook_duration_ms,card.audiobook_chapter_count,
       card.added_at,ownership.source_directory,card.subtitle
FROM folder_book_ownership ownership
JOIN book_cards card ON card.book_row_id=ownership.book_row_id
WHERE ownership.folder_id=:dir_id
  AND (:downloaded_only=0 OR card.downloaded)
  AND (:file_types=0 OR (:file_types & card.format_category)!=0)
  AND (:languages='' OR EXISTS(SELECT 1 FROM book_browse_language language
       WHERE language.book_row_id=card.book_row_id AND instr(:languages,',' || language.base_language || ',')>0))
  AND (instr(card.search_title,:normalized_query)>0
       OR instr(card.search_author,:normalized_query)>0)
ORDER BY CASE WHEN :book_sort=1 THEN (
    SELECT MAX(reading.changed_at) FROM sync_state_version reading
    WHERE reading.state_kind='reading_position' AND reading.state_key=card.content_hash
) END DESC, card.sort_title,card.title,card.content_hash;

-- name: get_descendant_content_hashes?
-- param: dir_id: &str
SELECT book.content_hash
FROM folder_book_ownership member
JOIN book ON book.row_id=member.book_row_id
WHERE member.folder_id=:dir_id
  AND book.deleted_at IS NULL AND book.hidden_at IS NULL
ORDER BY book.content_hash;

-- name: group_folder_children_with?
-- param: directory_ids: &str
-- param: file_types: i32
-- param: languages: &str
-- param: chip_sort: i32
-- param: downloaded_only: bool
WITH candidates(id,parent_id,name) AS (
            SELECT d.id,d.parent_id,d.name FROM json_each(:directory_ids) selected
            JOIN dir parent ON parent.id=selected.value
            JOIN dir d ON d.parent_id=parent.id AND d.id!=parent.id
            WHERE parent.deleted_at IS NULL AND d.deleted_at IS NULL AND d.name NOT LIKE '.%'
              AND NOT EXISTS(SELECT 1 FROM book_dir p JOIN book b ON b.row_id=p.book_row_id
                  WHERE p.dir_id=parent.id AND p.deleted_at IS NULL AND b.deleted_at IS NULL AND b.hidden_at IS NULL)

)
SELECT c.parent_id,c.id,c.name,COUNT(book.row_id) AS total,
       COUNT(CASE WHEN ownership.visible_downloaded_count>0 THEN book.row_id END) AS downloaded
FROM candidates c
LEFT JOIN folder_book_ownership ownership ON ownership.folder_id=c.id AND ownership.visible_placement_count>0
LEFT JOIN book ON book.row_id=ownership.book_row_id
  AND book.deleted_at IS NULL AND book.hidden_at IS NULL
  AND (:file_types=0 OR EXISTS(SELECT 1 FROM book_browse_format format
       WHERE format.book_row_id=book.row_id AND (:file_types & format.format_category)!=0))
  AND (:languages='' OR EXISTS(SELECT 1 FROM book_browse_language language
       WHERE language.book_row_id=book.row_id AND instr(:languages,',' || language.base_language || ',')>0))
GROUP BY c.parent_id,c.id,c.name
-- Keep empty folders only when browsing all holdings without metadata filters.
HAVING (NOT :downloaded_only OR downloaded>0)
   AND ((:file_types=0 AND :languages='') OR total>0)
ORDER BY c.parent_id,CASE WHEN :chip_sort=1 THEN CASE WHEN :downloaded_only THEN downloaded ELSE total END END DESC,
         normalize_search_text(c.name),c.name,c.id;
