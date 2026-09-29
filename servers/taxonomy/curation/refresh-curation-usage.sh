#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../../.." && pwd)
taxonomy="$root/shared/subject-projection/data/unified-taxonomy-v2.sqlite3"
usage="$root/shared/subject-projection/data/taxonomy-usage-openlibrary-2026-07-31.tsv"
cache_directory="${XDG_CACHE_HOME:-$HOME/.cache}/bokheim"
code_usage="${BOKHEIM_CODE_USAGE_DATABASE:-$cache_directory/openlibrary-code-usage.sqlite3}"
matcher="$root/target/release/taxonomy_usage"
mkdir -p "$cache_directory"
matches="${BOKHEIM_CODE_MATCHES_OUTPUT:-$cache_directory/taxonomy-code-matches.tsv}"

if [[ ! -f "$code_usage" ]]; then
  echo "missing $code_usage; run servers/taxonomy/curation/rebuild-code-usage.py SOURCE.sqlite CACHE.sqlite3 first" >&2
  exit 1
fi
if [[ ! -x "$matcher" ]]; then
  echo "missing $matcher; build the standalone audit example once" >&2
  exit 1
fi

usage_staging=$(mktemp "${usage}.building.XXXXXX")
match_staging=$(mktemp "${matches}.building.XXXXXX")
trap 'rm -f -- "$usage_staging" "$match_staging"' EXIT
"$matcher" "$code_usage" "$taxonomy" "$usage_staging" "$match_staging" >"$cache_directory/taxonomy-curation-audit.log"
chmod 644 "$usage_staging"
mv -- "$usage_staging" "$usage"
mv -- "$match_staging" "$matches"
python3 "$root/servers/taxonomy/curation/generate-viewer.py"
