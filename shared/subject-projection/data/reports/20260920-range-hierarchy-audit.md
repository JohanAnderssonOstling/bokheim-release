# Classification range hierarchy audit — 2026-09-20

**A containment tree alone cannot preserve the current classifier.** Nested overlaps are useful and can stay compact, but the stored rules also contain crossing overlaps and subject priorities that do not follow geometric containment. Adding `parent_range_id` and selecting the deepest match would change assignments.

This audit is read-only. The source taxonomy and production matching behavior were not changed. The audit tool is an ignored release-mode test in `src/range_audit.rs`.

The audited snapshot has release 18, format 11, 16,686 concepts and **13,640 LCC ranges**. Its runtime revision is `9:21c106d3-2b70-5458-bbc4-86ccc4757a36`. Source edits were occurring concurrently, so the final audit used a frozen SQLite snapshot, not a changing source database. Snapshot SHA-256: `1d4105563caa6bed15346bf6e58e1f4d1c200600ee37e60908aedf9908760439`.

| Structural finding | Count |
|---|---:|
| Overlapping range pairs examined | 34,710 |
| Properly nested pairs | 34,541 |
| Crossing pairs: overlap without either containing the other | **169** |
| Distinct ranges participating in crossing overlaps | **289** |
| Crossing pairs meeting at a shared numeric boundary | 26 |
| Equal intervals | 0 |
| Ranges with no containing range | 450 |
| Ranges with one closest container | 13,035 |
| Ranges with two incomparable closest containers | **155** |

Of the 169 crossing pairs, 37 have the same concept, 77 connect ancestor/descendant concepts, and 55 connect unrelated concepts. These counts are structural conflicts for a forest, not counts of ambiguous book classifications. Same-owner overlap can have unambiguous output; current precedence also resolves many different-owner overlaps. Merging same-owner rules needs testing because interval width participates in current ranking.

For example, **England Travel** (`DA600–DA667`, concept 53071) and **Historic Places & Monuments** (`DA660–DA669`, concept 13834) cross over `DA660–DA667`. Neither can be the other's containing parent. Broad ranges need not be expanded into individual codes to address this, but this pair cannot be represented as nested disjoint siblings unchanged.

There are also 501 nested pairs where the *outer range's concept is a descendant of the inner range's concept*. This is distinct from ordinary broad-parent/narrow-child nesting. For the particularly simple probe **`DB37.5`**:

| Rule | Range | Concept ID |
|---|---|---:|
| Austrian | `DB37–DB38` | 3537 |
| Reference, a direct child of Austrian in the subject graph | `DB1–DB49` | 13872 |

The smallest containing range selects Austrian. The current matcher selects Reference because subject descendants shadow ancestors before comparing range specificity. Neither rule needs to be declared wrong for this example to disprove behavioral equivalence of the proposed tree.

The audit compared **46,982 generated point probes**: finite range endpoints, numeric midpoints and parseable exact keys. Cutter-family endpoint probes use a concrete digit within the family. These are synthetic coverage probes, not a measured error rate on real books.

| Proposed lookup | Same candidate sets | Different candidate sets |
|---|---:|---:|
| Inclusion-minimal covering ranges only | 37,208 | 9,774 |
| Exact-key resolution first; otherwise inclusion-minimal ranges | 44,449 | **2,533** |

The second model preserves specificity and subject-ancestor pruning among its exact candidates. Nevertheless, it differs on 945 probes with exact candidates and 1,588 without them. All 2,533 differences also change the strict resolved concept or unresolved status; none are merely different ambiguous candidate sets with the same unresolved result.

For example, `AG105` has an exact assignment to Reference & Information (4671). Current classification selects the descendant General Reference Works (13727) through its range. Unconditional exact-first lookup therefore also changes behavior.

The remaining range-only mismatches without exact candidates include 1,339 probes with a *unique* minimal concept, and 249 with multiple incomparable minima. This is not just a matter of handling tied smallest ranges.

The audit additionally formed **13,640 whole-interval inputs** from the stored ranges and required full containment. A range-only minimal-container model agreed with current candidates on 13,085 and differed on 555; all inputs parsed. This comparison deliberately omits exact overrides. It demonstrates why testing only single-code lookups is insufficient, not a full evaluation of an interval-aware replacement.

| Code system | Normalized exact keys | Keys with multiple owners | Keys still tied after ancestor pruning | `*` prefix rules |
|---|---:|---:|---:|---:|
| DDC | 873 | 0 | 0 | 0 |
| BISAC | 5,133 | 10 | **7** | 0 |
| LCC | 10,681 | 28 | **2** | 0 |

The seven BISAC ties are `REL006790`, `REL006800`, `REL006810`, `REL006820`, `REL006880`, `REL006890` and `REL009000`. The two LCC ties are `PJ` and `PL`. They remain ambiguous in the current matcher; a direct lookup must preserve that status or use a reviewed ownership decision, rather than choosing the first row. These are not automatically erroneous rules. LCC "exact" keys can also be ancestor keys generated from a full call number; a bare SQL equality against the original input is insufficient.

**Recommendation:** retain the compact authored ranges, and do not make a single containing-parent pointer the authoritative matching model. DDC already supports a straightforward normalized exact lookup; BISAC needs explicit handling for its tied keys. For LCC, database-backed candidate retrieval can preserve current precedence while removing the separate in-memory representation.

If the goal is to eliminate most runtime ranking too, investigate compiling precedence into disjoint decision regions at publication time, with an explicit unresolved result where necessary. This would be a derived database index over compact authored rules, not enumeration of every code. Its row count and behavior still need to be measured, and whole-interval inputs and exact-key interactions need their own validation. The audit has not proved that such an index is a complete replacement.

The geometric audit uses structured numeric/Cutter endpoints, including whole-class and whole-Cutter-family bounds. Its containment comparisons were cross-checked against the production containment function for every overlapping pair. Comparisons use `match_candidates`, followed by strict ambiguity handling where stated; damaged-input recovery, source-label inference and a real-book corpus were not exhaustively audited.

Artifacts in the workspace:

- [Summary JSON](../../../../target/range-hierarchy-audit/summary.json)
- [Every crossing pair](../../../../target/range-hierarchy-audit/crossing-ranges.json)
- [All ambiguous closest parents](../../../../target/range-hierarchy-audit/ambiguous-parents.json)
- [Point differences](../../../../target/range-hierarchy-audit/point-comparisons.json)
- [Exact-first counterexamples](../../../../target/range-hierarchy-audit/exact-first-differences.json)
- [Whole-interval differences](../../../../target/range-hierarchy-audit/interval-comparisons.json)
- [Exact-key conflicts and current results](../../../../target/range-hierarchy-audit/selector-conflicts.json)
- [Frozen database](../../../../target/range-hierarchy-audit/taxonomy.sqlite3)

Reproduce from the workspace root, preserving the original artifacts:

```sh
TAXONOMY_AUDIT_INPUT="$PWD/target/range-hierarchy-audit/taxonomy.sqlite3" \
TAXONOMY_AUDIT_DIR="$PWD/target/range-hierarchy-audit-rerun" \
cargo test --release -p subject-projection --lib runtime::range_audit::audit -- --ignored --nocapture
```

Omit `TAXONOMY_AUDIT_INPUT` to audit the current bundled taxonomy instead. Validation: the release audit passed, including the production-comparator cross-checks, in 10.12 seconds.
