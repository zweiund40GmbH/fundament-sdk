#!/usr/bin/env bash
# Source file size check: warns above WARN_LIMIT lines (default 300) and fails above FAIL_LIMIT
# (default 400). Checks *.rs files outside target/ and .git/.
# Usage: scripts/check-lines.sh [root-dir]
set -euo pipefail

WARN_LIMIT="${WARN_LIMIT:-300}"
FAIL_LIMIT="${FAIL_LIMIT:-400}"
ROOT="${1:-.}"
cd "$ROOT"

warnings=0
failures=0
while IFS= read -r -d '' file; do
  path="${file#./}"
  lines=$(wc -l < "$file")
  if [ "$lines" -gt "$FAIL_LIMIT" ]; then
    echo "ERROR    $path: $lines lines (> $FAIL_LIMIT) - split the file"
    failures=$((failures + 1))
  elif [ "$lines" -gt "$WARN_LIMIT" ]; then
    echo "WARNING  $path: $lines lines (> $WARN_LIMIT)"
    warnings=$((warnings + 1))
  fi
done < <(find . \( -name target -o -name .git \) -prune -o -type f -name '*.rs' -print0)

echo "check-lines: $warnings warning(s), $failures error(s)"
[ "$failures" -eq 0 ]
