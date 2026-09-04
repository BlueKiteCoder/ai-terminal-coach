#!/bin/zsh
set -euo pipefail

root=${0:A:h:h}
raw_version=${1:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/Cargo.toml" | head -1)}
version=${raw_version#v}
[[ $version =~ '^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$' ]] || {
  print -u2 -- "invalid release version: $raw_version"
  exit 1
}
[[ $(uname -s) == Darwin ]] || {
  print -u2 'release archives must be built natively on macOS'
  exit 1
}

workspace_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/Cargo.toml" | head -1)
[[ $version == $workspace_version ]] || {
  print -u2 -- "requested version $version does not match Cargo.toml $workspace_version"
  exit 1
}

require_signed=${AICOACH_REQUIRE_SIGNED:-0}
require_hotkey=${AICOACH_REQUIRE_HOTKEY:-0}
signing_identity=${AICOACH_SIGNING_IDENTITY:-}
[[ $require_signed == 0 || $require_signed == 1 ]] || {
  print -u2 'AICOACH_REQUIRE_SIGNED must be 0 or 1'
  exit 1
}
[[ $require_hotkey == 0 || $require_hotkey == 1 ]] || {
  print -u2 'AICOACH_REQUIRE_HOTKEY must be 0 or 1'
  exit 1
}
if [[ $require_signed == 1 && -z $signing_identity ]]; then
  print -u2 'refusing a release archive without AICOACH_SIGNING_IDENTITY'
  exit 1
fi

git_dirty=false
if ! git -C "$root" diff --quiet --ignore-submodules -- || \
   ! git -C "$root" diff --cached --quiet --ignore-submodules -- || \
   [[ -n $(git -C "$root" ls-files --others --exclude-standard) ]]; then
  git_dirty=true
fi
if [[ $require_signed == 1 && $git_dirty == true ]]; then
  print -u2 'refusing a public release archive from a dirty Git worktree'
  exit 1
fi

export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$root/target"}
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-13.0}
[[ $MACOSX_DEPLOYMENT_TARGET =~ '^13(\.[0-9]+){0,2}$' ]] || {
  print -u2 -- "MACOSX_DEPLOYMENT_TARGET must be a macOS 13 version, got: $MACOSX_DEPLOYMENT_TARGET"
  exit 1
}
cargo build --release --workspace --locked

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
package="$stage/aicoach-$version"
mkdir -p "$package/bin" "$package/share/aicoach"
/usr/bin/install -m 0755 "$CARGO_TARGET_DIR/release/aicoach" "$package/bin/aicoach"
/usr/bin/install -m 0755 "$CARGO_TARGET_DIR/release/aicoachd" "$package/bin/aicoachd"
/usr/bin/install -m 0755 "$CARGO_TARGET_DIR/release/aicoach-ui" "$package/bin/aicoach-ui"
/usr/bin/install -m 0644 "$root/shell/aicoach.zsh" "$package/share/aicoach/aicoach.zsh"
/usr/bin/install -m 0644 "$root/config/default.toml" "$package/share/aicoach/default.toml"
/usr/bin/install -m 0644 "$root/scripts/aicoach-window.js" "$package/share/aicoach/aicoach-window.js"
/usr/bin/install -m 0644 "$root/scripts/aicoach-hide.js" "$package/share/aicoach/aicoach-hide.js"
/usr/bin/install -m 0644 "$root/LICENSE" "$package/LICENSE"
/usr/bin/install -m 0644 "$root/README.md" "$package/README.md"
/usr/bin/install -m 0644 "$root/CHANGELOG.md" "$package/CHANGELOG.md"

if "$root/scripts/build-macos-helper.sh" "$package/bin/aicoach-hotkey"; then
  print 'included macOS global hotkey helper'
elif [[ $require_hotkey == 1 ]]; then
  print -u2 'required macOS global hotkey helper could not be built'
  exit 1
else
  print 'global hotkey helper skipped; terminal-local Option+Space remains available'
fi

typeset -a executables
executables=("$package/bin/aicoach" "$package/bin/aicoachd" "$package/bin/aicoach-ui")
[[ -f "$package/bin/aicoach-hotkey" ]] && executables+=("$package/bin/aicoach-hotkey")

signature=ad-hoc
if [[ -n $signing_identity ]]; then
  signature=developer-id
  typeset -A identifiers
  identifiers=(
    aicoach com.bluekitecoder.aicoach.cli
    aicoachd com.bluekitecoder.aicoach.daemon
    aicoach-ui com.bluekitecoder.aicoach.ui
    aicoach-hotkey com.bluekitecoder.aicoach.hotkey
  )
  for executable in $executables; do
    name=${executable:t}
    /usr/bin/codesign --force --options runtime --timestamp \
      --identifier "$identifiers[$name]" --sign "$signing_identity" "$executable"
  done
else
  for executable in $executables; do
    /usr/bin/codesign --force --sign - "$executable" >/dev/null
  done
  print 'created ad-hoc-signed test archive; it is not a notarized public release'
fi
for executable in $executables; do
  /usr/bin/codesign --verify --strict "$executable"
done

arch=$(/usr/bin/lipo -archs "$package/bin/aicoach")
[[ $arch == x86_64 || $arch == arm64 ]] || {
  print -u2 -- "release binary must contain exactly one supported architecture, got: $arch"
  exit 1
}
for executable in $executables; do
  [[ $(/usr/bin/lipo -archs "$executable") == $arch ]] || {
    print -u2 -- "architecture mismatch in ${executable:t}"
    exit 1
  }
done

git_commit=$(git -C "$root" rev-parse --verify HEAD 2>/dev/null || print unknown)
source_epoch=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct 2>/dev/null || print 0)}
[[ $source_epoch == <-> ]] || {
  print -u2 'SOURCE_DATE_EPOCH must be an integer Unix timestamp'
  exit 1
}
print -r -- "{
  \"product\": \"AI Terminal Coach\",
  \"version\": \"$version\",
  \"platform\": \"macos\",
  \"architecture\": \"$arch\",
  \"minimum_macos\": \"$MACOSX_DEPLOYMENT_TARGET\",
  \"git_commit\": \"$git_commit\",
  \"git_dirty\": $git_dirty,
  \"signature\": \"$signature\"
}" > "$package/manifest.json"
/bin/chmod 0644 "$package/manifest.json"

