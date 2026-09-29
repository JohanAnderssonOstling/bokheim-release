# Joined Management and Business ranges

`HD28-70HF4999.2-6182` now separates into the complete intervals `HD28-70` and `HF4999.2-6182`. The detailed local LCC reference names HD28–HD70 Management / Industrial management and HF4999.2–HF6182 Business. The shorter CSV outline starts Business at HF5001, so the joined-code parser previously lacked the lower-bound anchor.

Added both abbreviated and fully qualified spellings of the documented Business span to the existing joined-field recognizer. Each component is matched independently by the existing taxonomy. No selectors or taxonomy rows were changed. The original combined notation remains evidence, and matcher versions were advanced for re-projection.

The complete string accounts for 1,793 classification-row uses in the frozen source cache; the preceding edition audit identified 1,310 editions with this value and no direct matching LCC, direct Dewey value, or matching work-record LCC. These are baseline counts, not a new full-library recount.

Validation: all 164 optimized release library tests passed. Regression checks cover both range spellings, lower-case input, independent matching of both components in the unified and outline taxonomies, and rejection of truncated endpoints, extra digits and incomplete trailing classifications. In particular, the separate 22-use string `HD28-70HF4999.2-618` remains unresolved rather than being silently extended.

Test log: `/tmp/lcc-business-joined-tests.log`.
