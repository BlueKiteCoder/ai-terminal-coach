#!/bin/zsh
set -euo pipefail
root=${0:A:h:h}
out=${1:-"$root/target/release/aicoach-hotkey"}
mkdir -p "${out:h}"
module_cache=$(mktemp -d)
trap 'rm -rf "$module_cache"' EXIT
export CLANG_MODULE_CACHE_PATH=$module_cache

if /usr/bin/swiftc -O -framework AppKit -framework Carbon \
  "$root/macos/AICoachHotkey.swift" -o "$out" 2>/dev/null; then
  print 'built Swift hotkey helper'
elif /usr/bin/xcrun clang -fobjc-arc -O2 -framework AppKit -framework Carbon \
  "$root/macos/AICoachHotkey.m" -o "$out"; then
  print 'Swift toolchain unavailable; built Objective-C hotkey helper'
else
  print -u2 'could not build the macOS global hotkey helper'
  exit 1
fi
/usr/bin/codesign --force --sign - "$out" >/dev/null 2>&1 || true
print -r -- "$out"
