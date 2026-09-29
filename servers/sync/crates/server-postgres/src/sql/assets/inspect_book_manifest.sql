WITH input AS MATERIALIZED (
    SELECT content_hash, size_bytes, ordinal
    FROM UNNEST($1::TEXT[], $2::BIGINT[]) WITH ORDINALITY
         AS value(content_hash, size_bytes, ordinal)
)
SELECT input.content_hash AS content_hash,
       input.size_bytes AS size_bytes,
       object.size_bytes AS stored_size_bytes,
       (charge.content_hash IS NOT NULL) AS owned,
       reservation.id AS reservation_id,
       reservation.library_id AS reservation_library_id,
       reservation.size_bytes AS reservation_size_bytes
FROM input
LEFT JOIN blob_object AS object
  ON object.content_hash=input.content_hash
LEFT JOIN user_blob_charge AS charge
  ON charge.user_id=$3
 AND charge.content_hash=input.content_hash
LEFT JOIN book_upload_reservation AS reservation
  ON reservation.user_id=$3
 AND reservation.content_hash=input.content_hash
ORDER BY input.ordinal;
