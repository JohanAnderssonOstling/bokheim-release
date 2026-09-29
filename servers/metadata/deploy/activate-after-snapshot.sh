#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 5 ]]; then
    echo "usage: $0 /absolute/snapshot.sqlite /absolute/build-log /absolute/data-directory /absolute/build-script /absolute/importer" >&2
    exit 2
fi

snapshot=$1
build_log=$2
building=${snapshot}.building
data_directory=$3
build_script=$4
importer=$5
current=/home/johan/data/openlibrary/current.sqlite
release=/var/lib/bokheim-deploy/metadata-incoming
deploy=/usr/local/sbin/bokheim-deploy-metadata
activate_wikidata=${BOKHEIM_ACTIVATE_WIKIDATA_SNAPSHOT:-false}

for path in "${snapshot}" "${build_log}" "${data_directory}" "${build_script}" "${importer}"; do
    if [[ ${path} != /* ]]; then
        echo "snapshot and build log paths must be absolute" >&2
        exit 2
    fi
done
if [[ ! -x ${build_script} || ! -x ${importer} ]]; then
    echo "build script and importer must be executable" >&2
    exit 1
fi
if [[ ${activate_wikidata} != false && ${activate_wikidata} != true ]]; then
    echo "BOKHEIM_ACTIVATE_WIKIDATA_SNAPSHOT must be true or false" >&2
    exit 2
fi

# A freshly launched build wrapper needs a moment to exec the importer.
sleep 5
restart_failures=0
while [[ ! -f ${snapshot} ]]; do
    if [[ ! -f ${building} ]]; then
        echo "neither completed nor resumable snapshot exists: ${snapshot}" >&2
        exit 1
    fi
    if pgrep -u "$(id -u)" -f "metadata-server[^ ]* import-rich .* ${snapshot}$" >/dev/null; then
        restart_failures=0
        sleep 30
        continue
    fi
    printf '\nautomatic resumable-import restart %s\n' "$(date -Is)" >>"${build_log}"
    if BOKHEIM_METADATA_SERVER_BIN=${importer} "${build_script}" "${data_directory}" "${snapshot}" >>"${build_log}" 2>&1; then
        continue
    fi
    restart_failures=$((restart_failures + 1))
    if (( restart_failures >= 3 )); then
        echo "snapshot importer failed three consecutive automatic resume attempts" >&2
        exit 1
    fi
    sleep 30
done

if ! tail -n 1 "${build_log}" | grep -Fq "created metadata snapshot ${snapshot} ("; then
    echo "snapshot exists but the pinned build did not record successful completion" >&2
    exit 1
fi
if [[ -e ${building} || -e ${snapshot}-wal || -e ${snapshot}-shm ]]; then
    echo "snapshot still has unfinished SQLite build state" >&2
    exit 1
fi

integrity=$(sqlite3 -readonly "${snapshot}" "PRAGMA integrity_check;")
schema=$(sqlite3 -readonly "${snapshot}" "SELECT schema_version FROM snapshot WHERE singleton=1;")
if [[ ${integrity} != ok || ${schema} != 9 ]]; then
    echo "snapshot validation failed (integrity=${integrity}, schema=${schema})" >&2
    exit 1
fi

# Wikidata enrichment is intentionally optional. Complete and validate the
# expensive scan, but do not put it on the production path unless an operator
# explicitly opts in after reviewing its value and operating cost.
if [[ ${activate_wikidata} != true ]]; then
    echo "Rich Wikidata snapshot completed and validated; production activation is deferred."
    exit 0
fi
if [[ ${schema} != 9 ]]; then
    echo "only a schema-v9 rich snapshot may be activated" >&2
    exit 1
fi

for required in metadata-server; do
    if [[ ! -f ${release}/${required} || -L ${release}/${required} ]]; then
        echo "missing or unsafe staged release artifact: ${required}" >&2
        exit 1
    fi
done

# Open the completed file with the exact staged server before changing the
# production symlink. This catches schema/configuration mismatches while the
# existing services remain untouched.
verify_log=${snapshot}.verify.log
BOKHEIM_METADATA_DATABASE=${snapshot} \
BOKHEIM_METADATA_BIND=127.0.0.1:18091 \
"${release}/metadata-server" >"${verify_log}" 2>&1 &
verify_pid=$!
cleanup() {
    kill "${verify_pid}" 2>/dev/null || true
    wait "${verify_pid}" 2>/dev/null || true
}
trap cleanup EXIT
ready=false
for _ in $(seq 1 30); do
    if curl --fail --silent --max-time 2 http://127.0.0.1:18091/health | grep -Fqx ok; then
        ready=true
        break
    fi
    sleep 1
done
if [[ ${ready} != true ]]; then
    echo "staged metadata server could not open the completed snapshot" >&2
    exit 1
fi
curl --fail --silent --show-error --max-time 10 \
    -H 'Content-Type: application/json' \
    --data '{"isbns":["9780821338278"]}' \
    http://127.0.0.1:18091/v2/enrichment >/dev/null
cleanup
trap - EXIT

previous=$(readlink -f "${current}" 2>/dev/null || true)
next=${current}.next
ln -sfn "${snapshot}" "${next}"
mv -Tf "${next}" "${current}"
if ! sudo -n "${deploy}"; then
    if [[ -n ${previous} ]]; then
        ln -sfn "${previous}" "${next}"
        mv -Tf "${next}" "${current}"
    fi
    echo "deployment failed; restored the previous metadata snapshot link" >&2
    exit 1
fi

echo "Rich metadata snapshot and staged metadata release activated successfully."
