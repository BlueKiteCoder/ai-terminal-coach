#!/bin/zsh
set -euo pipefail

root=${0:A:h:h}
tag=${1:-}
source_sha256=${2:-}
output=${3:-}

[[ $tag =~ '^v[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$' ]] || {
  print -u2 -- "invalid release tag: $tag"
  exit 1
}
[[ $source_sha256 =~ '^[0-9a-f]{64}$' ]] || {
  print -u2 'source archive SHA-256 must be 64 lowercase hexadecimal characters'
  exit 1
}
[[ -n $output ]] || {
  print -u2 'usage: render-homebrew-formula.sh TAG SOURCE_SHA256 OUTPUT'
  exit 2
}
mkdir -p "${output:A:h}"

/usr/bin/awk -v tag="$tag" -v sha="$source_sha256" '
  /^  homepage / {
    print
    print "  url \"https://github.com/BlueKiteCoder/ai-terminal-coach/archive/refs/tags/" tag ".tar.gz\""
    print "  sha256 \"" sha "\""
    next
  }
  { print }
' "$root/homebrew/aicoach.rb" > "$output"

/usr/bin/grep -Fq -- "refs/tags/$tag.tar.gz" "$output"
/usr/bin/grep -Fq -- "sha256 \"$source_sha256\"" "$output"
print -r -- "rendered stable Homebrew formula: ${output:A}"
