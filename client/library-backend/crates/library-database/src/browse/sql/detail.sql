-- name: book_detail_card?
-- param: content_hash: &str
SELECT content_hash,title,author,description,progress,downloaded,download_requested,format_category,audiobook_duration_ms,audiobook_chapter_count,added_at,NULL AS source_directory,subtitle FROM book_cards WHERE content_hash=:content_hash AND EXISTS (SELECT 1 FROM book_dir WHERE book_row_id=book_cards.book_row_id AND deleted_at IS NULL);

-- name: book_detail_directory?
-- param: content_hash: &str
SELECT dir_id FROM book_dir WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND deleted_at IS NULL ORDER BY dir_id LIMIT 1;

-- name: book_detail_authors?
-- param: content_hash: &str
SELECT identity.stable_id,credit.credited_name FROM author_credit credit JOIN author_identity identity ON identity.id=credit.author_identity_id WHERE credit.book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) ORDER BY credit.position;

-- name: book_detail_credits?
-- param: content_hash: &str
-- param: narrator_role: Vec<u8>
-- param: translator_role: Vec<u8>
SELECT identity.stable_id,contributor.name,contributor.role FROM book_contributor contributor JOIN author_identity identity ON identity.id=contributor.author_identity_id WHERE contributor.book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash) AND contributor.role IN (:narrator_role,:translator_role) ORDER BY contributor.position;

-- name: book_detail_subject_routes?
-- param: content_hash: &str
SELECT DISTINCT route.route_id FROM book_unified_concept assignment
JOIN curated.unified_concept_route route ON route.concept_id=assignment.concept_id
WHERE assignment.book_row_id=(SELECT row_id FROM book WHERE content_hash=:content_hash);

-- name: related_books?
-- param: author_ids: &str
-- param: content_hash: &str
SELECT DISTINCT card.content_hash,card.title,card.author,card.description,card.progress,card.downloaded,card.download_requested,card.format_category,card.audiobook_duration_ms,card.audiobook_chapter_count,card.added_at,NULL AS source_directory,card.subtitle FROM book_cards card JOIN author_credit credit ON credit.book_row_id=card.book_row_id JOIN author_identity identity ON identity.id=credit.author_identity_id WHERE identity.stable_id IN (SELECT value FROM json_each(:author_ids)) AND card.content_hash<>:content_hash AND EXISTS(SELECT 1 FROM book_dir placement WHERE placement.book_row_id=card.book_row_id AND placement.deleted_at IS NULL) ORDER BY LOWER(card.title),card.title,card.content_hash LIMIT 12;
