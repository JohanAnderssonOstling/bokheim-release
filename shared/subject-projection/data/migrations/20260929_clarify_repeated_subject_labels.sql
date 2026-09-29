-- Clarify code-specific leaves and a broad genre group whose labels repeat
-- an ancestor. Concept IDs and code assignments remain stable.
BEGIN IMMEDIATE;

UPDATE concept SET preferred_label='Operations Research & Systems Analysis', normalized_label='operations research & systems analysis'
WHERE concept_id=442 AND preferred_label='Applied Mathematics';

UPDATE concept SET preferred_label='General Personal Growth', normalized_label='general personal growth'
WHERE concept_id=3190 AND preferred_label='Personal Growth';

UPDATE concept SET preferred_label='Rhinologic Surgery', normalized_label='rhinologic surgery'
WHERE concept_id=8376 AND preferred_label='Surgery';

UPDATE concept SET preferred_label='Psychology of Religion', normalized_label='psychology of religion'
WHERE concept_id=10945 AND preferred_label='Religion';

UPDATE concept SET preferred_label='Film & Literature', normalized_label='film & literature'
WHERE concept_id=13577 AND preferred_label='Literature';

UPDATE concept SET preferred_label='Mystery & Thrillers', normalized_label='mystery & thrillers'
WHERE concept_id=55023 AND preferred_label='Suspense';

COMMIT;
