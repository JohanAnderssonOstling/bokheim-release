SELECT route.parent_route_id,concept.preferred_label
FROM curated.unified_concept_route route JOIN curated.concept concept USING(concept_id)
WHERE route.route_id=?1;
