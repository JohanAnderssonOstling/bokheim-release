# U.S. period completion

Applied migration: `20260916_complete_us_period_assignments.sql`.

## Corrections

- Added fallback ranges for Colonial, Revolutionary, Revolution-to-Civil-War, Civil War, twentieth-century and twenty-first-century subjects. E186, E201, E300, E456/E461, E740 and E895 now resolve to their periods instead of History.
- Consolidated fragmented century fallbacks without restoring individual administration subjects.
- Restored subject 51006 to Military, 1961–2000. Routed the separate diplomatic range to the existing twentieth-century Foreign Relations subject. Kept that whole-century subject out of the late-century branch so its earlier codes do not inherit a misleading date route.
- Added nineteenth-century browse routes for early/middle periods, Mexican War and Civil War, retaining valid alternative ancestry.
- Shortened repeated labels. Kept date qualifiers for topics that also appear under shared biography, political or military parents, where removing them would create ambiguous siblings.
- Replaced the unsupported legacy `E441-453` selector with an explicit range on Slavery, preserving the intended specific destination beneath the new period fallback.

## Source correction

The [full LOC 2025 E–F schedule](https://www.loc.gov/aba/publications/FreeLCC/LCC_E-F2025TEXT.pdf), PDF pages 67–68 and 149–153 (zero-based), supersedes the abbreviated outline used in the earlier review. E351 and its early decimal subdivisions belong to War of 1812 and were correctly assigned already; they have **not** been narrowed away. E840.4 is military/naval/air-force history, with military biography at E840.5; diplomacy is E840–E840.2. The twenty-first-century section begins at E891 and includes E919. No later administration allocations were invented.

## Verification

4,412 production-matcher probes passed, including 50 explicit expected destinations, numeric boundaries, cutter-form examples and BISAC period codes. No previously resolved probe became unresolved. Existing specific destinations were preserved except for the explicitly corrected diplomatic/military subdivisions. SQLite integrity and foreign keys passed, and the full taxonomy loaded successfully with its actual labels.

Snapshots and before/after probe CSVs are in `tmp/us-period-completion-20260916/`. The validation and usage refresh runner verifies the live database against the tested snapshot and refuses to overwrite concurrent taxonomy changes or publish stale counts.

The full usage scan completed (15,676,165 classification occurrences). Unrelated
history ancestry changed concurrently, so the global snapshot report was not
published wholesale. `publish_us_usage.py` verified unchanged code assignments
and all affected U.S. ancestor routes, then published refreshed counts for 65
scoped subjects (including History's direct fallback count), removed obsolete
administration rows, and preserved unrelated usage rows. Viewer regenerated.
The final snapshot passed all 4,412 probes; live ancestry has no cycles.
