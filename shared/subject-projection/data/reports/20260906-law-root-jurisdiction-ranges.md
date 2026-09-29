# Law root audit and jurisdiction range consolidation

Curated selectors: 49,946 → 43,262, net reduction 6,684. Law root: 9,404 → 2,716 direct selectors; final direct classification uses 327,136.

Consolidated reviewed jurisdiction-wide blocks into numeric ranges. General jurisdiction material remains on Law; existing more specific legal topics continue to win. KJ1..KJ4999 (European history of law) moved to existing Legal History & Roman Law (5878). No new subjects or hierarchy changes.

Corrected three non-law selectors: DT620.15 (gazetteers/dictionaries) and DT620.9 (local history) to Equatorial Guinea history (10723); DT620.27 (description/travel) to Travel / Africa / Central (2428). These changed five observed variants / ten classification uses.

| Prefix | Reviewed range | Destination |
|---|---|---|
| KVJ | KVJ1..KVJ2998 | Law |
| KK | KK1..KK9799.33 | Law |
| KJV | KJV1..KJV9158.2 | Law |
| KFX | KFX1004..KFX2593 | Law |
| KNN | KNN1..KNN9000.2 | Law |
| KJW | KJW50..KJW4550 | Law |
| KJW | KJW5200..KJW9600 | Law |
| KD | KD1..KD9500.24 | Law |
| KNU | KNU0..KNU8999 | Law |
| KFN | KFN1..KFN599 | Law |
| KFN | KFN601..KFN1199 | Law |
| KFN | KFN1201..KFN1799 | Law |
| KFN | KFN1801..KFN2399 | Law |
| KFN | KFN3601..KFN4199 | Law |
| KFN | KFN5001..KFN6199.5 | Law |
| KFN | KFN7401..KFN7999 | Law |
| KFN | KFN8601..KFN9199 | Law |
| KFC | KFC1..KFC1199.5 | Law |
| KFC | KFC1801..KFC2399 | Law |
| KFC | KFC3601..KFC4199 | Law |
| KNT | KNT1..KNT9999 | Law |
| KF | KF1..KF9827 | Law |
| KPH | KPH1..KPH9999 | Law |
| KE | KE1..KE9450 | Law |
| KNQ | KNQ0..KNQ9665 | Law |
| KLA | KLA0..KLA9999 | Law |
| KEQ | KEQ1..KEQ1199.5 | Law |
| KFP | KFP1..KFP599 | Law |
| KKA | KKA1..KKA9799 | Law |
| KJ | KJ1..KJ4999 | Legal History & Roman Law |
| KEO | KEO1..KEO1199.5 | Law |
| KGN | KGN0..KGN9800 | Law |
| KKR | KKR1..KKR4999 | Law |

Audited all 170,486 observed K* and DT codes using actual before/after matcher runs on frozen snapshots. 146 variants / 654 uses moved from Law into the approved history/travel destinations. 5,461 previously unmatched variants / 29,331 uses gained coverage inside the reviewed ranges. All other existing assignments are unchanged. No lost matches or unexpected changes; no exception restoration was needed. Classification uses are not unique books.

Ranges use the local LCC jurisdiction headings, preserving distinct state/regional intervals and their gaps. Did not copy suspicious concatenated numeric bounds from other source records (for example outlier spans whose endpoints appear to contain filing/table digits). Unreviewed or out-of-range selectors remain specific; the root still has 2,716 selectors for subsequent review.

Guarded every live taxonomy table against the baseline, compared installed data with the staged result, and checked integrity and foreign keys. Rebuilt the full code-match cache, usage counts and viewer. Reference: lcc-mdsconnect-2016.sqlite3 classification records and master-taxonomy-v2.sqlite3 for the misplaced DT codes. Evidence: client/.test-tmp/law-root-ranges/.

Concurrent history edits caused two guarded installation attempts to stop before writing. Those edits were preserved. A final refreshed audit and installation ran under a database write reservation, keeping the baseline stable until the verified migration committed.
