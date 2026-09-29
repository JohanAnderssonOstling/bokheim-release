# Bokheim taxonomy server

Read-only, unauthenticated HTTP access to the curated Bokheim unified taxonomy.
The service never imports raw LCC, DDC, or BISAC data: its only served state is
the reviewed `unified-taxonomy-v2.sqlite3` snapshot supplied at startup.

## Run

```sh
BOKHEIM_TAXONOMY_DATABASE=shared/subject-projection/data/unified-taxonomy-v2.sqlite3 \
  cargo run -p taxonomy-server
```

The default listener is `127.0.0.1:8093`. Set `BOKHEIM_TAXONOMY_BIND` to change
it. A non-loopback plain-HTTP listener additionally requires
`BOKHEIM_TAXONOMY_ALLOW_PUBLIC_HTTP=true`; production should normally keep the
listener on loopback and terminate HTTPS in a reverse proxy.

No route accepts or requires credentials. Browser clients may access the public
API through permissive CORS.

## Deploy

Provision the dedicated, root-owned deployer once on the server:

```sh
sudo servers/taxonomy/deploy/provision-taxonomy-deploy.sh johan
```

Routine releases are then built, staged, atomically activated, health checked,
and rolled back on failure with:

```sh
servers/taxonomy/deploy/deploy-remote.sh
```

The deployment installs the authoritative snapshot read-only, runs the service
on `127.0.0.1:8093`, and exposes it through Caddy at
`https://taxonomy.bokheim.se`.

## API

- `GET /health`
- `GET` or `HEAD /v1/taxonomy` — monotonically increasing release identifier and snapshot counts; supports `If-None-Match`
- `GET /v1/roots` — initial visible roots
- `POST /v1/resolve` — batch resolution for all locally unsupported codes
- `GET /v1/resolve?system=lcc&code=DC256` — single-code diagnostic lookup
- `GET /v1/concepts/{concept_id}` — one concept and its ancestor closure
- `GET /v1/concepts/{concept_id}/children` — immediate children and required ancestors
- `GET /v1/snapshot` — complete immutable curated SQLite snapshot

Slices are ordered with parents before descendants so a client can transactionally
merge them into its local taxonomy. `ETag` identifies the exact taxonomy release;
the snapshot endpoint supports `If-None-Match` and long-lived immutable caching.
The deployment installer allocates the next release number while holding its
deployment lock; rebuilding the same source tree does not create a release.

The production request is one bounded JSON batch for normal assignment operations:

```json
{"codes":[{"system":"lcc","code":"HF5601"},{"system":"ddc","code":"515"}]}
```

The response contains one result mapping per unique code and a single
deduplicated ancestor-closed concept union. Codes already supported by the
client must be filtered locally and are not sent.
Operations containing more than 512 unique missing codes, or whose encoded JSON
would exceed 64 KiB, are split into consecutive bounded requests. The client
combines them only when every response has the same taxonomy release ID.

The native client records classifications in `book_subject_code` before it
attempts unified assignment. Local-file imports, authority imports, and
multi-book subject enrichment then collect the codes absent from the installed
taxonomy and make one request for the operation. Returned concepts are merged
transactionally into the device-local SQLite taxonomy, the in-process matcher
is replaced, and every open library recalculates its assignments. A network
failure never rolls back book metadata; the recorded codes remain available
for a later hydration attempt.

The browser client follows the same policy in its worker: exact local coverage
is checked in WASM, misses are batched, and returned concepts plus positive or
negative resolutions are persisted in browser SQLite. The overlay is restored
into WASM on demand. Rich metadata and remotely synchronized metadata both
trigger hydration, while taxonomy downtime never blocks metadata storage or
core library synchronization. A new server release invalidates old resolution
rows so previously negative codes can be checked again.

Native and browser clients bundle only an ancestor-closed structural seed: all
roots, their direct children, and second-level children. They retain the
server's monotonically increasing release ID and check `/v1/taxonomy` at startup/use,
then at most once every six hours. The bodyless conditional request normally
returns `304 Not Modified`. A changed ETag discards the rehydratable sparse
overlay and negative-resolution ledger, restores the bundled seed, and lets
normal code hydration refill only the concepts that device uses.

## Curation

Selector maintenance lives in [`curation/`](curation): the propose/preview/apply
scripts, coverage audits, viewer generators, and the `taxonomy-curation` bins
(moved from `shared/subject-projection`, which now holds only the matcher
library). See [`curation/SELECTOR-MAINTENANCE.md`](curation/SELECTOR-MAINTENANCE.md)
for the workflow.
