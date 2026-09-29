# National chronological compaction

Implemented the requested review and compaction of France, Germany, Russia and China. Eighteen subject merges transfer their classification assignments intact to broader eras.

| Branch | Compaction | Retained distinctions |
|---|---|---|
| France | Five dated presidential subjects, de Gaulle through Chirac, into Fifth Republic | The republics, empires, monarchies, occupation/liberation, revolution/Napoleon and Hundred Years’ War |
| Germany | Four medieval dynastic periods into Early & Medieval; Early and Late DDR into Democratic Republic | Frankish Era; Reformation, Enlightenment and other substantial early-modern phases; Thirty Years’ War; Empire, Weimar, Nazi Era, occupation, the two German republics and reunification |
| Russia | Nineteenth-century and Nicholas II periods into Romanov Era; Early Soviet, Stalin Period and 1953–1985 into Soviet Era | Romanov versus Soviet periods; Revolution & Civil War; Perestroika & Breakup; post-Soviet Federation |
| China | 1912–1928 and 1928–1937 into Republican China | Existing broad ancient/dynastic periods, Qing, the 1937–1945 and 1945–1949 wartime phases, Cultural Revolution and the main post-1949 eras |

The two later Chinese Republican phases remain because their code intervals include distinct wars: the local reference places the Sino-Japanese War at DS777.53 and Civil War at DS777.54. The retained phase subjects include surrounding general history, so they were not relabeled as exclusively military events. There are no individual emperor children in the inspected Chinese dynasty branches to remove.

The dated French presidency subjects have LCC DC420–DC424 ranges whose reference captions describe life and administration. These mixed presidency/biography subjects are merged as approved administration-level detail; their European Biographies links disappear with them. Fifth Republic does not inherit that biography parent. Standalone biography topics such as Hitler, Stalin, Trotsky, Putin, Mao, Deng and Zhou remain. Stalin is promoted to the surviving Soviet Era subject when its narrower period is removed.

The data changes and viewer structure are applied. Post-write inspection confirms all 18 merged IDs are gone, all explicitly retained subjects remain, and the surviving branches have the intended children. **Classification validation, tests and usage-count refresh remain deferred at the user's request.** Displayed usage figures are from the last completed refresh.

Pending end-of-batch validation includes:

- `20260920_english_era_compaction.sql`
- `20260920_history_period_compaction.sql`
- `20260920_national_chronology_compaction.sql`

The frozen database before these deferred batches is `/tmp/taxonomy-before-english-era-compaction.sqlite3`. This phase also has `/tmp/taxonomy-before-national-chronologies.sqlite3` for an isolated comparison.

Artifacts: migration, [exact merge and preservation manifest](20260920-national-chronology-compaction.json).
