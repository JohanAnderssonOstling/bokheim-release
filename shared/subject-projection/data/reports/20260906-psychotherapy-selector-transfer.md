# Psychotherapy selector transfer

Moved 19,435 direct classification uses across 3,689 normalized codes from Internal Medicine to the existing Clinical Psychology / Psychotherapy subject. Transferred 160 stored parent selectors and supplemented observed descendant codes. No concepts or parent links were changed.

Internal Medicine now retains 36,055 direct hits across 5,872 codes.

## Included

- RC480–RC481: general psychotherapy, brief therapy, crisis intervention, therapeutic relationships and client-centered psychotherapy.
- RC488–RC488.8: group, family, marital and couples therapies and support groups.
- RC489 Cutter codes explicitly identified in the local LCC reference as psychological therapies or related aspects. Unknown Cutters and selected non-psychotherapy techniques were retained for review.

## Excluded

- RC475–RC475.7: general mixed therapeutics reference material.
- RC482: physical therapy; RC483–RC483.5: medication; RC483.9–RC485.5: shock treatments; RC487: occupational therapy.
- RC489.N3 narcotherapy, F44 Feldenkrais, R64 Rolfing, S5 sleep therapy, S68 sports therapy, R86 running/jogging and H67 horseback riding.
- Hypnosis, psychoanalysis, diagnosis, nursing and psychiatric disorders were outside this transfer.

## Validation

The real taxonomy matcher evaluated all 42,279 RC input codes before and after. Every change moved from Internal Medicine to Psychotherapy; no unrelated routing changed. The full baseline was checked against the matcher for all RC codes, and every non-RC code group/path was preserved. Usage and viewer were regenerated, and database integrity, foreign keys and unchanged hierarchy passed.

Reference: local lcc-mdsconnect-2016.sqlite3, classification_record/classification_span entries for RC475–RC489.2. Hits count retained Open Library classification uses, not unique books.
