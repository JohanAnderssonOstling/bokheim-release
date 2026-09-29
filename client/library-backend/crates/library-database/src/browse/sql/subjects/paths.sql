-- Only used routes are expanded; the full taxonomy stays in its immutable database.
CREATE TEMP VIEW IF NOT EXISTS subject_route_paths AS
WITH RECURSIVE paths(route_id,concept_id,parent_route_id,path,parent_path,label) AS (
    SELECT r.route_id,r.concept_id,r.parent_route_id,c.preferred_label,'Subject',c.preferred_label
    FROM main.library_subject used
    JOIN curated.unified_concept_route r USING(route_id)
    JOIN curated.concept c USING(concept_id)
    WHERE r.parent_route_id=0
    UNION ALL
    SELECT r.route_id,r.concept_id,r.parent_route_id,p.path || ' / ' || c.preferred_label,p.path,c.preferred_label
    FROM paths p JOIN curated.unified_concept_route r ON r.parent_route_id=p.route_id
    JOIN main.library_subject used ON used.route_id=r.route_id
    JOIN curated.concept c ON c.concept_id=r.concept_id
)
SELECT * FROM paths
UNION ALL
SELECT 0,-1,0,'Subject','Subject','Subject' FROM main.library_subject WHERE route_id=0;