export TZ=UTC
timestamp=$(date -r "$source_epoch" +%Y%m%d%H%M.%S)
/usr/bin/find "$package" -exec /usr/bin/touch -h -t "$timestamp" {} +

mkdir -p "$root/dist"
prefix="aicoach-$version-macos-$arch"
tar_archive="$root/dist/$prefix.tar.gz"
zip_archive="$root/dist/$prefix.zip"
checksum="$root/dist/$prefix.sha256"
tar_file="$root/dist/$prefix.tar"
file_list="$stage/archive-files.txt"
/bin/rm -f "$tar_archive" "$zip_archive" "$checksum" "$tar_file"
(
  cd "$stage"
  /usr/bin/find "aicoach-$version" -print | LC_ALL=C /usr/bin/sort > "$file_list"
  COPYFILE_DISABLE=1 /usr/bin/tar --no-recursion --format ustar \
    --uid 0 --gid 0 --uname root --gname wheel \
    --no-acls --no-fflags --no-xattrs --no-mac-metadata -cf "$tar_file" -T "$file_list"
  COPYFILE_DISABLE=1 /usr/bin/zip -X -q "$zip_archive" -@ < "$file_list"
)
/usr/bin/gzip -n -9 -f "$tar_file"
(
  cd "$root/dist"
  /usr/bin/shasum -a 256 "${tar_archive:t}" "${zip_archive:t}" > "${checksum:t}"
)

"$root/scripts/verify-release-archive.sh" "$tar_archive" "$version" "$arch"
"$root/scripts/verify-release-archive.sh" "$zip_archive" "$version" "$arch"
print -r -- "wrote $tar_archive"
print -r -- "wrote $zip_archive"
print -r -- "wrote $checksum"
