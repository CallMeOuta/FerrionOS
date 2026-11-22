#!/usr/bin/env bash
set -euo pipefail
tree=$(realpath "${1:?usage: record.sh ROOT_TREE}")
here=$(cd "$(dirname "$0")" && pwd)
files="$here/files"
epoch=1794787200

cd "$files"
echo "4721f391ed90541fddacab5acf947aa0d3dc7d27b2e1e8eda2be8970586c3274  six-1.17.0-py2.py3-none-any.whl" | sha256sum -c --quiet -
tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner --mode=a+rX,u+w,go-w \
    -cf fixtures.tar lc4.sh lc4.py six-1.17.0-py2.py3-none-any.whl

scratch=$(mktemp -d /tmp/ferrion-lc4-record.XXXXXX)
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
    /bin/dash -c '/bin/dash -c "mkdir /tmp/f && tar -xf - -C /tmp/f && /bin/dash /tmp/f/lc4.sh" >/out/stdout 2>/out/stderr; echo $? >/out/status' \
    <fixtures.tar
cp "$scratch/out/stdout" expected.stdout
cp "$scratch/out/stderr" expected.stderr
cp "$scratch/out/status" expected.status
sha256sum lc4.sh lc4.py six-1.17.0-py2.py3-none-any.whl fixtures.tar expected.stdout expected.stderr expected.status >SHA256SUMS
cat expected.stdout expected.stderr expected.status
