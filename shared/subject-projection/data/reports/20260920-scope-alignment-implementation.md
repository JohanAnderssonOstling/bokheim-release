# Scope alignment implementation

Implemented the six subjects identified in the [scope audit](20260920-scope-audit.md). Changes are local.

| Area | Result |
|---|---|
| Good & Evil | BJ1296–BJ1517.9 now uses Ethics as its broad fallback. Good & Evil retains BJ1400–BJ1408.5 and its BISAC mapping. Existential ethics, duty and happiness no longer fall into Good & Evil. |
| Psycholinguistics & Synesthesia | Reused Psycholinguistics (3784), adding a Cognition browsing route alongside Linguistics. It owns BF455–BF463. The former mixed subject (12878) is now Synesthesia, owning BF495–BF499. Intervening cognitive topics fall back to Cognition. |
| Habit, Adjustment & Nature–Nurture | Narrowed coverage to BF335–BF337 and BF341–BF346. Posture and imitation now resolve to Cognition. Environmental Psychology is directly under Cognition. The literary Imitation subject was deliberately not reused for psychological imitation. |
| Creative Thinking | Restricted to BF408–BF426. Intelligence and general thinking retain their specific mappings; the broader BF408–BF454.9 fallback belongs to Cognition. |
| Acute & Specialty Care | Broad RT89–RT120 coverage belongs to Nursing. RT89–RT89.3 belongs to Management & Leadership. Public/community nursing and home/rural nursing codes use Home & Community Nursing. Removed the now-uncoded wrapper and promoted its five children directly under Nursing. |
| Mental Illness Prevention | Renamed Public Mental Health, keeping its Public Health parent and RA790–RA790.95 coverage. |

The migration preserves broad fallback coverage rather than dropping code intervals. It updates explicit selectors as well as stored ranges. Narrow range rows preserve psycholinguistics, synesthesia and creativity precedence in the production matcher. No new subject IDs were needed; one wrapper was removed, leaving 16,909 concepts.

Validation includes real-label classification probes, migration replay equivalence, database integrity, foreign keys, cycle and sibling-label checks, 41,105 existing selector/endpoint probes, and the full observed LCC/DDC code comparison. Exact comparison totals and the final database fingerprint are recorded in [validation results](20260920-scope-alignment-validation.json). The usage table and viewer are regenerated from the corrected classification results.

All nine curated scope regression tests pass in the optimized release build. The library suite has 249 passes and the same four failures before and after this migration:

- `runtime::tests::decorative_arts_lcc_detail_routes_to_existing_concepts`
- `taxonomy_sqlite::tests::server_slice_is_persisted_as_an_ancestor_closed_local_overlay`
- `tests::declared_damaged_lcc_reaches_unified_projection_with_original_position`
- `tests::frequent_russian_geographic_codes_resolve_to_specific_regions`

Artifacts: migration, [subject change manifest](20260920-scope-alignment.json), [observed classification changes](20260920-scope-alignment-classification-changes.tsv).
