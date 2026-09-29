#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
    echo "usage: $0 DATA_DIRECTORY OUTPUT.sqlite [MAX_RECORDS]" >&2
    exit 2
fi

data_directory=$1
output=$2
record_limit=${3:-}
manifest=${data_directory}/rich-snapshot.sources
minimum_free_bytes=${BOKHEIM_METADATA_MIN_FREE_BYTES:-300000000000}

mkdir -p -- "${data_directory}"

if [[ ! -f ${manifest} ]]; then
    editions_url=$(curl --fail --silent --show-error --location --head --output /dev/null --write-out '%{url_effective}' https://openlibrary.org/data/ol_dump_editions_latest.txt.gz)
    works_url=$(curl --fail --silent --show-error --location --head --output /dev/null --write-out '%{url_effective}' https://openlibrary.org/data/ol_dump_works_latest.txt.gz)
    authors_url=$(curl --fail --silent --show-error --location --head --output /dev/null --write-out '%{url_effective}' https://openlibrary.org/data/ol_dump_authors_latest.txt.gz)
    ratings_url=$(curl --fail --silent --show-error --location --head --output /dev/null --write-out '%{url_effective}' https://openlibrary.org/data/ol_dump_ratings_latest.txt.gz)
    reading_log_url=$(curl --fail --silent --show-error --location --head --output /dev/null --write-out '%{url_effective}' https://openlibrary.org/data/ol_dump_reading-log_latest.txt.gz)
    wikidata_url=https://dumps.wikimedia.org/wikidatawiki/entities/latest-all.json.bz2
    wikidata_etag=$(curl --fail --silent --show-error --head "${wikidata_url}" | sed -n 's/^[Ee][Tt][Aa][Gg]:[[:space:]]*//p' | tr -d '\r')
    if [[ ! ${wikidata_etag} =~ ^\"[A-Za-z0-9._+-]+\"$ ]]; then
        echo "Wikidata did not provide a usable strong ETag" >&2
        exit 1
    fi
    {
        printf 'EDITIONS_URL=%s\n' "${editions_url}"
        printf 'WORKS_URL=%s\n' "${works_url}"
        printf 'AUTHORS_URL=%s\n' "${authors_url}"
        printf 'RATINGS_URL=%s\n' "${ratings_url}"
        printf 'READING_LOG_URL=%s\n' "${reading_log_url}"
        printf 'WIKIDATA_URL=%s\n' "${wikidata_url}"
        printf 'WIKIDATA_ETAG=%s\n' "${wikidata_etag}"
    } >"${manifest}"
fi

# Reject malformed or duplicate entries before reading values without eval.
if grep -Ev '^(EDITIONS_URL|WORKS_URL|AUTHORS_URL|RATINGS_URL|READING_LOG_URL|WIKIDATA_URL)=https://[A-Za-z0-9._~:/?&=%+-]+$|^WIKIDATA_ETAG="[A-Za-z0-9._+-]+"$' "${manifest}" | grep -q .; then
    echo "invalid source manifest: ${manifest}" >&2
    exit 1
fi
manifest_value() {
    local name=$1
    local value
    value=$(sed -n "s/^${name}=//p" "${manifest}")
    if [[ -z ${value} || $(grep -c "^${name}=" "${manifest}") -ne 1 ]]; then
        echo "missing or duplicate ${name} in ${manifest}" >&2
        exit 1
    fi
    printf '%s' "${value}"
}
EDITIONS_URL=$(manifest_value EDITIONS_URL)
WORKS_URL=$(manifest_value WORKS_URL)
AUTHORS_URL=$(manifest_value AUTHORS_URL)
RATINGS_URL=$(manifest_value RATINGS_URL)
READING_LOG_URL=$(manifest_value READING_LOG_URL)
WIKIDATA_URL=$(manifest_value WIKIDATA_URL)
WIKIDATA_ETAG=$(manifest_value WIKIDATA_ETAG)

available_bytes=$(df --output=avail -B1 "${data_directory}" | tail -n 1 | tr -d ' ')
if (( available_bytes < minimum_free_bytes )); then
    echo "at least ${minimum_free_bytes} free bytes are required; ${available_bytes} are available" >&2
    exit 1
fi

download() {
    local url=$1
    local destination=$2
    local etag=${3:-}
    if [[ -f ${destination} ]]; then
        return
    fi
    local headers=()
    if [[ -n ${etag} ]]; then
        headers=(--header "If-Match: ${etag}")
    fi
    curl --fail --location --retry 20 --retry-all-errors --continue-at - "${headers[@]}" --output "${destination}.part" "${url}"
    mv -- "${destination}.part" "${destination}"
}

editions=${data_directory}/$(basename "${EDITIONS_URL}")
works=${data_directory}/$(basename "${WORKS_URL}")
authors=${data_directory}/$(basename "${AUTHORS_URL}")
ratings=${data_directory}/$(basename "${RATINGS_URL}")
reading_log=${data_directory}/$(basename "${READING_LOG_URL}")
wikidata_etag_id=${WIKIDATA_ETAG//\"/}
wikidata=${data_directory}/wikidata-all-${wikidata_etag_id}.json.bz2

download "${EDITIONS_URL}" "${editions}"
download "${WORKS_URL}" "${works}"
download "${AUTHORS_URL}" "${authors}"
download "${RATINGS_URL}" "${ratings}"
download "${READING_LOG_URL}" "${reading_log}"
download "${WIKIDATA_URL}" "${wikidata}" "${WIKIDATA_ETAG}"

metadata_server_bin=${BOKHEIM_METADATA_SERVER_BIN:-}
if [[ -z ${metadata_server_bin} ]]; then
    cargo build --release -p metadata-server
    metadata_server_bin=target/release/metadata-server
fi
if [[ ! -x ${metadata_server_bin} ]]; then
    echo "metadata server binary is not executable: ${metadata_server_bin}" >&2
    exit 1
fi

arguments=(import-rich "${editions}" "${works}" "${authors}" "${wikidata}" "${ratings}" "${reading_log}" "${output}")
if [[ -n ${record_limit} ]]; then
    arguments+=("${record_limit}")
fi

# A completed dump-specific index makes Wikidata restarts seek directly to the
# committed record. Until the atomic index file appears, the importer keeps its
# normal sequential fallback.
wikidata_index=${wikidata%.json.bz2}.seek-index.json
wikidata_seeker=/home/johan/bin/index-wikidata.py
indexed_bzip2_runtime=/home/johan/lib/indexed-bzip2-1.7.0
if [[ -f ${wikidata_index} && -x ${wikidata_seeker} && -d ${indexed_bzip2_runtime} ]]; then
    export BOKHEIM_WIKIDATA_SEEKER=${wikidata_seeker}
    export BOKHEIM_WIKIDATA_SEEK_INDEX=${wikidata_index}
    export PYTHONPATH=${indexed_bzip2_runtime}${PYTHONPATH:+:${PYTHONPATH}}
fi
exec "${metadata_server_bin}" "${arguments[@]}"
