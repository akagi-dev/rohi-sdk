#!/usr/bin/env bash
# Checks that every .rs file starts with the header from .github/license-check/HEADER-APACHE2.
# Usage: scripts/check-license.sh [file.rs ...]   (defaults to all .rs files in the repository)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
header="$root/.github/license-check/HEADER-APACHE2"
cd "$root"

[[ -f $header ]] || { echo "error: header file not found: $header" >&2; exit 2; }
lines=$(wc -l < "$header")

if (( $# > 0 )); then
    files=("$@")
elif git rev-parse --git-dir >/dev/null 2>&1; then
    mapfile -d '' -t files < <(git ls-files -z --cached --others --exclude-standard -- '*.rs')
else
    mapfile -d '' -t files < <(find . -name target -prune -o -name '*.rs' -type f -print0)
fi

bad=0
for f in "${files[@]}"; do
    [[ -f $f ]] || continue
    if ! head -n "$lines" -- "$f" | cmp -s - "$header"; then
        echo "missing or wrong license header: $f"
        bad=$((bad + 1))
    fi
done

if (( bad > 0 )); then
    echo "$bad of ${#files[@]} file(s) have a missing or wrong license header" >&2
    exit 1
fi
echo "license headers OK (${#files[@]} files)"
