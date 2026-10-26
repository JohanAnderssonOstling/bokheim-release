#!/usr/bin/env bash
set -euo pipefail

TARGET=${1:?Usage: verify-glibc-compatibility.sh FILE_OR_DIRECTORY [MAX_GLIBC_VERSION] [--exclude FILE]}
MAXIMUM_VERSION=${2:-2.17}
EXCLUDED_FILE=
if test "$#" -gt 2; then
  if test "$#" -ne 4 || test "$3" != --exclude; then
    printf 'Expected --exclude FILE after the maximum glibc version\n' >&2
    exit 1
  fi
  EXCLUDED_FILE=$(realpath -m -- "$4")
fi

test -e "$TARGET"
TARGET=$(realpath -- "$TARGET")
command -v file >/dev/null
command -v readelf >/dev/null

declare -a ELF_FILES=()
if test -d "$TARGET"; then
  while IFS= read -r -d '' candidate; do
    test "$candidate" != "$EXCLUDED_FILE" || continue
    if file --brief "$candidate" | grep -q '^ELF '; then
      ELF_FILES+=("$candidate")
    fi
  done < <(find "$TARGET" -type f -print0)
elif file --brief "$TARGET" | grep -q '^ELF '; then
  ELF_FILES+=("$TARGET")
fi

test "${#ELF_FILES[@]}" -gt 0

highest_version=0
highest_file=
for elf_file in "${ELF_FILES[@]}"; do
  version_info=$(readelf --version-info --wide "$elf_file")
  # This nonnumeric ABI tag requires a 2.36+ loader; numeric-only scans miss it.
  if [[ "$version_info" == *GLIBC_ABI_DT_RELR* ]]; then
    version_info+=$'\nGLIBC_2.36'
  fi
  unknown_versions=$(printf '%s\n' "$version_info" | grep -oE 'GLIBC_[A-Za-z0-9_.]+' | grep -vE '^GLIBC_([0-9]+(\.[0-9]+)*|ABI_DT_RELR)$' || true)
  if test -n "$unknown_versions"; then
    printf 'ERROR: %s has unsupported glibc ABI requirements: %s\n' "$elf_file" "$unknown_versions" >&2
    exit 1
  fi
  while IFS= read -r required_version; do
    test -n "$required_version" || continue
    if test "$(printf '%s\n%s\n' "$highest_version" "$required_version" | sort --version-sort | tail -n 1)" = "$required_version"; then
      highest_version=$required_version
      highest_file=$elf_file
    fi
    if test "$(printf '%s\n%s\n' "$MAXIMUM_VERSION" "$required_version" | sort --version-sort | tail -n 1)" = "$required_version" && test "$required_version" != "$MAXIMUM_VERSION"; then
      printf 'ERROR: %s requires GLIBC_%s, newer than supported GLIBC_%s\n' "$elf_file" "$required_version" "$MAXIMUM_VERSION" >&2
      exit 1
    fi
  done < <(printf '%s\n' "$version_info" | sed -n 's/.*GLIBC_\([0-9][0-9.]*\).*/\1/p' | sort --version-sort --unique)
done

printf 'GLIBC compatibility verified: %s ELF files, highest requirement GLIBC_%s (%s)\n' \
  "${#ELF_FILES[@]}" "$highest_version" "${highest_file:-none}"
