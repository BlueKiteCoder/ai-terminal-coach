#!/bin/zsh
set -euo pipefail

root=${0:A:h:h}
version=${1:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/Cargo.toml" | head -1)}
[[ -n $version ]] || { print -u2 'could not determine version'; exit 1; }

export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$root/target"}
cargo build --release --workspace --locked

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/aicoach-$version/bin" "$stage/aicoach-$version/share/aicoach"
cp "$CARGO_TARGET_DIR/release/aicoach" "$stage/aicoach-$version/bin/"
cp "$CARGO_TARGET_DIR/release/aicoachd" "$stage/aicoach-$version/bin/"
cp "$CARGO_TARGET_DIR/release/aicoach-ui" "$stage/aicoach-$version/bin/"
cp "$root/shell/aicoach.zsh" "$stage/aicoach-$version/share/aicoach/"
cp "$root/config/default.toml" "$stage/aicoach-$version/share/aicoach/"
cp "$root/scripts/aicoach-window.js" "$stage/aicoach-$version/share/aicoach/"
cp "$root/scripts/aicoach-hide.js" "$stage/aicoach-$version/share/aicoach/"
cp "$root/LICENSE" "$stage/aicoach-$version/"

if "$root/scripts/build-macos-helper.sh" "$stage/aicoach-$version/bin/aicoach-hotkey" >/dev/null 2>&1; then
  print 'included optional macOS global hotkey helper'
else
  print 'global hotkey helper skipped; terminal-local Option+Space remains available'
fi

mkdir -p "$root/dist"
arch=$(uname -m)
[[ $arch == x86_64 || $arch == arm64 ]] || {
  print -u2 -- "unsupported macOS architecture: $arch"
  exit 1
}
archive="$root/dist/aicoach-$version-macos-$arch.tar.gz"
tar -C "$stage" -czf "$archive" "aicoach-$version"
shasum -a 256 "$archive"
