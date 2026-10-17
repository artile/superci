#!/bin/sh
# Builds mkfs.erofs (erofs-utils 1.9.4, with lz4 1.10.0) as one static program for Linux arm64, for the layer store's
# converter (the AWS control plane's second function), from sources pinned by their SHA-256: zig's C compiler, no
# configure (config.h says what the build has). Writes it and its licenses to the folder given.
#   scripts/mkfs-erofs/build.sh OUT
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=$(mkdir -p "$1" && cd "$1" && pwd)
work="$here/../../target/mkfs-erofs"
mkdir -p "$work"
cd "$work"
fetch() { # url sha256 file
  if [ ! -f "$3" ] || [ "$(shasum -a 256 "$3" | cut -d' ' -f1)" != "$2" ]; then curl -q -fsSL -o "$3.part" "$1" && mv "$3.part" "$3"; fi
  [ "$(shasum -a 256 "$3" | cut -d' ' -f1)" = "$2" ] || { echo "$3 is not the one pinned" >&2; exit 1; }
}
fetch https://github.com/erofs/erofs-utils/archive/refs/tags/v1.9.4.tar.gz 7d135aa2550326a5acf20f53c518aea5a8900015ce50700044e40f818c31dd80 erofs.tar.gz
fetch https://github.com/lz4/lz4/archive/refs/tags/v1.10.0.tar.gz 537512904744b35e232912055ccf8ec66d768639ff3abe5788d90d792ec5f48b lz4.tar.gz
rm -rf erofs-utils-1.9.4 lz4-1.10.0
tar -xzf erofs.tar.gz
tar -xzf lz4.tar.gz
E=erofs-utils-1.9.4
L=lz4-1.10.0/lib
files=""
for f in config.c io.c cache.c super.c inode.c xattr.c exclude.c namei.c data.c compress.c compressor.c zmap.c decompress.c \
  compress_hints.c hashmap.c sha256.c blobchunk.c dir.c fragments.c dedupe.c uuid_unparse.c uuid.c tar.c block_list.c \
  rebuild.c diskbuf.c bitops.c dedupe_ext.c vmdk.c metabox.c global.c importer.c base64.c compressor_lz4.c \
  compressor_lz4hc.c kite_deflate.c compressor_deflate.c xxhash.c workqueue.c remotes/oci.c remotes/docker_config.c gzran.c; do
  files="$files $E/lib/$f"
done
# shellcheck disable=SC2086
zig cc -target aarch64-linux-musl -O2 -static -s -include "$here/config.h" -D_GNU_SOURCE -D_FILE_OFFSET_BITS=64 -I"$here" -I"$E/include" -I"$E/lib" -I"$L" \
  -o "$out/mkfs.erofs" "$E/mkfs/main.c" $files "$L/lz4.c" "$L/lz4hc.c" -lpthread
# erofs-utils: its library GPL-2.0+ OR MIT (taken as MIT), mkfs MIT; lz4: its library BSD-2-Clause.
{ echo "mkfs.erofs: erofs-utils 1.9.4 (https://github.com/erofs/erofs-utils), with lz4 1.10.0 (https://github.com/lz4/lz4)."; echo
  echo "== erofs-utils (MIT) =="; cat "$E/LICENSES/MIT"; echo; echo "== lz4 library (BSD-2-Clause) =="; cat "$L/LICENSE"; } > "$out/mkfs.erofs.LICENSES"
