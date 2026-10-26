#!/bin/bash
# The reader, for real: run by ../try.sh inside a privileged Linux container, with at /t a packed small image (out/),
# the checksums of its files (expected.txt) and the reader program (reader).
set -u
PIN=$(ls /t/out | head -1)
cd /srv && cp -r /t/out /srv/out
# A copy with one piece changed (piece 20: in the big file), and one with another index.
cp -r /srv/out/$PIN /srv/bad && mkdir -p /srv/badroot && mv /srv/bad /srv/badroot/$PIN && printf 'XXXX' | dd of=/srv/badroot/$PIN/p/000020 bs=1 seek=100 conv=notrunc 2>/dev/null
mkdir -p /srv/otherroot && cp -r /srv/out/$PIN /srv/otherroot/$PIN && sed -i 's/"piece":1048576/"piece":1048576 /' /srv/otherroot/$PIN/index.json
mkdir -p /srv/hotroot && cp -r /srv/out/$PIN /srv/hotroot/$PIN && echo '[5,6,7]' > /srv/hotroot/$PIN/hot.json
(cd /srv && python3 -m http.server 8000 --bind 127.0.0.1 >/dev/null 2>&1 &)
sleep 1
pass=0; fail=0
ok() { if [ "$2" = "$3" ]; then echo "ok   $1 (${SECONDS} s)"; pass=$((pass+1)); else echo "FAIL $1 (${SECONDS} s): got '$2', expected '$3'"; fail=$((fail+1)); fi; }
start_at() { # address, cache name, extra args
  mkdir -p /mnt/$2/file /mnt/$2/lower /mnt/$2/cache
  /t/reader "$1" /mnt/$2/file /mnt/$2/cache "${@:3}" > /mnt/$2/log 2>&1 &
  echo $! > /mnt/$2/pid
  for i in $(seq 1 100); do ls /mnt/$2/file/* >/dev/null 2>&1 && break; sleep 0.05; done
  timeout 60 mount -t squashfs -o loop,ro /mnt/$2/file/* /mnt/$2/lower 2>/mnt/$2/mounterr
}
start() { start_at "http://127.0.0.1:8000/$1/$PIN" "${@:2}"; } # root dir, cache name, extra args
# (Unmounted: the reader ends, saying how.)
# (Each step bounded: a hang ends, said, rather than holding the whole test.)
finish() {
  local p; p=$(cat /mnt/$1/pid)
  umount /mnt/$1/lower 2>/dev/null; umount /mnt/$1/file 2>/dev/null || umount -l /mnt/$1/file 2>/dev/null
  for i in $(seq 1 100); do kill -0 "$p" 2>/dev/null || break; sleep 0.1; done
  kill -9 "$p" 2>/dev/null; wait "$p"; echo $?
}
# (How a start that cannot go on ends: its code, and what it said, on one line.)
stops() { timeout 60 /t/reader "$@" > /mnt/c/stop 2>&1; echo "$? $(tr '\n' ' ' < /mnt/c/stop | head -c 600)"; }
# (Each file by itself: some sha256sum stop at the first file they cannot read.)
sums() { (cd /mnt/$1/lower && find . -type f | sort | while read -r f; do timeout 120 sha256sum "$f" 2>&1; done); }
stat_of() { sleep 1.2; python3 -c "import json,sys; print(json.load(open('/mnt/$1/cache/stats.json')).get('$2'))"; }

# 1. As published: every file reads as it was packed; each piece checked; the hot list fetched.
start out a
ok "files read as packed" "$(sums a | sha256sum)" "$(cat /t/expected.txt | sha256sum)"
ok "each piece checked" "$(grep -c 'each checked' /mnt/a/log)" 1
ok "hot list used" "$(grep -c 'hot: 3 pieces' /mnt/a/log)" 1
ok "nothing failed" "$(stat_of a failed)" 0

# 2. A changed piece: only what lies in it fails to read; the rest reads; nothing wrong is ever handed out.
start badroot b
bad=$(sums b)
ok "a file in the changed piece is refused" "$(echo "$bad" | grep -c 'Input/output error')" 1
ok "other files still read" "$(echo "$bad" | grep -v 'error' | grep -c -F -f <(grep -v big.bin /t/expected.txt | cut -d' ' -f1))" 9
ok "the changed piece is said" "$(grep -c 'piece 20: not what the index says' /mnt/b/log | sed 's/[1-9][0-9]*/some/')" some
ok "counted as failed" "$( [ "$(stat_of b failed)" -ge 1 ] && echo yes)" yes

# 3. Another index than the address names: the reader stops.
mkdir -p /mnt/c/file /mnt/c/cache
/t/reader "http://127.0.0.1:8000/otherroot/$PIN" /mnt/c/file /mnt/c/cache > /mnt/c/log 2>&1; code=$?
ok "an index that is not the pinned one stops it" "$code $(grep -c 'not the one the address names' /mnt/c/log)" "1 1"

# 4. A hot list the index does not name: not used; the image still works.
start hotroot d
ok "another hot list is not used" "$(grep -c 'hot.json is not the one the index names' /mnt/d/log)" 1
ok "files still read" "$(sums d | sha256sum)" "$(cat /t/expected.txt | sha256sum)"

# 5. A cache that may hold little: pieces are dropped and fetched again; everything still reads right, twice.
start out e --max-cache-mb 6
ok "reads with a small cache" "$(sums e | sha256sum)" "$(cat /t/expected.txt | sha256sum)"
echo 3 > /proc/sys/vm/drop_caches
ok "reads again after the kernel forgot" "$(sums e | sha256sum)" "$(cat /t/expected.txt | sha256sum)"
ok "pieces were dropped" "$( [ "$(stat_of e dropped)" -ge 20 ] && echo yes)" yes
ok "it holds no more than it may" "$( [ "$(stat_of e kept_bytes)" -le $((6*1048576)) ] && echo yes)" yes
ok "its file takes that little disk" "$( [ "$(du -m /mnt/e/cache/pieces | cut -f1)" -le 8 ] && echo yes)" yes

# 6. A disk with no room to spare: nothing is kept beyond what a read holds, and reads still work.
start out f --keep-free-mb 99999999
ok "reads with no room to spare" "$(sums f | sha256sum)" "$(cat /t/expected.txt | sha256sum)"
ok "almost nothing kept" "$( [ "$(stat_of f kept_bytes)" -le $((4*1048576)) ] && echo yes)" yes

# 7. Not https, and not this machine: refused.
/t/reader "http://example.com/x" /mnt/c/file /mnt/c/cache > /mnt/c/log2 2>&1
ok "plain http elsewhere is refused" "$(grep -c 'must start with https://' /mnt/c/log2)" 1
# (Done with: unmounted, so the machine's loop devices are not all taken.)
for d in a b d e f; do finish $d > /dev/null; done

# 8. Asked of nothing: how to call it, said. An https address that does not answer: asked again, then given up.
ok "with no address, how to call it is said" "$(stops | grep -c '^2 image-reader <address>')" 1
ok "an address that does not answer stops it" "$(stops https://127.0.0.1:1/x /mnt/c/file /mnt/c/cache | grep -c '^1 .*no index.json at the address')" 1

# 9. An address that pins nothing, an index without checksums: pieces are taken as they come (one cut short is not),
# from this machine by name, with as many fetchers and as much read ahead as asked; what is read is recorded; unmounted,
# it ends. Hot lists as given: with a piece twice and one there is not; empty; not a list; too big.
python3 - "/srv/out/$PIN" <<'PY'
import json, shutil, sys
i = json.load(open(sys.argv[1] + '/index.json'))
for name, hot in [('plain-a', '[1,1,999]'), ('plain-b', '[]'), ('plain-c', 'nope'), ('plain-d', '[' + ','.join(['0'] * 9000000) + ']')]:
    shutil.copytree(sys.argv[1] + '/p', '/srv/' + name + '/p')
    json.dump({ 'name': i['name'], 'size': i['size'], 'piece': i['piece'] }, open('/srv/' + name + '/index.json', 'w'))
    open('/srv/' + name + '/hot.json', 'w').write(hot)
PY
truncate -s 1000 /srv/plain-a/p/000020
start_at http://localhost:8000/plain-a g --parallel 2 --ahead 1 --record /mnt/g/used.json
sums g > /dev/null
ok "an address that pins nothing is said" "$(grep -c 'the address pins nothing' /mnt/g/log)" 1
ok "pieces unchecked is said" "$(grep -c 'unchecked (an index without checksums)' /mnt/g/log)" 1
ok "a piece cut short is not used" "$(grep -c 'piece 20: 1000 bytes, expected 1048576' /mnt/g/log | sed 's/[1-9][0-9]*/some/')" some
ok "the hot list, each piece once" "$(grep -c 'hot: [01] pieces' /mnt/g/log)" 1
ok "what was read, recorded" "$(sleep 1.2; python3 -c "import json; print(len(json.load(open('/mnt/g/used.json'))) > 3)")" True
finish g > /mnt/g/ended
ok "unmounted, it ends" "$(cat /mnt/g/ended)" 0
for h in b c d; do start_at http://127.0.0.1:8000/plain-$h h$h; sleep 1; done
ok "an empty hot list" "$(grep -c 'hot: 0 pieces' /mnt/hb/log)" 1
ok "a hot list that is none, or too big: not used" "$(cat /mnt/hc/log /mnt/hd/log | grep -c 'hot:')" 0
ok "files read without one" "$(sums hc | sha256sum)" "$(cat /t/expected.txt | sha256sum)"
for d in hb hc hd; do finish $d > /dev/null; done

# 10. Pinned (by --pin) to an index without checksums; an index that is none: it stops. A hot list a pinned index does
# not name: not used.
ok "pinned without checksums stops it" "$(stops http://127.0.0.1:8000/plain-b /mnt/c/file /mnt/c/cache --pin "$(sha256sum /srv/plain-b/index.json | cut -c1-16)" | grep -c "^1 .*a pinned index without its pieces' checksums")" 1
mkdir -p /srv/plain-bad && echo '{}' > /srv/plain-bad/index.json
ok "an index that is none stops it" "$(stops http://127.0.0.1:8000/plain-bad /mnt/c/file /mnt/c/cache | grep -c '^1 .*index.json: no size')" 1
NOHOT=$(/t/reader pack /t/small.sqsh /srv/nohot --piece 1048576 | head -1 | xargs basename)
echo '[0]' > /srv/nohot/$NOHOT/hot.json
start_at http://127.0.0.1:8000/nohot/$NOHOT i; sleep 1
ok "a hot list the pinned index does not name is not used" "$(grep -c 'hot.json is not named by the pinned index' /mnt/i/log)" 1
finish i > /dev/null

# 11. A cache it cannot make: its folder, its file, its file's size; a mount point that is not there.
ok "a cache folder that cannot be" "$(stops "http://127.0.0.1:8000/out/$PIN" /mnt/c/file /t/test.sh/cache | grep -c '^1 .*/t/test.sh/cache: ')" 1
mkdir -p /mnt/k/cache/pieces
ok "a cache file that cannot be" "$(stops "http://127.0.0.1:8000/out/$PIN" /mnt/c/file /mnt/k/cache | grep -c '^1 .*/mnt/k/cache/pieces: ')" 1
mkdir -p /mnt/l/cache && ln -sf /dev/full /mnt/l/cache/pieces
ok "a cache file that cannot be made its size" "$(stops "http://127.0.0.1:8000/out/$PIN" /mnt/c/file /mnt/l/cache | grep -c '^1 .*/mnt/l/cache/pieces: ')" 1
ok "a mount point that is not there" "$(stops "http://127.0.0.1:8000/out/$PIN" /mnt/nowhere /mnt/c/cache2 | grep -c '^1 .*mount: ')" 1

# 12. A cache on a disk that fills: the oldest pieces give way and files still read; on one too small for a piece, it is
# kept nowhere (said).
mkdir -p /mnt/m/cache /mnt/n/cache && mount -t tmpfs -o size=8m tmpfs /mnt/m/cache && mount -t tmpfs -o size=512k tmpfs /mnt/n/cache
start out m --keep-free-mb 0
ok "a disk that fills: files read" "$(sums m | sha256sum)" "$(cat /t/expected.txt | sha256sum)"
ok "pieces gave way" "$( [ "$(stat_of m dropped)" -ge 1 ] && echo yes)" yes
finish m > /dev/null
start out n --keep-free-mb 0
ok "a disk too small for a piece is said" "$(grep -c 'kept nowhere' /mnt/n/log | sed 's/[1-9][0-9]*/some/')" some
finish n > /dev/null

# 13. A slow address and one fetcher: a read of a piece queued to be fetched later has it fetched now; files read.
cat > /srv/slow.py <<'PY'
import functools, http.server, time
class Slow(http.server.SimpleHTTPRequestHandler):
    def do_GET(self): time.sleep(0.15); super().do_GET()
    def log_message(self, *a): pass
http.server.ThreadingHTTPServer(('127.0.0.1', 8002), functools.partial(Slow, directory='/srv')).serve_forever()
PY
(python3 /srv/slow.py >/dev/null 2>&1 &)
sleep 1
start_at "http://127.0.0.1:8002/out/$PIN" o --parallel 1 --ahead 4
ok "one slow fetcher: files read" "$(sums o | sha256sum)" "$(cat /t/expected.txt | sha256sum)"

echo "passed $pass, failed $fail"
[ $fail = 0 ] || { for d in a b c d e f g hb hc hd i m n o; do echo "--- $d"; tail -5 /mnt/$d/log 2>/dev/null; cat /mnt/$d/mounterr 2>/dev/null; done; }
