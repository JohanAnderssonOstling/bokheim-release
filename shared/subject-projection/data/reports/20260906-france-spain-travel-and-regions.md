# France and Spain: travel separation and regional history

Created 13 direct regional history children. Moved 6,450 direct classification uses across 449 normalized codes from History to the existing Travel branches, and 11,573 uses across 906 codes to the regional history children.

Travel destinations: Geography, Places & Travel / Travel / Europe / France; and the existing Spain & Portugal travel subject. Travel-specific guidebooks and description/travel codes take precedence over the regional history extraction. Existing travel classifications remain intact.

| Country | Regional history child | Hits moved | Codes moved | LCC grouping before travel exclusions |
|---|---|---:|---:|---|
| France | Paris | 1,960 | 259 | DC701–DC790 |
| France | Provence | 305 | 45 | DC611.P951–DC611.P979 |
| France | Brittany | 611 | 46 | DC611.B841–DC611.B9173 |
| France | Corsica | 461 | 31 | DC611.C8–DC611.C8365 |
| Spain | Catalonia | 3,078 | 185 | DP302.C57–DP302.C69 |
| Spain | Basque Provinces | 1,181 | 76 | DP302.B41–DP302.B55 |
| Spain | Andalusia | 831 | 57 | DP302.A41–DP302.A55 |
| Spain | Galicia | 716 | 32 | DP302.G11–DP302.G2 |
| Spain | Balearic Islands | 593 | 33 | DP302.B16–DP302.B3 |
| Spain | Canary Islands | 463 | 36 | DP302.C36–DP302.C51 |
| Spain | Barcelona | 533 | 43 | DP402.B2–DP402.B3 |
| Spain | Madrid | 397 | 29 | DP350–DP374 |
| Spain | Valencia (province) | 444 | 34 | DP302.V11–DP302.V25 |

The 13 approved regions/cities were retained even where removing travel reduced the history total below 500 hits. LCC historical regions/provinces are used, not a uniform modern administrative map.

## Travel moved

| Destination | Hits moved | Codes moved |
|---|---:|---:|
| Travel: France | 3,705 | 296 |
| Travel: Spain | 2,745 | 153 |

## Mixed history/travel codes

27 codes covering 512 direct uses have LCC captions that explicitly combine history and travel. They were retained for review; it is not possible to separate their books by code alone. No claim is made that every travel book has been removed from History. Book-level metadata review is needed for these mixed codes.

| Country | Code | Hits |
|---|---|---:|
| France | DC611.A556 | 100 |
| France | DC611.I27 | 79 |
| France | DC801.N72 | 57 |
| France | DC611.C52 | 49 |
| France | DC611.V357 | 26 |
| France | DC611.I3 | 24 |
| France | DC801.C24 | 21 |
| France | DC611.T184 | 19 |
| France | DC611.R282 | 18 |
| Spain | DP27.5 | 18 |
| France | DC611.Y56 | 16 |
| France | DC801.R36 | 16 |
| France | DC611.B752 | 14 |
| France | DC801.L49 | 12 |
| France | DC801.T87 | 12 |
| France | DC611.A299 | 11 |
| France | DC611.L831 | 4 |
| France | DC611.S327 | 3 |
| France | DC611.S46 | 3 |
| France | DC801N72 | 3 |
| France | DC611.A556B74 | 1 |
| France | DC611.C52B88 | 1 |
| France | DC611.I27B613 | 1 |
| France | DC611.V357N67 | 1 |
| France | DC611A556 | 1 |
| France | DC801.N72B27 | 1 |
| France | DC801.R36G34 | 1 |

## Implementation and verification

- Existing exact selectors were transferred to their new owners, with explicit observed descendant codes added where necessary. Cutter ranges were expanded into explicit selectors: the matcher does not safely interpret a Cutter interval as a geographic range.
- Broad country selectors remain as fallbacks for other places and unresolved material. Future imports can require additional exact coverage.
- Local LCC reference: classification_record/classification_span in lcc-mdsconnect-2016.sqlite3. Travel rules require explicit guidebook/travel captions and exclude captions mentioning history. Madrid uses DP350–DP374; Paris uses DC701–DC790.
- The real taxonomy matcher evaluated all 18,400 imported DC/DP codes before and after. Changes are solely from the two country history parents to the approved region or travel destinations. Existing concept paths and all non-DC/DP selectors are unchanged.
- Cached baseline DC/DP assignments were verified against the real matcher. All other cached code groups and paths were preserved. Database integrity, foreign keys, final usage and viewer were verified.
