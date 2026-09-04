#!/bin/zsh
set -euo pipefail

[[ $# -eq 3 ]] || {
  print -u2 'usage: verify-release-archive.sh ARCHIVE VERSION ARCHITECTURE'
  exit 2
}
archive=${1:A}
version=$2
expected_arch=$3
require_signed=${AICOACH_REQUIRE_SIGNED:-0}
require_hotkey=${AICOACH_REQUIRE_HOTKEY:-0}
[[ $require_signed == 0 || $require_signed == 1 ]] || {
  print -u2 'AICOACH_REQUIRE_SIGNED must be 0 or 1'
  exit 1
}
[[ $require_hotkey == 0 || $require_hotkey == 1 ]] || {
  print -u2 'AICOACH_REQUIRE_HOTKEY must be 0 or 1'
  exit 1
}
[[ -f $archive ]] || { print -u2 -- "archive not found: $archive"; exit 1; }
[[ $version =~ '^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$' ]] || {
  print -u2 -- "invalid expected version: $version"
  exit 1
}
[[ $expected_arch == arm64 || $expected_arch == x86_64 ]] || {
  print -u2 -- "invalid expected architecture: $expected_arch"
  exit 1
}

prefix="aicoach-$version-macos-$expected_arch"
case ${archive:t} in
  "$prefix.tar.gz"|"$prefix.zip") ;;
  *) print -u2 -- "unexpected archive name: ${archive:t}"; exit 1 ;;
esac
checksum="${archive:h}/$prefix.sha256"
[[ -f $checksum ]] || { print -u2 -- "checksum file not found: $checksum"; exit 1; }
expected_hash=$(/usr/bin/awk -v file="${archive:t}" '$2 == file { print $1 }' "$checksum")
[[ $expected_hash =~ '^[0-9a-f]{64}$' ]] || {
  print -u2 -- "checksum entry missing or malformed for ${archive:t}"
  exit 1
}
actual_hash=$(/usr/bin/shasum -a 256 "$archive" | /usr/bin/awk '{ print $1 }')
[[ $actual_hash == $expected_hash ]] || {
  print -u2 -- "checksum mismatch for ${archive:t}"
  exit 1
}

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
list="$stage/archive-list.txt"
if [[ $archive == *.tar.gz ]]; then
  /usr/bin/tar -tzf "$archive" > "$list"
else
  /usr/bin/unzip -Z1 "$archive" > "$list"
