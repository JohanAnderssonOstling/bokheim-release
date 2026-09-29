# Navigation storage

The bundled taxonomy stores each route occurrence as `route_id`, `concept_id`,
and `parent_route_id`. A concept can occur on several routes; its concept ID
alone cannot identify a navigation parent. Parent route zero denotes the
synthetic `Subject` root. Display names come from `concept.preferred_label`.

The build assigns route IDs deterministically in path order, then discards the
working paths. IDs are local to a bundle, not synchronization identifiers. The
library's derived subject membership cache records the bundled taxonomy revision
and rebuilds when it changes. Book assignments continue to store concept IDs.
Library schema 60 migrates schema 59 by discarding only derived browse state.

Single-route lookup walks parents in Rust using a cached prepared statement.
Resolving an incoming display path walks matching child labels, including labels
that themselves contain ` / `. Bulk navigation uses recursive SQL views; these
views do not persist path strings. Library views expand only used routes.

Folders already store parent IDs and names. Single-folder lookup uses a cached
parent statement within one read snapshot, reusing a caller's transaction when
present. Missing or deleted ancestors produce no path; cycles produce an error.
Bulk folder listings continue to use recursive SQL. Filesystem projection and
recovery records retain their last-known physical paths independently of the
current logical hierarchy.

The compiled matcher also stores numeric parent references rather than full
paths. Its route and concept records use vectors, with one display label per
concept. Concept-to-route lists contain numeric references. Path lookup uses a
sorted numeric hash index and checks the complete parent chain before accepting
a candidate, so hash collisions cannot change the result. Public route objects
still include their display paths, constructed when requested.

FAST matching starts from concepts whose preferred names match the supplied
heading components, then checks adjacent parent references. It does not expand
all taxonomy paths. Labels containing the display separator remain whole labels;
matcher version 132 refreshes assignments for that corrected edge case.

LCC range candidates use a static interval tree compiled alongside the matcher.
The parsed ranges remain in one vector. A second vector contains range IDs
sorted by lower class/number bound, with each implicit subtree root storing the
range ID of its greatest upper bound. Point and whole-interval queries prune
subtrees without allocating; Cutter containment and subject ranking still run
in the existing matcher. This replaces the per-subclass vectors of range IDs.
It does not change classification semantics or require an assignment-version
bump. See `reports/20260920-rust-interval-index.md` for validation and benchmarks.
