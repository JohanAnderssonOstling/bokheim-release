INSERT INTO book_subject_assignment_evidence(book_row_id,system_id,subject_path,subject_position) VALUES(?1,?2,?3,?4) ON CONFLICT(book_row_id,system_id,subject_path,subject_position) DO NOTHING
