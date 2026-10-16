#!/bin/bash
# Docker with superci-layers as containerd's snapshotter and content store, against Docker as it comes: the same
# work done both ways (pull, run, build, commit, save, load, push, pull back, prune), and what can be compared
# (image ids, what `docker save` writes) the same. Then superci-layers and containerd started again mid-way, and
# everything still there. In a privileged container (see Dockerfile), with superci-layers at /superci-layers.
set -Eeuo pipefail
trap 'echo "FAILED at line $LINENO: $BASH_COMMAND" >&2; for f in /var/log/superci-layers.log /var/log/containerd.log /var/log/dockerd.log; do echo "== $f"; tail -30 "$f"; done' ERR
T() { echo "== $(date +%T) $*"; }
CONF=/etc/containerd/config.toml
cp "$CONF" /root/config.stock.toml
mkdir -p /etc/docker /root/out

start() { # $1: stock | superci
  if [ "$1" = superci ]; then
    /superci-layers --root=/var/lib/superci-layers --socket=/run/superci-layers.sock >> /var/log/superci-layers.log 2>&1 &
    for i in $(seq 100); do [ -S /run/superci-layers.sock ] && break; sleep 0.1; done
  fi
  containerd >> /var/log/containerd.log 2>&1 &
  for i in $(seq 100); do [ -S /run/containerd/containerd.sock ] && break; sleep 0.1; done
  dockerd --containerd=/run/containerd/containerd.sock >> /var/log/dockerd.log 2>&1 &
  for i in $(seq 200); do docker info >/dev/null 2>&1 && break; sleep 0.1; done
  docker info >/dev/null
}
stop() {
  pkill dockerd || true; for i in $(seq 100); do pgrep -x dockerd >/dev/null || break; sleep 0.1; done
  pkill -x containerd || true; for i in $(seq 100); do pgrep -x containerd >/dev/null || break; sleep 0.1; done
  pkill -x superci-layers || true; for i in $(seq 100); do pgrep -x superci-layers >/dev/null || break; sleep 0.1; done
}
fresh() {
  stop
  for m in $(awk '$2 ~ "^/var/lib/(docker|containerd|superci-layers)" {print $2}' /proc/mounts | sort -r); do umount -l "$m" || true; done
  rm -rf /var/lib/docker /var/lib/containerd /var/lib/superci-layers /run/containerd /run/superci-layers.sock
}

# The same work each way; what it gives, written to /root/out/$1.
work() {
  local o=/root/out/$1
  T "$1: pull and run"
  docker pull -q alpine:3.22 > /dev/null
  docker pull -q busybox:1.37 > /dev/null
  docker run --rm alpine:3.22 cat /etc/alpine-release > "$o.run"
  docker image inspect -f '{{.Id}}' alpine:3.22 busybox:1.37 > "$o.ids"
  T "$1: build (two stages, a cache mount, a copy from another image)"
  mkdir -p /root/ctx && cd /root/ctx
  printf 'hello\n' > hello.txt
  cat > Dockerfile <<'DF'
FROM alpine:3.22 AS build
RUN --mount=type=cache,target=/var/cache/apk apk add --no-cache jq > /dev/null && jq --version > /jq-version
RUN mkdir -p /out && echo built > /out/built && ln -s built /out/link && rm -f /etc/motd
FROM busybox:1.37
COPY --from=build /out /out
COPY --from=build /jq-version /jq-version
COPY hello.txt /hello.txt
RUN echo "$(cat /hello.txt) $(cat /out/link)" > /result && rm /hello.txt
DF
  docker build -q -t e2e/built:1 . > /dev/null
  docker run --rm e2e/built:1 sh -c 'cat /result /jq-version; ls /hello.txt 2>&1 || true' > "$o.built"
  cd /root
  T "$1: commit, diff, cp, export"
  docker run --name c1 alpine:3.22 sh -c 'echo changed > /etc/changed; rm /etc/hostname 2>/dev/null; mkdir -p /new/dir' 
  docker diff c1 | sort > "$o.diff"
  docker commit c1 e2e/committed:1 > /dev/null
  docker cp c1:/etc/changed "$o.cp"
  docker export c1 | tar -t | sort > "$o.export"
  docker rm c1 > /dev/null
  docker run --rm e2e/committed:1 cat /etc/changed > "$o.committed"
  T "$1: save, remove, load"
  docker save alpine:3.22 -o /root/alpine-$1.tar
  sha256sum < /root/alpine-$1.tar > "$o.save"
  # (A build's layers carry the times its files were made: what a save holds is compared, not its digests.)
  docker save e2e/built:1 | tar -t | sed -E 's/[0-9a-f]{64}/DIGEST/' | sort | uniq -c > "$o.save-built"
  docker rmi -f e2e/committed:1 > /dev/null
  docker save e2e/built:1 -o /root/built.tar && docker rmi e2e/built:1 > /dev/null && docker load -q -i /root/built.tar > /dev/null
  docker run --rm e2e/built:1 cat /result >> "$o.built"
  T "$1: push to a registry and pull it back"
  docker run -d --name reg -p 127.0.0.1:5000:5000 registry:2 > /dev/null
  for i in $(seq 50); do curl -q -sf http://127.0.0.1:5000/v2/ > /dev/null && break; sleep 0.2; done
  docker tag e2e/built:1 127.0.0.1:5000/built:1
  docker push -q 127.0.0.1:5000/built:1 > /dev/null
  docker rmi 127.0.0.1:5000/built:1 e2e/built:1 > /dev/null
  docker pull -q 127.0.0.1:5000/built:1 > /dev/null
  docker run --rm 127.0.0.1:5000/built:1 cat /result >> "$o.built"
  docker rm -f reg > /dev/null
  T "$1: volumes, disk usage, prune"
  docker volume create v1 > /dev/null
  docker run --rm -v v1:/v alpine:3.22 sh -c 'echo kept > /v/f'
  docker run --rm -v v1:/v busybox:1.37 cat /v/f > "$o.volume"
  docker system df --format '{{.Type}}' > "$o.df"
  docker image prune -a -f > /dev/null
  docker images -q | wc -l > "$o.left"
}

