#!/usr/bin/env bash
set -euo pipefail
tree=$(realpath "${1:?usage: record.sh ROOT_TREE}")
here=$(cd "$(dirname "$0")" && pwd)
files="$here/files"
epoch=1794787200

cd "$files"
tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner --mode=a+rX,u+w,go-w \
    -cf fixtures.tar lc5.sh lc5.py

scratch=$(mktemp -d /tmp/ferrion-lc5-record.XXXXXX)
trap 'podman unshare rm -rf "$scratch"' EXIT
cp -a "$tree" "$scratch/root"
etc="$scratch/root/etc"
for f in passwd group hostname hosts nsswitch.conf resolv.conf; do rm -f "$etc/$f"; done
printf 'root:x:0:0:root:/root:/bin/sh\nuser:x:1000:1000:user:/home/user:/bin/sh\nnobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin\n' >"$etc/passwd"
printf 'root:x:0:\nuser:x:1000:\nnogroup:x:65534:\n' >"$etc/group"
printf 'localhost\n' >"$etc/hostname"
printf '127.0.0.1\tlocalhost\n127.0.1.1\tlocalhost\n::1\tlocalhost\n' >"$etc/hosts"
printf 'passwd: files\ngroup: files\nshadow: files\nhosts: files dns\n' >"$etc/nsswitch.conf"
mkdir -p "$scratch/root/home/user"
chmod 700 "$scratch/root/home/user"
podman unshare chown 1000:1000 "$scratch/root/home/user"

for mode in online offline; do
    mkdir "$scratch/$mode"
    chmod 777 "$scratch/$mode"
    if [[ $mode == online ]]; then
        network=(--network=pasta)
    else
        : >"$scratch/resolv.conf"
        network=(--network=none --volume "$scratch/resolv.conf:/etc/resolv.conf:ro")
    fi
    podman run --rm -i --user 1000:1000 "${network[@]}" \
        --tmpfs /tmp:rw,mode=1777 --volume "$scratch/$mode:/out" --workdir / --rootfs "$scratch/root" \
        /usr/bin/env -i HOME=/home/user SHELL=/bin/sh PATH=/usr/local/bin:/usr/bin:/bin \
        /bin/dash -c "/bin/dash -c \"mkdir /tmp/f && tar -xf - -C /tmp/f && /bin/dash /tmp/f/lc5.sh $mode\" >/out/stdout 2>/out/stderr; echo \$? >/out/status" \
        <fixtures.tar
    for part in stdout stderr status; do
        cp "$scratch/$mode/$part" "$mode.$part"
    done
done
sha256sum lc5.sh lc5.py fixtures.tar online.stdout online.stderr online.status offline.stdout offline.stderr offline.status >SHA256SUMS
head -n 50 online.* offline.*
