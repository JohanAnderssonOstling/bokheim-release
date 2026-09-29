# Tudor compaction

Merged Henry VII, Henry VIII, Edward VI & Mary I, Elizabeth I and Thomas More into Tudor era (10149), as requested. The Tudor era subject is now a leaf beneath English → By Period.

During validation, an additional sibling duplicate, Tudor & Elizabethan Era (3532), was found owning BISAC HIS015030 and DA350–DA360. It was merged into the same Tudor era subject, so the shorter LCC range no longer sends Elizabethan material to a separate subject.

All six deleted concepts' selectors and ranges were transferred intact. Thomas More's European Biographies browsing relationship was removed with the person subject; Tudor era did not inherit that biography parent. The taxonomy now contains 16,735 subjects.

Focused checks of DA330, DA331, DA334.M8, DA334.M89, DA340, DA350, DA360.9 and HIS015030 resolve to Tudor era. All 54,700 existing selector and endpoint probes retain coverage. The migration matches its preview and passes integrity, foreign-key, cycle and sibling-label checks.

The full observed comparison checked 11,977,153 code strings: 1,727 changed destinations, all matching the six intended merges, with zero coverage losses or gains. The comparison used the existing optimized release matcher on frozen taxonomy snapshots.

Release test compilation was initially blocked by concurrent routing changes in `src/fast.rs`. After that code was updated, a retry passed all 13 curated scope tests. The library suite has 253 passes and the same four previously known failures; four tests were added by the concurrent source changes. No Rust implementation files were changed for this compaction. Final results and viewer verification are recorded in the [validation report](20260920-tudor-validation.json).

Artifacts: migration, [six-merge manifest](20260920-tudor-compaction.json), [observed classification changes](20260920-tudor-classification-changes.tsv). Usage counts and the viewer are refreshed against the final taxonomy.
