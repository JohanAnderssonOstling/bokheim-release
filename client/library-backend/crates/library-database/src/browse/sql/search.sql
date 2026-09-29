-- name: search_library_cards?
-- param: normalized_query: &str
-- param: downloaded_only: bool
-- param: file_type: i32
SELECT card.content_hash,card.title,card.author,card.description,card.progress,
       card.downloaded,card.download_requested,card.format_category,
       card.audiobook_duration_ms,card.audiobook_chapter_count,
       card.added_at,NULL AS source_directory,card.subtitle,
       instr(card.search_title,:normalized_query)>0 AS title_match
FROM book_cards card
WHERE (instr(card.search_title,:normalized_query)>0 OR instr(card.search_author,:normalized_query)>0)
  AND (:downloaded_only=0 OR card.downloaded)
  AND (:file_type=0 OR card.format_category=:file_type)
ORDER BY card.sort_title,card.content_hash;

-- name: search_folder_cards?
-- param: directory_id: &str
-- param: normalized_query: &str
-- param: downloaded_only: bool
-- param: file_type: i32
SELECT card.content_hash,card.title,card.author,card.description,card.progress,
       card.downloaded,card.download_requested,card.format_category,
       card.audiobook_duration_ms,card.audiobook_chapter_count,
       card.added_at,NULL AS source_directory,card.subtitle,
       instr(card.search_title,:normalized_query)>0 AS title_match
FROM folder_book_ownership member
JOIN book_cards card ON card.book_row_id=member.book_row_id
WHERE member.folder_id=:directory_id AND (instr(card.search_title,:normalized_query)>0 OR instr(card.search_author,:normalized_query)>0)
  AND (:downloaded_only=0 OR card.downloaded)
  AND (:file_type=0 OR card.format_category=:file_type)
ORDER BY card.sort_title,card.content_hash;

-- name: search_subject_cards?
-- param: route_id: i64
-- param: normalized_query: &str
-- param: downloaded_only: bool
-- param: file_type: i32
SELECT card.content_hash,card.title,card.author,card.description,card.progress,
       card.downloaded,card.download_requested,card.format_category,
       card.audiobook_duration_ms,card.audiobook_chapter_count,
       card.added_at,NULL AS source_directory,card.subtitle,
       instr(card.search_title,:normalized_query)>0 AS title_match
FROM subject_browse_member member
JOIN book_cards card ON card.book_row_id=member.book_row_id
WHERE member.route_id=:route_id AND (instr(card.search_title,:normalized_query)>0 OR instr(card.search_author,:normalized_query)>0)
  AND (:downloaded_only=0 OR card.downloaded)
  AND (:file_type=0 OR card.format_category=:file_type)
ORDER BY card.sort_title,card.content_hash;
