#!/bin/zsh
set -euo pipefail

root=${0:A:h:h}
cd "$root"

# Keep removed private-provider markers split so this guard does not match itself.
typeset -a forbidden
forbidden=(
  'wanjie''data'
  'maas-''openapi'
  '243c''0411'
  'sk-[A-Za-z0-9_-]{20,}'
)

failed=0
for pattern in $forbidden; do
  if git grep -I -n -E -- "$pattern" -- . ':!scripts/check-no-secrets.sh'; then
    failed=1
  fi
done

if [[ $failed == 1 ]]; then
  print -u2 'repository contains a forbidden credential or removed private-provider marker'
  exit 1
fi
print 'tracked source secret scan passed'
