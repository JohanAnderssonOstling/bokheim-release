#!/usr/bin/env bash
set -euo pipefail

TARGET=${1:?Usage: verify-glibc-compatibility.sh FILE_OR_DIRECTORY [MAX_GLIBC_VERSION]}
MAXIMUM_VERSION=${2:-2.35}

test -e "$TARGET"
command -v file >/dev/null
command -v readelf >/dev/null

declare -a ELF_FILES=()
if test -d "$TARGET"; then
  while IFS= read -r -d '' candidate; do
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
  done < <(readelf --version-info --wide "$elf_file" 2>/dev/null | sed -n 's/.*GLIBC_\([0-9][0-9.]*\).*/\1/p' | sort --version-sort --unique)
done

printf 'GLIBC compatibility verified: %s ELF files, highest requirement GLIBC_%s (%s)\n' \
  "${#ELF_FILES[@]}" "$highest_version" "${highest_file:-none}"
