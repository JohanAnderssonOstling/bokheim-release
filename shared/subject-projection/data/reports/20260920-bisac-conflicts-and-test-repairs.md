# BISAC conflicts and taxonomy test repairs

The seven unresolved BISAC codes were incomplete selector moves, not a defect
in the matcher's ambiguity handling.

The approved Bible-group migration
added these codes to specific groups without removing their previous broad
owners. The groups occupy separate branches, so ancestor pruning could not
remove the broad owners. The bundled [BISAC 2021 headings](../bisac-2021.csv)
support the specific destinations selected by that migration.

| Code | Removed owner | Retained destination |
| --- | --- | --- |
| REL006790 | Biblical Commentary | Apocrypha & Pseudepigrapha |
| REL006800 | Biblical Commentary | Gospels & Acts |
| REL006810 | Biblical Commentary | Epistles |
| REL006820 | Biblical Commentary | Epistles |
| REL006880 | Old Testament | Apocrypha & Pseudepigrapha |
| REL006890 | New Testament | Apocrypha & Pseudepigrapha |
| REL009000 | Systematic Theology | Creeds & Catechisms |

The repair migration
was applied to `unified-taxonomy-v2.sqlite3`. It removes only these seven rows,
checks that each retained destination exists, and is idempotent. Subject
relationships and LCC ranges are unchanged. The matcher content revision is
derived from the changed selectors during compilation.

Using the production `taxonomy_probe` matcher on snapshots before and after
the repair, all 5,133 stored distinct BISAC selectors were compared. Exactly
these seven results changed, from unresolved to their retained destinations;
all 5,133 now resolve uniquely. A regression test covers these seven codes,
case/whitespace normalization, and four general codes that must remain on
the broad subjects. This comparison does not validate the semantics of every
other mapping against an external classification authority.

Four existing test failures were also investigated:

- Decorative arts: the specific subjects still matched correctly; expected
  paths used the retired `Other arts & art industries` grouping. Updated the
  assertions to the current decorative-arts route.
- Russian geography: the specific regions still matched correctly; expected
  paths predated the `History / By Region / European / East / Russian / Local`
  hierarchy. Updated the prefix while preserving the region assertions.
- Server overlay persistence: the fixture required a root named `Humanities`,
  which no longer exists. It now selects an existing root independently of its
  name and still verifies the inserted child's parent and selector.
- Damaged LCC projection: the test incorrectly required `QA76.` to resolve,
  contradicting the parser and recovery tests that preserve incomplete
  classifications as unresolved fragments. Kept the recoverable examples and
  added a separate test asserting that `QA76.` produces no assignment while
  preserving its original metadata and unresolved evidence. Parser behavior
  is unchanged.

Validation: `cargo test --release -p subject-projection` passed with 259 unit
tests and 13 integration tests; the manual range audit remains ignored.
SQLite integrity and foreign-key checks passed. Migration reruns made no
persistent changes; a missing retained destination caused rejection without
deleting mappings.

Local evidence: [before snapshot](../../../../target/bisac-conflict-fix/before.sqlite3)
and [complete changed-result comparison](../../../../target/bisac-conflict-fix/comparison.json).
The previous range audit describes its frozen pre-repair snapshot; this repair
does not resolve or reclassify its LCC overlap findings.
