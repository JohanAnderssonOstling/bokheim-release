# Abbreviated decimal LCC endpoints

The incoming range parser now expands decimal-only endpoints using the start’s integer part: T57.6–.97 becomes T57.6–T57.97, and TA418.5–.84 becomes TA418.5–TA418.84. Cutter-only endpoints retain their separate interpretation. Missing numeric starts and reversed intervals are rejected by the span parser; complete containment remains required.

The two reported codes match existing subjects Applied Mathematics (442, 1,489 recorded uses) and Physical properties (6625, 1,441 uses): 2,930 uses in total. No subjects, selectors or schema changes were needed. Unified matcher version is 30; outline matcher version is 4.

Validation: 58 library tests pass, including decimal expansion, abbreviated Cutter preservation, decimal-plus-Cutter endpoints, missing and reversed endpoints, and out-of-range negatives. Five probes pass against the live taxonomy. A full local matching audit regenerated usage counts, the matching cache and viewer; the current database was compared with the audit snapshot before publication. The final parser guard for a missing numeric start affects no observed codes in the local cache.

Working files: /home/johan/.cache/bokheim/lcc-decimal-endpoints/; test log: /home/johan/.cache/bokheim/lcc-decimal-tests.log.
