INSERT INTO user_storage_account(user_id)
SELECT id FROM users WHERE id=$1
ON CONFLICT (user_id) DO NOTHING
