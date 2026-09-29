# Description and travel below History

The current one-level LCC expansion exposes 74 distinct `Description and
travel` concepts through the History tree. This reflects traditional LCC
national-history schedules (DA-DR, DS, DT, and related classes), not the desired
primary navigation of the unified taxonomy.

## Directly reconcilable destinations

Twelve branches have one exact, unambiguous destination already present below
`Geography, Places & Travel / Travel`:

- Benelux Countries
- Denmark
- Finland
- France
- Germany
- Ireland
- Japan
- Korea
- Norway
- Sweden
- Switzerland
- Turkey

Africa and Asia also have matching Travel destinations, but their labels occur
elsewhere in Geography (for example under cartographic surveys), so any automated
match must be constrained specifically to the Travel route.

All twelve exact matches are reconciled. Their detailed LCC selectors and any
chronological travel children move to the corresponding existing Travel concept,
and the redundant History-side `Description and travel` concepts are removed.
This covers France, Benelux Countries, Denmark, Finland, Germany, Ireland, Japan,
Korea, Norway, Sweden, Switzerland, and Turkey.

## Destinations needing curation

After those moves, 62 History-side concepts remain. Africa and Asia can be
matched only by constraining the destination to the Travel route because those
labels also occur in other Geography branches. The other 60 do not have an exact
destination node in the current Travel tree. They include:

- countries such as Afghanistan, Albania, Bangladesh, Belgium, Brunei, Bulgaria,
  Indonesia, Iran, Iraq, Malaysia, New Zealand, Pakistan, the Philippines,
  Romania, Singapore, Syria, and Thailand;
- historical or renamed places such as East and West Germany, Prussia, Yugoslavia,
  the British Empire, and the Dutch East Indies;
- subnational regions such as England, Wales, Scotland, Central/Northern/Southern
  Italy, Cape Province, KwaZulu-Natal, and Transvaal;
- broad or composite regions such as the Islamic World, Arab countries, the
  Maghrib, Northwest Africa, and the Balkan Peninsula.

These should not be flattened into the nearest continent. The safe follow-up is
to create or identify an appropriate Travel destination, then transfer the LCC
selector and chronological travel children while retaining the historical source
caption only as provenance.
