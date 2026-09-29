# General subject naming pass

Status: complete.

{
  "status": "complete",
  "counts": {
    "subjects": 14001,
    "over40": 0,
    "over30": 1931,
    "under24": 8998,
    "max_length": 40
  },
  "rename_events": 2762,
  "unique_renamed_subjects": 2756,
  "missing_aliases": 0,
  "lost_years": 0
}

Labels were shortened using clear umbrella terms and given subject context where needed. National historical periods omit the country being periodized while retaining existing dates and period descriptions. Foreign rulers, occupations, named civilizations and historical polities remain where they identify the period. Existing date-only period labels retain their dates; this naming pass does not invent historical descriptions.

Each applied batch passed the full production matcher before installation. Selectors, parent links and taxonomy metadata were compared exactly with each batch baseline and left unchanged by this pass. Original preferred labels are retained as aliases. Final alias, year-token, foreign-key and database integrity checks passed. Usage paths, matcher cache and taxonomy viewer were refreshed after the final batch.

The 24–30-character preference is a target, not a reason to omit necessary scope: some standalone labels remain up to 40 characters. The CSV records each rename event, including subsequent refinements of a label.
