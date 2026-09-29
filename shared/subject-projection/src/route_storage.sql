-- Route occurrences are identified by the chain of concept ids that reaches
-- them, not by displayed labels: two different concepts can carry the same
-- preferred_label, and a label-keyed chain collided under that condition.
DROP TABLE IF EXISTS unified_concept_route;
CREATE TEMP TABLE compiled_routes AS
WITH RECURSIVE routes(key,concept_id,parent_key) AS (
    SELECT '/' || c.concept_id,c.concept_id,'' FROM concept c
    WHERE NOT EXISTS(SELECT 1 FROM concept_parent p WHERE p.concept_id=c.concept_id)
      AND (EXISTS(SELECT 1 FROM concept_parent p WHERE p.parent_concept_id=c.concept_id)
        OR EXISTS(SELECT 1 FROM lcc_selector s WHERE s.concept_id=c.concept_id)
        OR EXISTS(SELECT 1 FROM lcc_range s WHERE s.concept_id=c.concept_id)
        OR EXISTS(SELECT 1 FROM bisac_selector s WHERE s.concept_id=c.concept_id)
        OR EXISTS(SELECT 1 FROM ddc_selector s WHERE s.concept_id=c.concept_id))
    UNION
    SELECT r.key || '/' || c.concept_id,c.concept_id,r.key
    FROM routes r JOIN concept_parent p ON p.parent_concept_id=r.concept_id
    JOIN concept c ON c.concept_id=p.concept_id
)
SELECT row_number() OVER (ORDER BY key) AS route_id,* FROM routes;
CREATE UNIQUE INDEX temp.compiled_route_key ON compiled_routes(key);
CREATE TABLE unified_concept_route(
    route_id INTEGER PRIMARY KEY CHECK(route_id>0),
    concept_id INTEGER NOT NULL REFERENCES concept(concept_id),
    parent_route_id INTEGER NOT NULL CHECK(parent_route_id>=0 AND parent_route_id<>route_id)
) STRICT;
INSERT INTO unified_concept_route
SELECT child.route_id,child.concept_id,COALESCE(parent.route_id,0)
FROM compiled_routes child LEFT JOIN compiled_routes parent ON parent.key=child.parent_key;
DROP TABLE compiled_routes;
CREATE INDEX idx_unified_concept_route_parent ON unified_concept_route(parent_route_id,concept_id);
CREATE INDEX idx_unified_concept_route_concept ON unified_concept_route(concept_id);
-- Bulk navigation derives paths in one traversal. Zero is the synthetic Subject root.
CREATE VIEW unified_concept_paths AS
WITH RECURSIVE paths(route_id,concept_id,parent_route_id,path,parent_path,label) AS (
    SELECT r.route_id,r.concept_id,r.parent_route_id,c.preferred_label,'Subject',c.preferred_label
    FROM unified_concept_route r JOIN concept c USING(concept_id) WHERE r.parent_route_id=0
    UNION ALL
    SELECT r.route_id,r.concept_id,r.parent_route_id,p.path || ' / ' || c.preferred_label,p.path,c.preferred_label
    FROM paths p JOIN unified_concept_route r ON r.parent_route_id=p.route_id
    JOIN concept c ON c.concept_id=r.concept_id
)
SELECT * FROM paths;
