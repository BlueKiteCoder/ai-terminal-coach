#!/bin/zsh
set -euo pipefail

root=${0:A:h:h}
cd "$root"

# Keep the generic credential prefix split so this guard does not match itself.
typeset -a forbidden
forbidden=(
  's''k-[A-Za-z0-9_-]{20,}'
)

failed=0
for pattern in $forbidden; do
  if git grep -I -n -E -- "$pattern" -- . ':!scripts/check-no-secrets.sh'; then
    failed=1
  fi
done

if [[ $failed == 1 ]]; then
  print -u2 'repository contains a credential-like token'
  exit 1
fi
print 'tracked source secret scan passed'