fi
while IFS= read -r entry; do
  [[ $entry == "aicoach-$version" || $entry == "aicoach-$version/"* ]] || {
    print -u2 -- "archive entry escapes package root: $entry"
    exit 1
  }
  [[ $entry != /* && $entry != *'/../'* && $entry != *'/..' && $entry != *'\\'* ]] || {
    print -u2 -- "unsafe archive entry: $entry"
    exit 1
  }
done < "$list"
if [[ -n $(LC_ALL=C /usr/bin/sort "$list" | /usr/bin/uniq -d) ]]; then
  print -u2 'archive contains duplicate entries'
  exit 1
fi

if [[ $archive == *.tar.gz ]]; then
  COPYFILE_DISABLE=1 /usr/bin/tar -xzf "$archive" -C "$stage"
else
  COPYFILE_DISABLE=1 /usr/bin/unzip -q "$archive" -d "$stage"
fi
package="$stage/aicoach-$version"
[[ -d $package ]] || { print -u2 'package root is missing'; exit 1; }
if /usr/bin/find "$package" -type l | /usr/bin/grep -q .; then
  print -u2 'release archive must not contain symbolic links'
  exit 1
fi
if /usr/bin/find "$package" ! -type d ! -type f | /usr/bin/grep -q .; then
  print -u2 'release archive contains a special filesystem object'
  exit 1
fi

typeset -a required_files executables
required_files=(
  bin/aicoach
  bin/aicoachd
  bin/aicoach-ui
  share/aicoach/aicoach.zsh
  share/aicoach/default.toml
  share/aicoach/aicoach-window.js
  share/aicoach/aicoach-hide.js
  LICENSE
  README.md
  CHANGELOG.md
  manifest.json
)
for relative in $required_files; do
  [[ -f "$package/$relative" ]] || { print -u2 -- "missing packaged file: $relative"; exit 1; }
done
if [[ $require_hotkey == 1 && ! -f "$package/bin/aicoach-hotkey" ]]; then
  print -u2 'release requires bin/aicoach-hotkey'
  exit 1
fi

executables=("$package/bin/aicoach" "$package/bin/aicoachd" "$package/bin/aicoach-ui")
[[ -f "$package/bin/aicoach-hotkey" ]] && executables+=("$package/bin/aicoach-hotkey")

allowed="$stage/allowed-files.txt"
print -rl -- $required_files > "$allowed"
[[ -f "$package/bin/aicoach-hotkey" ]] && print -r -- 'bin/aicoach-hotkey' >> "$allowed"
LC_ALL=C /usr/bin/sort -o "$allowed" "$allowed"
actual="$stage/actual-files.txt"
(
  cd "$package"
  /usr/bin/find . -type f -print | /usr/bin/sed 's#^\./##' | LC_ALL=C /usr/bin/sort > "$actual"
)
if ! /usr/bin/cmp -s "$allowed" "$actual"; then
  print -u2 'release archive contains missing or unexpected regular files:'
  /usr/bin/diff -u "$allowed" "$actual" >&2 || true
  exit 1
fi

for executable in $executables; do
  [[ -x $executable ]] || { print -u2 -- "not executable: ${executable:t}"; exit 1; }
  [[ $(/usr/bin/lipo -archs "$executable") == $expected_arch ]] || {
    print -u2 -- "wrong architecture in ${executable:t}"
    exit 1
  }
  /usr/bin/codesign --verify --strict "$executable"
  details=$(/usr/bin/codesign -dv --verbose=4 "$executable" 2>&1)
  if [[ $require_signed == 1 && $details != *'Authority=Developer ID Application:'* ]]; then
    print -u2 -- "${executable:t} is not signed with Developer ID Application"
    exit 1
  fi
  if [[ $require_signed == 1 && $details != *'(runtime)'* ]]; then
    print -u2 -- "${executable:t} does not enable the hardened runtime"
    exit 1
  fi
  if [[ $require_signed == 1 && $details != *'Timestamp='* ]]; then
    print -u2 -- "${executable:t} has no secure signing timestamp"
    exit 1
  fi
  minimum=$(xcrun vtool -show-build "$executable" 2>/dev/null | \
    /usr/bin/awk '$1 == "minos" { print $2; exit }')
  [[ -n $minimum ]] || { print -u2 -- "missing minimum macOS version in ${executable:t}"; exit 1; }
  /usr/bin/awk -v value="$minimum" 'BEGIN {
    split(value, pieces, ".");
    if ((pieces[1] + 0) > 13) exit 1;
  }' || {
    print -u2 -- "${executable:t} requires macOS $minimum, expected 13.x or older"
    exit 1
  }
done

/usr/bin/grep -Fq -- "\"version\": \"$version\"" "$package/manifest.json"
/usr/bin/grep -Fq -- "\"architecture\": \"$expected_arch\"" "$package/manifest.json"
/usr/bin/grep -Fq -- '"platform": "macos"' "$package/manifest.json"
/usr/bin/grep -Eq -- '"minimum_macos": "13(\.[0-9]+){0,2}"' "$package/manifest.json"
/usr/bin/grep -Eq -- '"git_dirty": (true|false)' "$package/manifest.json"
if [[ $require_signed == 1 ]]; then
  /usr/bin/grep -Fq -- '"git_dirty": false' "$package/manifest.json"
fi
if [[ $require_signed == 1 ]]; then
  /usr/bin/grep -Fq -- '"signature": "developer-id"' "$package/manifest.json"
else
  /usr/bin/grep -Eq -- '"signature": "(ad-hoc|developer-id)"' "$package/manifest.json"
fi
removed_private_values='wanjie''data|243c''0411'
if /usr/bin/grep -R -E -n -- "$removed_private_values" "$package" >/dev/null; then
  print -u2 'release archive contains a removed private provider value'
  exit 1
fi
if [[ $(uname -m) == $expected_arch ]]; then
  [[ $("$package/bin/aicoach" --version) == "aicoach $version" ]] || {
    print -u2 'packaged CLI version does not match archive version'
    exit 1
  }
fi
print -r -- "verified ${archive:t} ($expected_arch, macOS 13+)"
