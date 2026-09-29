INSERT INTO book_unified_concept(book_row_id,concept_id,mapper_version) VALUES(?1,?2,?3) ON CONFLICT(book_row_id,concept_id) DO UPDATE SET mapper_version=excluded.mapper_version
