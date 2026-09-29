# LCC range endpoints (taxonomy format 8)

The editable source taxonomy retains format 8 for curation. Bundled client
databases and upgraded writable caches use format 11. Format 9 introduced
structured ranges; format 11 stores only preferred names and removes aliases.

## One name per concept (format 11)

`concept.preferred_label` is the sole display name. `concept.normalized_label`
is its Unicode-lowercased, whitespace-collapsed lookup key. The nonunique
`(normalized_label,concept_id)` index supports matching without introducing
alternative names; different concepts may legitimately share a preferred name.
Taxonomy merge writes update both columns atomically.

Client upgrades remove the `source_label` table and its indexes. Old source
snapshots and wire payloads may still contain source-label fields: readers
ignore them, constructors discard them, and compatibility exports return an
empty list. FAST hierarchy matching uses route display names only. Matcher
revision 131 invalidates projections previously computed using aliases.

The editable format-8 source retains an empty compatibility table for older
curation scripts, but its entries are not loaded or matched. The former active
aliases have been deleted. Historical migration/reference records remain
archival evidence, not alternative names used by the application.

## Client storage (format 9)

`lcc_range` stores `concept_id` and, for each boundary, `letters`, `number`,
and `cutters` columns prefixed with `start_` or `end_`. Full `start_code` and
`end_code` strings are not stored. Canonical text is reconstructed at the
definition API boundary when required.

- Class letters are uppercase text; class numbers use SQLite REAL, matching
  the existing matcher's f64 comparisons.
- A NULL number denotes a whole-class boundary: zero at the lower end and
  infinity at the upper end. It is distinct from explicit numeric zero.
- Cutters are ordered JSON arrays of `[letters, fractional_digits]` pairs.
  Digits remain strings to preserve leading zeros and decimal ordering.
  Trailing fractional zeros are removed canonically; there may be multiple
  Cutter components. An empty digit string denotes the entire Cutter family.
- A unique index across the canonical boundaries preserves one owner per
  interval, including whole-class boundaries. Concept and class/number
  indexes support candidate lookups. Empty, malformed, and noncanonical
  persisted boundaries are rejected by the range reader/writer.

Format 8 to 9 migration is transactional and includes the two reviewed
historical corrections. Format 7 first uses its existing atomic upgrade
to format 8. Read-only versions 7 and 8 remain readable. The shared compiler
canonicalizes range selectors identically across storage versions, so
equivalent definitions have the same revision.

This storage change does not yet replace the separate precomputed matcher
with database-backed lookups.

## Editable source (format 8)

`lcc_range(start_code, end_code, concept_id)` stores complete, inclusive endpoints.
The primary key is `(start_code, end_code)`: each interval has one owner.

Examples:

| Start | End | Meaning |
|---|---|---|
| QA75.5 | QA76.95 | Numeric span within one subclass |
| D1 | DX301 | Numeric span across subclasses |
| QA273.A1 | QA274.9 | Span with a Cutter boundary |
| R | RZ | Complete subclasses R through RZ |

Letter-only lower endpoints begin at the start of the subclass. Letter-only
upper endpoints include the entire subclass. Numeric upper endpoints include
all their Cutter subdivisions. Cutter upper endpoints include their deeper
filing subdivisions; a broader incoming endpoint cannot fit inside a more
specific upper boundary. Thus an incoming `QC1.A1–QC1.C5` does not fit inside
`QC1.A1..QC1.C5.B9`.

Incoming ranges accept repeated or abbreviated numeric prefixes, abbreviated
Cutter endpoints, decimal-only endpoints, and hyphen, en-dash or em-dash separators.
A decimal-only endpoint inherits the integer part: `T57.6–.97` means
`T57.6–T57.97`; a Cutter endpoint such as `-.C4` inherits the full number. Explicit printed
range selectors take precedence. Otherwise one stored interval must contain
the complete incoming interval; matching the two ends independently or
bridging gaps between disjoint selectors is not sufficient. Equally specific
unrelated matches remain unresolved.

The Rust reader accepts the current structured taxonomy format. Bundled
databases and client seeds use the same range columns and strict endpoint
validation.

The taxonomy includes reviewed broad History (`D1..DX301`) and Medicine (`R..RZ`)
coverage. Existing narrower subjects retain precedence. Other unsupported
codes, missing selectors and holdings markers are separate curation issues.
