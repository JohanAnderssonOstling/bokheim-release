# Library of Congress Classification outline data

`lcc-2024-outline.csv` is derived from the 39 freely downloadable Library of
Congress Classification outline PDFs whose schedule data was selected in May
2024. The source index is maintained by the Library of Congress at:

https://www.loc.gov/aba/publications/FreeLCC/freelcc.html

Only call-number ranges, captions, and their outline relationships are
embedded. The complete classification schedules are not included. “Library of
Congress” and “Library of Congress Classification” identify the source system;
they do not imply endorsement by the Library of Congress.

For taxonomy curation that needs finer structural detail, use the reproducible
MDSConnect MARCXML importer documented in `LCC-MDSCONNECT-2016-NOTICE.md`.

Reviewed correction (2026-09-07): the extracted BG1068–BG1073 row was erroneous. The LC classification reference record 168132 places Sleep. Somnambulism at BF1068–BF1073 under Parapsychology. The CSV now preserves that code, caption and hierarchy; BG must not be registered as a subclass by reimporting the erroneous row. See docs/reports/lcc-outline-bg-correction-20260907.md.
