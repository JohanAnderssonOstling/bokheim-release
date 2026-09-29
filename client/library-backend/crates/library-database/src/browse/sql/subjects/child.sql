SELECT route.route_id,concept.preferred_label FROM curated.unified_concept_route route
JOIN curated.concept concept USING(concept_id)
WHERE route.parent_route_id=?1
  AND (?2=concept.preferred_label OR substr(?2,1,length(concept.preferred_label)+3)=concept.preferred_label || ' / ');
