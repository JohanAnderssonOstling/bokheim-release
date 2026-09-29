#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 /absolute/rich.sqlite.building /absolute/core.sqlite" >&2
    exit 2
fi

building=$1
core=$2
current=/home/johan/data/openlibrary/current.sqlite
release=/var/lib/bokheim-deploy/metadata-incoming
deploy=/usr/local/sbin/bokheim-deploy-metadata

for path in "${building}" "${core}"; do
    [[ ${path} == /* ]] || { echo "paths must be absolute" >&2; exit 2; }
done
[[ -f ${building} && ! -L ${building} ]] || { echo "missing rich build database: ${building}" >&2; exit 1; }

# The core is immutable once published. Re-running the monitor after a crash is
# therefore harmless and must never overwrite a snapshot that may be active.
if [[ -f ${core} ]]; then
    exit 0
fi

while true; do
    ready=$(sqlite3 -readonly -cmd '.timeout 5000' "${building}" \
        "SELECT COUNT(*) FROM import_checkpoint WHERE phase IN ('edition_identities_materialize','work_titles_materialize','ratings','reading_log','authors_materialize') AND completed=1;" 2>/dev/null || true)
    [[ ${ready} == 5 ]] && break
    sleep 30
done

temporary=${core}.building.$$
cleanup() {
    status=$?
    trap - EXIT
    rm -f -- "${temporary}" "${temporary}-wal" "${temporary}-shm"
    exit "${status}"
}
trap cleanup EXIT
[[ ! -e ${temporary} ]] || { echo "temporary core path already exists" >&2; exit 1; }

# SQLite's online backup API includes committed WAL pages and never observes an
# in-flight Wikidata transaction. The source importer may continue throughout.
while ! sqlite3 -readonly -cmd '.timeout 30000' "${building}" ".backup '${temporary}'"; do
    rm -f -- "${temporary}" "${temporary}-wal" "${temporary}-shm"
    sleep 30
done

counts=$(sqlite3 -readonly "${temporary}" \
    "SELECT
       (SELECT records_read FROM import_checkpoint WHERE phase='editions' AND completed=1),
       (SELECT records_read FROM import_checkpoint WHERE phase='works' AND completed=1),
       (SELECT records_read FROM import_checkpoint WHERE phase='authors' AND completed=1);")
IFS='|' read -r edition_count work_count author_count <<<"${counts}"
for count in "${edition_count}" "${work_count}" "${author_count}"; do
    [[ ${count} =~ ^[0-9]+$ ]] || { echo "core snapshot is missing a completed Open Library count" >&2; exit 1; }
done
imported_at_ms=$(( $(date +%s) * 1000 ))

sqlite3 "${temporary}" \
    "BEGIN IMMEDIATE;
     DELETE FROM entity;
     DELETE FROM entity_alias;
     DELETE FROM place_parent;
     DELETE FROM place_country;
     DELETE FROM wikidata_candidate;
     DELETE FROM wikidata_place_candidate;
     DELETE FROM wikidata_instance;
     DELETE FROM wikidata_subclass;
     UPDATE edition_entity_evidence SET wikidata_id=NULL;
     UPDATE work_entity_evidence SET wikidata_id=NULL;
     DROP TABLE IF EXISTS entity_stage;
     DROP TABLE IF EXISTS entity_alias_stage;
     DROP TABLE IF EXISTS place_parent_stage;
     DROP TABLE IF EXISTS place_country_stage;
     DROP TABLE IF EXISTS wikidata_candidate_stage;
     DROP TABLE IF EXISTS wikidata_place_candidate_stage;
     DROP TABLE IF EXISTS wikidata_instance_stage;
     DROP TABLE IF EXISTS wikidata_subclass_stage;
     INSERT OR REPLACE INTO snapshot(singleton,schema_version,dump_date,imported_at_ms,edition_records,work_records,author_records)
       VALUES(1,8,'2026-07-31',${imported_at_ms},${edition_count},${work_count},${author_count});
     INSERT INTO import_checkpoint(phase,records_read,completed) VALUES('snapshot_finalize',0,1)
       ON CONFLICT(phase) DO UPDATE SET records_read=0,completed=1;
     COMMIT;
     VACUUM;
     ANALYZE;
     PRAGMA optimize;
     PRAGMA journal_mode=DELETE;"
integrity=$(sqlite3 -readonly "${temporary}" 'PRAGMA integrity_check;')
[[ ${integrity} == ok ]] || { echo "core snapshot integrity check failed: ${integrity}" >&2; exit 1; }

for required in metadata-server; do
    [[ -f ${release}/${required} && ! -L ${release}/${required} ]] || { echo "missing staged release artifact: ${required}" >&2; exit 1; }
done

verify_log=${core}.verify.log
BOKHEIM_METADATA_DATABASE=${temporary} BOKHEIM_METADATA_BIND=127.0.0.1:18091 \
    "${release}/metadata-server" >"${verify_log}" 2>&1 &
verify_pid=$!
verify_cleanup() {
    kill "${verify_pid}" 2>/dev/null || true
    wait "${verify_pid}" 2>/dev/null || true
}
ready=false
for _ in $(seq 1 30); do
    if curl --fail --silent --max-time 2 http://127.0.0.1:18091/health | grep -Fqx ok; then
        ready=true
        break
    fi
    sleep 1
done
[[ ${ready} == true ]] || { verify_cleanup; echo "staged metadata server could not open core snapshot" >&2; exit 1; }
curl --fail --silent --show-error --max-time 10 \
    -H 'Content-Type: application/json' \
    --data '{"queries":[{"query_id":"core-smoke","title":"Core smoke test","authors":["Bokheim"],"publishers":[],"book_year":2000}]}' \
    http://127.0.0.1:18091/v2/edition-identities >/dev/null
verify_cleanup

mv -- "${temporary}" "${core}"
ln -sfn "${core}" "${current}.next"
mv -Tf "${current}.next" "${current}"
sudo -n "${deploy}"

trap - EXIT
echo "Open Library schema-v8 snapshot activated; schema-v9 Wikidata build continues."
