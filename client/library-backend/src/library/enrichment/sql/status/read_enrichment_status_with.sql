WITH per_book AS (
            SELECT b.row_id,
                CASE
                    WHEN MAX(a.status='updated')=1 OR MAX(j.status='applied')=1 THEN 'enriched'
                    WHEN MAX(a.status='failed')=1 OR MAX(j.status='unavailable')=1 THEN 'failed'
                    WHEN MAX(a.status='ambiguous')=1 OR MAX(j.status='ambiguous')=1 THEN 'no_match'
                    WHEN MAX(pending.content_hash IS NOT NULL)=1 THEN 'pending'
                    WHEN COUNT(a.provider_id)>0 OR MAX(j.status='no_match')=1 THEN 'no_match'
                    ELSE 'pending'
                END AS state
            FROM book b LEFT JOIN external_metadata_attempt a
                ON a.book_row_id=b.row_id AND a.provider_id IN (?1,?2,?3,?4)
            LEFT JOIN audible_enrichment_jobs j ON j.content_hash=b.content_hash
            LEFT JOIN pending_audible_enrichment pending ON pending.content_hash=b.content_hash
            WHERE b.deleted_at IS NULL AND b.hidden_at IS NULL
            GROUP BY b.row_id
        ) SELECT COUNT(*), COALESCE(SUM(state='enriched'),0), COALESCE(SUM(state='pending'),0),
            COALESCE(SUM(state='no_match'),0), COALESCE(SUM(state='failed'),0)
          FROM per_book
