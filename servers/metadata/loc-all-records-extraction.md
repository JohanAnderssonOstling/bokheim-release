# LC records without ISBNs

The version-2 full import was started on 2026-09-07 on `192.168.1.68`.
It copies the active Open Library snapshot and rereads all 43 locally verified
2016 Books All MARC parts. It retains every non-deleted record with a control
number, including records without valid ISBNs. Title search indexes only records
with LCC evidence.

Server paths:

- Process manifest: `/home/johan/data/loc-books-2016/extraction-all-records-process.json`
- Progress and final statistics: `/home/johan/data/loc-books-2016/extraction-all-records.log`
- Importer: `/home/johan/data/loc-books-2016/import-loc-lcc-v2.py`
- Output: `/home/johan/data/openlibrary/metadata-with-loc-2016-all-records.sqlite`
- Staging: `metadata-with-loc-2016-all-records.sqlite.building-*/snapshot.sqlite`
- Tested release server binary: `/home/johan/bin/metadata-server-lc-all-records`

The process is detached from the laptop. The output appears only after the
SQLite integrity check succeeds. A staging file or completed individual part
is not a completed import. The log's final JSON reports retained records,
ISBN mappings, and LCC-bearing records without ISBNs.

The new release metadata server is deployed. The live snapshot is currently
`metadata-with-loc-2016.sqlite`, providing the completed ISBN/LCC supplement.
An automatic activation worker is waiting for the expanded snapshot to finish:

- Worker: `/home/johan/data/loc-books-2016/activate-lc-when-ready.py`
- Worker process: `activation-all-records-worker-process.json`
- Worker state: `activation-all-records-pending.json`
- Worker log: `activation-all-records-worker.log`
- Deployment result: `deployment-all-records-status.json`

These state/log names are relative to `/home/johan/data/loc-books-2016`.
The worker checks importer liveness and only uses the atomically published
output. Before deployment it verifies all 43 parts, the OL corpus identity,
importer version, a shadow-server ISBN lookup and a real no-ISBN title/author
lookup. It stops if the active snapshot or staged binary has changed meanwhile.
Activation uses the existing privileged deployment helper, retaining the current
metadata-server binary and provider definitions. Failed post-deployment checks
restore the previous snapshot and binaries. The installed service uses SQLite;
no subject mmap index is configured.

The desktop application has not been replaced by this deployment. A release
rebuild/relaunch is needed to consume the new optional `authority_subjects`
response; old clients safely ignore that protobuf field. Existing clients can
already use the newly active ISBN/LCC supplement. No ISBN or Open Library
identity is synthesized from an authority-only classification result.

Validation: Python importer tests cover ISBN validation, no-ISBN records,
replacement/deletion cleanup, resume and source preservation. Release Rust tests
cover authority agreement/conflicts, title/author checks, candidate overflow,
protobuf round trips, old snapshots, split stores, and enrichment without
assigning an ISBN. The staged release executable also passed a read-only lookup
smoke check on the server.
