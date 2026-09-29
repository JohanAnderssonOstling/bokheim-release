# Selector reintroduction audit

The old `compile-curated-selectors.py` initialized a plan from curated rows,
then unconditionally rolled master rows into retained IDs or their nearest
retained master ancestors. It deduplicated identical strings, but did not
respect reviewed containing ranges or selector moves to other destinations.
It also inferred wildcard selectors from one-letter class selectors and pruned
ancestor duplicates. These transformations could change coverage or ownership,
not merely storage size. Independently allocated concept IDs were treated as
shared identity without checking labels.

On one immutable snapshot, the old compiler produced 64,701 selectors from
38,301 (+26,400). The updated maintenance plan retains exactly 38,301, and a
second application to an audit copy leaves the rows unchanged. Indigenous Law
would have grown from 504 selectors to 723 with the old plan; it remains at 504
with maintenance. The initial live dry run used a slightly earlier state
(38,414 → 64,810); the frozen comparison is the reproducible result.

This demonstrates a concrete reintroduction path. Execution history was not
available to establish that this script caused every earlier selector increase;
other concurrent migrations and direct database writes also occurred.

## Changes

- Maintenance now canonicalizes only existing selector syntax, preserving
  destinations, boundaries, exact exceptions and all source systems. It no longer
  imports master selectors, infers wildcards, merges ranges or prunes ancestors.
- Master imports are read-only CSV proposals, separate from `--apply`. Proposals
  suppress already assigned selectors and same-destination numeric coverage,
  preserve range gaps and check labels before treating IDs as retained. The
  frozen export produced 21,688 candidates and 94 unresolved source selectors;
  these are unapproved candidates, not applied taxonomy additions.
- Applying maintenance rejects concurrent selector changes rather than replacing
  the database from a stale plan. Only the changed rows are written.
- `attach-observed-lcc.py` no longer overwrites a current matched or ambiguous
  coverage status with `unmatched` merely because a stale candidate CSV includes
  the notation. The other uncovered-selector tool already filters by coverage.
- `refresh-curation-usage.sh` only rebuilds hit-count and match-cache artifacts;
  it does not write selectors and was left unchanged.

## Validation

Six regression tests passed: maintenance idempotence and preservation of exact
exceptions, proposals respecting ranges and explicit moves, independent ID
collisions, range gaps, concurrent writes, and the observed-code proposal flow
with a stale matched candidate plus a genuinely unmatched candidate. A separate
CLI check confirmed that combining master proposals with `--apply` is rejected
without changing the audit database. Real snapshot maintenance was applied twice
and verified row-for-row against syntax-canonicalized input.

Neither live taxonomy database was modified by this task, and no cache refresh
was necessary. Other direct SQL writers are not constrained by these tool
changes. Operational instructions: `tools/SELECTOR-MAINTENANCE.md`.
Evidence: `client/.test-tmp/selector-import-audit/`, including the previous
compiler, frozen databases, `result.json`, regression output and proposal CSV.