T "stock: Docker as it comes"
fresh; start stock
[ "$(docker info -f '{{.Driver}}')" = overlayfs ] || { echo "stock Docker's driver is $(docker info -f '{{.Driver}}')" >&2; exit 1; }
work stock

T "superci: containerd's content store off, superci-layers for snapshots and content"
fresh
sed 's|^disabled_plugins = \[\(.*\)\]|disabled_plugins = [\1, "io.containerd.content.v1.content"]|; s|\["", |[|' /root/config.stock.toml > "$CONF"
grep -q 'io.containerd.content.v1.content' "$CONF" || echo 'disabled_plugins = ["io.containerd.content.v1.content"]' >> "$CONF"
printf '\n[proxy_plugins]\n  [proxy_plugins.superci]\n    type = "snapshot"\n    address = "/run/superci-layers.sock"\n  [proxy_plugins.superci-content]\n    type = "content"\n    address = "/run/superci-layers.sock"\n' >> "$CONF"
echo '{"storage-driver": "superci", "features": {"containerd-snapshotter": true}}' > /etc/docker/daemon.json
start superci
[ "$(docker info -f '{{.Driver}}')" = superci ] || { echo "Docker's driver is $(docker info -f '{{.Driver}}')" >&2; exit 1; }
work superci

T "the same both ways"
bad=0
for f in run ids built diff cp export committed save save-built volume df left; do
  if cmp -s "/root/out/stock.$f" "/root/out/superci.$f"; then echo "  same: $f"; else echo "  DIFFERENT: $f"; diff "/root/out/stock.$f" "/root/out/superci.$f" | head -10 || true; bad=1; fi
done
[ -n "$(ls /var/lib/superci-layers/overlay/snapshots)" ] || { echo "no snapshot was made in superci-layers" >&2; bad=1; }
[ -n "$(ls /var/lib/containerd/io.containerd.content.v1.content/blobs/sha256 2>/dev/null)" ] || { echo "no blob was written through superci-layers" >&2; bad=1; }
[ $bad = 0 ]

T "superci-layers started again while Docker runs; then everything started again"
docker pull -q alpine:3.22 > /dev/null
docker run -d --name live alpine:3.22 sleep 600 > /dev/null
pkill -x superci-layers; sleep 0.5
/superci-layers --root=/var/lib/superci-layers --socket=/run/superci-layers.sock >> /var/log/superci-layers.log 2>&1 &
for i in $(seq 100); do [ -S /run/superci-layers.sock ] && break; sleep 0.1; done; sleep 0.5
docker run --rm alpine:3.22 echo "a container after superci-layers came back" 
docker exec live echo "the running container kept going"
docker rm -f live > /dev/null
stop; start superci
docker run --rm alpine:3.22 echo "after containerd, Docker and superci-layers all started again"
docker system prune -a -f > /dev/null
docker images -q | wc -l | grep -qx 0
T "passed"
