#!/bin/zsh
set -euo pipefail

root=${0:A:h:h}
cd "$root"

# `status` is a read-only special parameter in Zsh. Assigning to it makes a
# workflow step fail before the command whose exit code it meant to preserve.
if git grep -n -E '^[[:space:]]+status=' -- .github/workflows; then
  print -u2 -- 'GitHub workflow assigns to Zsh read-only parameter `status`'
  exit 1
fi

print 'GitHub workflow shell-safety check passed'
