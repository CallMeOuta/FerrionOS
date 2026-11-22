#!/usr/bin/env bash
set -euo pipefail
tree=$(realpath "${1:?usage: record.sh ROOT_TREE APT_CACHE}")
apt_cache=$(realpath "${2:?usage: record.sh ROOT_TREE APT_CACHE}")
here=$(cd "$(dirname "$0")" && pwd)
files="$here/files"
epoch=1794787200

cd "$files"
podman run --rm --network=none \
    --volume "$apt_cache:/apt:ro" \
    --volume "$files:/work" \
    docker.io/library/debian:trixie bash -euo pipefail -c '
rm -f /etc/apt/sources.list.d/debian.sources
printf "%s\n" "deb [check-valid-until=no signed-by=/usr/share/keyrings/debian-archive-keyring.pgp] file:/apt trixie main" > /etc/apt/sources.list
apt-get -o Acquire::Check-Valid-Until=false update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
    gcc=4:14.2.0-1 gcc-14=14.2.0-19 binutils=2.44-3 libc6-dev=2.41-12+deb13u4 >/dev/null
dpkg-query -W -f="\${Package}\t\${Version}\t\${Architecture}\n" binutils gcc gcc-14 libc6-dev > /tmp/actual.tsv
diff -u /work/toolchain.tsv /tmp/actual.tsv
export LC_ALL=C TZ=UTC SOURCE_DATE_EPOCH=1794787200
cd /work
for name in hello-dyn threads spawn signals thread-reg; do
    gcc -O2 -Wl,--build-id=none -pthread -o "$name" "${name}.c"
done
'

tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner --mode=a+rX,u+w,go-w \
    -cf fixtures.tar hello-dyn threads spawn signals thread-reg lc3.sh

scratch=$(mktemp -d /tmp/ferrion-lc3-record.XXXXXX)
trap 'podman unshare rm -rf "$scratch"' EXIT
cp -a "$tree" "$scratch/root"
etc="$scratch/root/etc"
for f in passwd group hostname hosts nsswitch.conf; do rm -f "$etc/$f"; done
printf 'root:x:0:0:root:/root:/bin/sh\nuser:x:1000:1000:user:/home/user:/bin/sh\nnobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin\n' >"$etc/passwd"
printf 'root:x:0:\nuser:x:1000:\nnogroup:x:65534:\n' >"$etc/group"
printf 'localhost\n' >"$etc/hostname"
printf '127.0.0.1\tlocalhost\n127.0.1.1\tlocalhost\n::1\tlocalhost\n' >"$etc/hosts"
printf 'passwd: files\ngroup: files\nshadow: files\nhosts: files\n' >"$etc/nsswitch.conf"
mkdir -p "$scratch/root/home/user"
chmod 700 "$scratch/root/home/user"
podman unshare chown 1000:1000 "$scratch/root/home/user"

mkdir "$scratch/out"
chmod 777 "$scratch/out"
podman run --rm -i --user 1000:1000 --network=none \
    --tmpfs /tmp:rw,mode=1777 --volume "$scratch/out:/out" --workdir / --rootfs "$scratch/root" \
    /usr/bin/env -i HOME=/home/user SHELL=/bin/sh PATH=/usr/local/bin:/usr/bin:/bin \
    /bin/dash -c '/bin/dash -c "mkdir /tmp/f && tar -xf - -C /tmp/f && /bin/dash /tmp/f/lc3.sh" >/out/stdout 2>/out/stderr; echo $? >/out/status' \
    <fixtures.tar
cp "$scratch/out/stdout" expected.stdout
cp "$scratch/out/stderr" expected.stderr
cp "$scratch/out/status" expected.status
sha256sum hello-dyn threads spawn signals thread-reg lc3.sh fixtures.tar expected.status expected.stderr expected.stdout >SHA256SUMS
cat expected.stdout expected.stderr expected.status
