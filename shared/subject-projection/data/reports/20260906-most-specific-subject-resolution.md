# Most-specific subject resolution — 2026-09-06

Matching now collects every applicable selector before choosing a subject. A matching descendant shadows any matching ancestor across all parent paths, even when the ancestor has an exact selector or a higher range priority.

Among unrelated subjects, exact selectors beat ranges and prefixes; more specific exact keys and narrower numeric ranges win. A single-subclass range beats a cross-subclass range. Range priority only breaks equally specific range ties. Equally ranked unrelated concepts remain available through diagnostic candidate APIs, but no subject is assigned arbitrarily. The one-subject rule now applies to BISAC as well as LCC. Multiple paths for the same concept remain valid.

The runtime revision moved to 7 and the projection matcher version to 25 so cached projections can be invalidated. The cached production audit executable was rebuilt, and the full match cache, usage counts, and viewer were refreshed against a guarded final snapshot. No taxonomy subjects or selectors were modified by this task.

## Validation

- Six focused specificity tests and the existing storage-pruning regression test passed. These cover descendants defeating exact/high-priority ancestors, multiple parents, unrelated depth, narrow versus broad ranges, unknown subclasses, and BISAC ambiguity.
- Both old and new matchers ran against the same frozen taxonomy and the complete cached Open Library usage dataset: 3,774,074 codes representing 21,512,946 classification uses. Both matched 2,852,806 codes / 14,027,268 uses, with no new or lost matches.
- 29,590 codes / 135,016 uses moved to matching descendants. Another 6,367 codes / 30,060 uses changed because selector specificity now precedes range priority among unrelated subjects.
- An initial comparison against an older cache suggested 1,134 missing matches. Re-running the actual old matcher against the same snapshot established that these were stale cache entries, not new ambiguities or losses from this change.
- The full library test run exposed older taxonomy-path expectations that no longer match the curated database. Sampled failures (business, Protestant denominations, electricity, Russia) were reproduced with the old matcher. Those unrelated fixture expectations were retained. The taxonomy changed again during testing; refreshed usage was installed only after comparing every live table with the final snapshot.

Evidence: `client/.test-tmp/most-specific-routing/`, including full before/after matcher outputs, `routing-audit.json`, focused test logs, baseline fixture outputs, final installation snapshot, and `completion.json`.
