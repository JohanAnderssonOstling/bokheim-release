#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 12 ]]; then
    echo "usage: $0 INDEXER_PID DUMP.bz2 INDEX.json RECORD SEEKER PYTHONPATH ACTIVATION SNAPSHOT BUILD_LOG DATA_DIRECTORY BUILD_SCRIPT IMPORTER" >&2
    exit 2
fi

indexer_pid=$1
source=$2
index=$3
record=$4
seeker=$5
python_path=$6
activation=$7
snapshot=$8
build_log=$9
data_directory=${10}
build_script=${11}
importer=${12}

if [[ ! ${indexer_pid} =~ ^[1-9][0-9]*$ || ! ${record} =~ ^[0-9]+$ ]]; then
    echo "indexer PID and record must be numeric" >&2
    exit 2
fi
for path in "${source}" "${index}" "${seeker}" "${python_path}" "${activation}" "${snapshot}" "${build_log}" "${data_directory}" "${build_script}" "${importer}"; do
    if [[ ${path} != /* ]]; then
        echo "all paths must be absolute" >&2
        exit 2
    fi
done
if [[ ! -f ${source} || ! -x ${seeker} || ! -d ${python_path} || ! -x ${activation} || ! -x ${build_script} || ! -x ${importer} ]]; then
    echo "missing index handoff dependency" >&2
    exit 1
fi

while [[ ! -f ${index} ]]; do
    if ! kill -0 "${indexer_pid}" 2>/dev/null; then
        echo "Wikidata indexer exited without publishing ${index}" >&2
        exit 1
    fi
    sleep 30
done

PYTHONPATH=${python_path}${PYTHONPATH:+:${PYTHONPATH}} \
    "${seeker}" verify "${source}" "${index}" "${record}" --threads 2

echo "Wikidata seek index validated; resuming ingestion at record ${record}."
exec "${activation}" "${snapshot}" "${build_log}" "${data_directory}" "${build_script}" "${importer}"
