#!/bin/zsh
set -euo pipefail

root=${0:A:h:h}
cd "$root"

failed=0
while IFS= read -r record; do
  reference=${record#*uses:}
  reference=${reference%%\#*}
  reference=${reference//[[:space:]]/}

  # Repository-local actions are already fixed by the checked-out commit.
  if [[ "$reference" == ./* ]]; then
    continue
  fi

  if [[ ! "$reference" =~ '@[0-9a-f]{40}$' ]]; then
    print -u2 -- "GitHub Action is not pinned to a full commit SHA: $record"
    failed=1
  fi
done < <(git grep -n -E '^[[:space:]]*(-[[:space:]]+)?uses:' -- .github/workflows || true)

if [[ $failed == 1 ]]; then
  exit 1
fi
print 'GitHub Actions immutable-reference check passed'
