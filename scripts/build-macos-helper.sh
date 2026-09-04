#!/bin/zsh
set -euo pipefail
root=${0:A:h:h}
out=${1:-"$root/target/release/aicoach-hotkey"}
deployment_target=${MACOSX_DEPLOYMENT_TARGET:-13.0}
[[ $deployment_target =~ '^13(\.[0-9]+){0,2}$' ]] || {
  print -u2 -- "MACOSX_DEPLOYMENT_TARGET must be a macOS 13 version, got: $deployment_target"
  exit 1
}
architecture=$(uname -m)
[[ $architecture == arm64 || $architecture == x86_64 ]] || {
  print -u2 -- "unsupported macOS architecture: $architecture"
  exit 1
}
target="$architecture-apple-macos$deployment_target"
mkdir -p "${out:h}"
module_cache=$(mktemp -d)
trap 'rm -rf "$module_cache"' EXIT
export CLANG_MODULE_CACHE_PATH=$module_cache

if /usr/bin/swiftc -O -target "$target" -framework AppKit -framework Carbon \
  "$root/macos/AICoachHotkey.swift" -o "$out" 2>/dev/null; then
  print 'built Swift hotkey helper'
elif /usr/bin/xcrun clang -fobjc-arc -O2 -target "$target" -framework AppKit -framework Carbon \
  "$root/macos/AICoachHotkey.m" -o "$out"; then
  print 'Swift toolchain unavailable; built Objective-C hotkey helper'
else
  print -u2 'could not build the macOS global hotkey helper'
  exit 1
fi
/usr/bin/codesign --force --sign - "$out" >/dev/null 2>&1 || true
print -r -- "$out"
