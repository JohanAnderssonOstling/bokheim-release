#!/bin/sh
# Rebuild the committed Kobo package. The ARMv7 musl cross toolchain is only
# available from musl.cc, a single volunteer-run host that is regularly
# unreachable, so the package is cross-compiled here and committed to the
# repository instead of being built or downloaded by CI. The installer
# workflow embeds whatever this script last committed, so run it whenever the
# Kobo application changes.
#
#   apps/desktop-gpui/scripts/refresh-kobo-package.sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)
COMMITTED="$REPO_ROOT/apps/kobo-installer/package/Bokheim-Kobo-Libra2-armv7.tar.gz"

cd "$REPO_ROOT"

KOBO_CPU_PROFILE=${KOBO_CPU_PROFILE:-libra2}
export KOBO_CPU_PROFILE
ARCHIVE=$("$SCRIPT_DIR/package-kobo.sh")
test -s "$ARCHIVE"

# A package missing the launcher or holding a host-architecture binary bricks
# nothing, but it wastes a release and a device round trip to discover, and
# committing one puts it in the repository's history for good.
listing=$(tar -tzf "$ARCHIVE")
for required in \
    ./.adds/bokheim/desktop-gpui-kobo \
    ./.adds/bokheim/fbink \
    ./.adds/bokheim/run.sh \
    ./.adds/bokheim/run-monitored.sh \
    ./.adds/nm/desktop-gpui-kobo; do
    printf '%s\n' "$listing" | grep -Fxq "$required" || {
        printf 'The package is missing %s\n' "$required" >&2
        exit 1
    }
done
VERIFY_DIR=$(mktemp -d)
trap 'rm -rf "$VERIFY_DIR"' EXIT
tar -xzf "$ARCHIVE" -C "$VERIFY_DIR" ./.adds/bokheim/desktop-gpui-kobo
file "$VERIFY_DIR/.adds/bokheim/desktop-gpui-kobo" | grep -Eq 'ELF 32-bit.*ARM' || {
    printf 'The packaged binary is not a 32-bit ARM executable:\n' >&2
    file "$VERIFY_DIR/.adds/bokheim/desktop-gpui-kobo" >&2
    exit 1
}

mkdir -p "$(dirname "$COMMITTED")"
cp "$ARCHIVE" "$COMMITTED"

printf 'Updated %s\n' "$COMMITTED"
printf 'Commit it so the installer workflow embeds this build:\n'
printf '  git add %s\n' "apps/kobo-installer/package/Bokheim-Kobo-Libra2-armv7.tar.gz"
printf '  git commit -m "Refresh the committed Kobo package"\n'
