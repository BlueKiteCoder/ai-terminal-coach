#!/bin/zsh
set -euo pipefail

root=${0:A:h:h}
tag=${1:-}
[[ $tag =~ '^v[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$' ]] || {
  print -u2 -- "release tag must be semantic and start with v: $tag"
  exit 1
}
version=${tag#v}
workspace_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/Cargo.toml" | head -1)
[[ $version == $workspace_version ]] || {
  print -u2 -- "tag $tag does not match Cargo.toml version $workspace_version"
  exit 1
}
[[ -f "$root/CHANGELOG.md" ]] || { print -u2 'CHANGELOG.md is missing'; exit 1; }
/usr/bin/grep -Eq -- "^## \\[$version\\] - [0-9]{4}-[0-9]{2}-[0-9]{2}$" \
  "$root/CHANGELOG.md" || {
  print -u2 -- "CHANGELOG.md has no dated $version release section"
  exit 1
}
cargo metadata --locked --no-deps --format-version 1 >/dev/null

if git -C "$root" rev-parse -q --verify "refs/tags/$tag^{}" >/dev/null; then
  tag_commit=$(git -C "$root" rev-parse "refs/tags/$tag^{}")
  head_commit=$(git -C "$root" rev-parse HEAD)
  [[ $tag_commit == $head_commit ]] || {
    print -u2 -- "tag $tag points to $tag_commit, but the checked-out commit is $head_commit"
    exit 1
  }
fi
print -r -- "verified release metadata for $tag"
