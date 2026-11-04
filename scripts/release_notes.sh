#!/bin/sh
# Prints the CHANGELOG.md section for a version (its body, without the
# heading), as Markdown. Fails when the version has
# no section or the section is empty, so a release cannot ship without
# notes.
#
#   scripts/release_notes.sh 0.3.2
set -eu
cd "$(dirname "$0")/.."

VERSION=${1:?usage: release_notes.sh VERSION}
SECTION=$(awk -v heading="## [$VERSION]" '
    index($0, heading) == 1 { found = 1; next }
    found && /^## \[/ { exit }
    found && /^\[[^]]*\]: / { exit }
    found { print }
' CHANGELOG.md | sed -e '/./,$!d' | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}')

if [ -z "$(printf '%s' "$SECTION" | tr -d '[:space:]')" ]; then
    echo "error: CHANGELOG.md has no notes for $VERSION (add a \"## [$VERSION] - YYYY-MM-DD\" section)" >&2
    exit 1
fi

printf '%s\n' "$SECTION"
