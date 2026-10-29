# Re-encode existing book sync payloads

Protocol 31 packs PDF page geometry into numeric columns and PDF dependencies
into a range dictionary with packed references. TOCs use typed recursive
messages containing only titles, targets, and children, without repeated field
names or empty child lists. Book records use typed fields for titles,
contributors, bibliographic attributes, and descriptions; contributor UUIDs
use their 16-byte representation. Synchronization and enrichment use the same
record codec, with unchanged JSON and persisted layouts. The transport also merges the historical second
enrichment record into the book's existing record. Native and web use
the same merge and encoding. Historical state envelopes from protocols 28–30
and existing bincode additions remain readable by the new client. New additions
store only finalized metadata and the enrichment outcome, navigation, cover,
and popularity. The appended storage variant retains old variant numbering.

Delivery addresses and winning versions also use shared typed transport
messages, with binary replica UUIDs. This bookkeeping is encoded by both the
relay and domain clients; it does not interpret domain payloads or change their
addresses, versions, or persisted bytes. Old delivery representations remain
readable. It needs the coordinated client/server rollout, but no database
backfill: the HTTP encoder produces it when sending any stored state.

The relay deliberately stores opaque client payloads. Updating its outer HTTP
codec does not compact previously stored book additions. This offline tool uses
the shared client model to re-encode those values without putting client domain
dependencies in the server. It checks stored addresses, validates each state,
and checks a decoded round trip before producing a replacement. It emits only
book rows whose bytes change. Repeating it is safe.

Coordinate the protocol-31 client/server rollout before applying replacements.
Existing protocol-30 clients cannot decode protocol-31 state or HTTP envelopes.
No cursor, winning version, or library generation needs to change: the book
state itself is identical. Returning clients already hold that state; newly
downloaded payloads benefit from the compact representation.

Build the tool with the normal local Cargo target directory:

```sh
cargo build -p library-replica --example compact-sync-payloads
```

Using the database owner's `psql` session, export the four columns below with
`COPY (...) TO STDOUT` and pipe them through the tool. Its output has five
tab-separated columns: the address, previous bytes, and replacement bytes.

```sql
COPY (
  SELECT library_id, kind, encode(entity_key,'hex'), encode(value,'hex')
  FROM sync_state
  WHERE kind IN ('book_add','external_book') AND entity_subkey=''
) TO STDOUT;
```

Load the output into a temporary staging table in the update session:

```sql
CREATE TEMP TABLE compact_book_state (
  library_id text NOT NULL, kind text NOT NULL, entity_key_hex text NOT NULL,
  previous_hex text NOT NULL, replacement_hex text NOT NULL,
  PRIMARY KEY(library_id,kind,entity_key_hex)
);
-- COPY compact_book_state FROM STDIN using the tool's output.
```

Apply the conditional update in one transaction. The format-only rewrite must
bypass the first-write-wins guard; ordinary sync publications retain that guard.
The table lock prevents sync writers from running while it is disabled. The
previous-bytes predicate skips values changed since the export.

```sql
BEGIN;
LOCK TABLE sync_state IN EXCLUSIVE MODE;
ALTER TABLE sync_state DISABLE TRIGGER lww_guard;
UPDATE sync_state AS state
SET value=decode(compact.replacement_hex,'hex')
FROM compact_book_state AS compact
WHERE state.library_id=compact.library_id AND state.kind=compact.kind
  AND state.entity_key=decode(compact.entity_key_hex,'hex')
  AND state.entity_subkey=''
  AND state.value=decode(compact.previous_hex,'hex');
ALTER TABLE sync_state ENABLE TRIGGER lww_guard;
COMMIT;
```

Byte-range indexes still accompany synced PDF metadata. Loading them on demand
is separate: the current server has no index-only endpoint. That change needs
a reader-only resource and local cache, including a path for existing embedded
indexes. The packed representation can also be used by that resource.
