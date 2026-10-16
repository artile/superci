#!/bin/sh
# Publishes SuperCI to crates.io, so that `cargo install superci` builds the program: `superci-core`, then the
# superci-programs-* crates, which carry what ./build.sh built (the control planes, the runner agent, superci-cache,
# superci-layers) in their embedded/, since a crate is built with Rust alone (three of them: crates.io takes 10 MB
# a crate), then `superci`. A version already there is passed by, so a publish that stopped halfway can be run again.
#   ./scripts/crates-publish.sh             # publish (the token: CARGO_REGISTRY_TOKEN, or `cargo login`)
#   ./scripts/crates-publish.sh --dry-run   # pack and build each as crates.io would get it; publish nothing
#   ./scripts/crates-publish.sh --embed     # only fill embedded/ from what is built
#   ./scripts/crates-publish.sh --embedded  # publish with embedded/ as it is (filled on another machine)
# Needs ./build.sh to have run, except with --embedded.
set -eu
cd "$(dirname "$0")/.."
# Said exactly, or nothing is done: anything this does not know is not taken for "publish".
[ $# -le 1 ] || { echo "one way at a time: --dry-run, --embed or --embedded" >&2; exit 2; }
case "${1:-}" in "" | --dry-run | --embed | --embedded) ;; *) echo "$1 is not a way to run this (--dry-run, --embed, --embedded)" >&2; exit 2 ;; esac
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
crates="superci-core superci-programs-planes superci-programs-cache superci-programs-layers superci"
for dep in core programs-planes programs-cache programs-layers; do
  grep -q "superci-$dep = { path = \"../$dep\", version = \"=$version\" }" crates/cli/Cargo.toml \
    || { echo "crates/cli/Cargo.toml does not ask for superci-$dep =$version" >&2; exit 1; }
done

# What each crate carries: its folder, then the files (built where, put in embedded/ as what).
carried() {
  echo "crates/programs-planes crates/plane-cloudflare/build/index.js:plane-cloudflare.js crates/plane-cloudflare/build/index_bg.wasm:plane-cloudflare.wasm crates/runners-cloudflare/build/index.js:runners-cloudflare.js crates/runners-cloudflare/build/index_bg.wasm:runners-cloudflare.wasm crates/plane-aws/build/bootstrap.zip:plane-aws.zip crates/plane-modal/build/superci-plane:plane-modal"
  echo "crates/programs-cache target/x86_64-unknown-linux-musl/release/superci-cache:cache-x86_64 target/aarch64-unknown-linux-musl/release/superci-cache:cache-aarch64 target/x86_64-pc-windows-gnu/release/superci-cache.exe:cache-windows.exe"
  echo "crates/programs-layers target/x86_64-unknown-linux-musl/release/superci-layers:layers-x86_64 target/aarch64-unknown-linux-musl/release/superci-layers:layers-aarch64"
  echo "crates/cli CHANGELOG.md:CHANGELOG.md"
}
carried | while read -r dir files; do
  embedded=$dir/embedded
  if [ "${1:-}" = "--embedded" ]; then
    for f in $files; do [ -f "$embedded/${f#*:}" ] || { echo "$embedded/${f#*:} is missing" >&2; exit 1; }; done
  else
    rm -rf "$embedded" && mkdir -p "$embedded"
    for f in $files; do
      [ -f "${f%%:*}" ] || { echo "${f%%:*} is not built: run ./build.sh first" >&2; exit 1; }
      cp "${f%%:*}" "$embedded/${f#*:}"
    done
  fi
done
if [ "${1:-}" = "--embedded" ]; then
  cmp -s CHANGELOG.md crates/cli/embedded/CHANGELOG.md || { echo "embedded/ was filled from another commit (its changelog differs)" >&2; exit 1; }
fi
[ "${1:-}" = "--embed" ] && exit 0

if [ "${1:-}" = "--dry-run" ]; then
  # Each packed and built from its package alone, each after those it needs as packed.
  cargo package $(for c in $crates; do printf -- '-p %s ' "$c"; done) --locked --allow-dirty
  ls -l target/package/*-"$version".crate
  # (crates.io takes 10 MB a crate.)
  for f in target/package/*-"$version".crate; do [ "$(wc -c < "$f")" -lt 10000000 ] || { echo "$f is over crates.io's 10 MB" >&2; exit 1; }; done
  exit 0
fi

there() { [ "$(curl -q -s -o /dev/null -w '%{http_code}' -A 'superci-release (https://github.com/supercorp-ai/superci)' "https://crates.io/api/v1/crates/$1/$version")" = 200 ]; }
for crate in $crates; do
  if there "$crate"; then echo "$crate $version is on crates.io already"; continue; fi
  # cargo waits until the crate is in the index before it returns, so the next one finds it.
  cargo publish -p "$crate" --locked --allow-dirty
done
