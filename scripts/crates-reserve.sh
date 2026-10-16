#!/bin/sh
# Makes the crates SuperCI's program depends on exist on crates.io, so that each can trust the release workflow:
# crates.io lets a crate's first version come only with a token, never by trusted publishing. Each gets a 0.0.0 that
# carries nothing and is yanked at once (no one may depend on it); the release workflow publishes the real versions.
# Run once, by hand, signed in to crates.io (`cargo login`, or CARGO_REGISTRY_TOKEN). A crate there already is passed by.
#   ./scripts/crates-reserve.sh                 # the three superci-programs-* crates
#   ./scripts/crates-reserve.sh NAME [NAME...]  # these
set -eu
cd "$(dirname "$0")/.."
names=${*:-superci-programs-planes superci-programs-cache superci-programs-layers}
for name in $names; do
  if [ "$(curl -q -s -o /dev/null -w '%{http_code}' -A 'superci-release (https://github.com/supercorp-ai/superci)' "https://crates.io/api/v1/crates/$name")" = 200 ]; then
    echo "$name is on crates.io already"
    continue
  fi
  dir=$(mktemp -d)
  mkdir -p "$dir/src"
  cat > "$dir/Cargo.toml" <<EOF
[package]
name = "$name"
version = "0.0.0"
edition = "2021"
license = "MIT"
description = "Part of SuperCI (the superci crate); this version carries nothing"
repository = "https://github.com/supercorp-ai/superci"
homepage = "https://superci.dev"

[workspace]
EOF
  echo "//! Part of SuperCI (the \`superci\` crate): its real versions come with SuperCI's releases." > "$dir/src/lib.rs"
  (cd "$dir" && cargo publish --allow-dirty)
  cargo yank --version 0.0.0 "$name"
  rm -rf "$dir"
  echo "$name: on crates.io (0.0.0, yanked). Now its Trusted Publishing: https://crates.io/crates/$name/settings"
done
