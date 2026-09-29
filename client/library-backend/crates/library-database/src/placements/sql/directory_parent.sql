SELECT parent_id,name FROM dir WHERE id=?1 AND deleted_at IS NULL;
