CREATE TABLE lcc_range(
    concept_id INTEGER NOT NULL REFERENCES concept(concept_id) ON DELETE CASCADE,
    start_letters TEXT NOT NULL CHECK(length(start_letters)>0 AND start_letters NOT GLOB '*[^A-Z]*'),
    start_number REAL CHECK(start_number>=0 AND start_number<=1.7976931348623157e308),
    start_cutters TEXT NOT NULL CHECK(json_valid(start_cutters) AND json_type(start_cutters)='array' AND json(start_cutters)=start_cutters),
    end_letters TEXT NOT NULL CHECK(length(end_letters)>0 AND end_letters NOT GLOB '*[^A-Z]*'),
    end_number REAL CHECK(end_number>=0 AND end_number<=1.7976931348623157e308),
    end_cutters TEXT NOT NULL CHECK(json_valid(end_cutters) AND json_type(end_cutters)='array' AND json(end_cutters)=end_cutters),
    CHECK(start_number IS NOT NULL OR start_cutters='[]'),
    CHECK(end_number IS NOT NULL OR end_cutters='[]')
) STRICT;
-- NULL class bounds must participate in uniqueness too. Numeric bounds are nonnegative.
CREATE UNIQUE INDEX lcc_range_owner_idx ON lcc_range(start_letters,coalesce(start_number,-1),start_cutters,end_letters,coalesce(end_number,-1),end_cutters);
CREATE INDEX lcc_range_concept_idx ON lcc_range(concept_id);
CREATE INDEX lcc_range_start_idx ON lcc_range(start_letters,start_number);
CREATE INDEX lcc_range_end_idx ON lcc_range(end_letters,end_number);
