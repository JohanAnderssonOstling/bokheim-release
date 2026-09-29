INSERT INTO book_subject_code(book_row_id,subject_position,system_id,code) VALUES(?1,?2,?3,?4) ON CONFLICT(book_row_id,subject_position,system_id,code) DO NOTHING
