UPDATE library_blob_claim SET blob_hash=$3 WHERE user_id=$1 AND content_hash=$2;
