# Ambiguous LCC source-record review

Reviewed two damaged notations across four Open Library editions. The three editions carrying `BL65.C8BL60HM621-HM6` also supply different complete classifications. The damaged string therefore cannot have a single safe replacement.

| Edition | Complete alternative on this edition | Cached subject |
|---|---|---|
| [Islam and Controversy](https://openlibrary.org/books/OL27986979M) | PN605.I8 M66 2014 | Literary Movements & Relations |
| [Reimagining The European Family Cultures Of Immigration](https://openlibrary.org/books/OL26018494M) | HQ626 .S48 2013 | Family Studies by Country |
| [Advertising Commercial Spaces And The Urban](https://openlibrary.org/books/OL26116404M) | HF5821 .C757 2010 | Marketing |
| [Social Cultural Engineering and the Singaporean State](https://openlibrary.org/books/OL28214894M) | None | Unmatched |

## Findings

- Current Open Library JSON already contains the broken strings. The aggregate cache retains complete notation (normalization version 2), so these examples are not truncation introduced by that cache.
- `servers/metadata/src/import/core.rs:632` imports each classification array entry separately, normalizing whitespace but not concatenating entries. Complete alternatives are preserved.
- Each of the three usable alternatives has a subject match in the existing matching cache. A damaged value on an edition does not, by itself, mean that the edition has no subject.
- The fourth edition, OL28214894M, has only `BL65.C8BL60GN301-GN6`. Another edition of the same work, OL28215909M, supplies the broad range `H1-970.9`. This is not evidence for completing the damaged range or copying a shelfmark onto the first edition.
- No guessed range completion, global alias, selector addition or source-record edit was made.

## Limits and follow-up

The local edition-level source snapshot recorded in cache metadata is no longer available at its recorded path. This review checks current Open Library records and the existing published matching cache; it does not establish how many of all historical damaged rows have a valid alternative. A complete edition-level audit requires restoring the source snapshot or querying a source service retaining edition IDs.

The initial optimized release build was blocked by duplicate Management Information Systems paths. The conflicting parent link for subject 12552 was subsequently removed by `20260907_remove_duplicate_mis_navigation_link.sql`. Subject 12552 retains its Strategic Management parent; subject 134 retains its IT routes. A fresh optimized release library build now passes, and no duplicate sibling paths remain. Verification log: `/tmp/lcc-source-review/build-recheck.log`.

Machine-readable evidence: `20260907-ambiguous-lcc-source-review.json`. Downloaded source records and build log: `/tmp/lcc-source-review/`.

## Source database located

The July 2026 source snapshot was subsequently located on metadata server `192.168.1.68`. The completed edition-level audit is documented in `20260907-edition-lcc-coverage-audit.md`; all 15 source editions carrying the original damaged example have now been checked.
