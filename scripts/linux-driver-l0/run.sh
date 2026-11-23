#!/usr/bin/env bash
set -eu

WORK="${1:-/tmp/ferrion-linux-driver-l0}"
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/results"
mkdir -p "$OUT"

count() {
    local name="$1"
    shift
    python3 "$HERE/count.py" count --work "$WORK" --out "$OUT/$name.json" "$@" > "$OUT/$name.txt"
}

count amdgpu-6.10 --version 6.10 --linux "$WORK/linux-6.10" --drm-kmod "$WORK/drm-kmod-6.12" --stack amdgpu
count amdgpu-6.12 --version 6.12 --linux "$WORK/linux-6.12" --drm-kmod "$WORK/drm-kmod-6.12" --stack amdgpu
count amdgpu-6.14 --version 6.14 --linux "$WORK/linux-6.14" --stack amdgpu
count amdgpu-6.18 --version 6.18 --linux "$WORK/linux-6.18" --stack amdgpu --files-of-both-ports
count amdgpu-6.18-freebsd-files --version 6.18 --linux "$WORK/linux-6.18" --stack amdgpu
count amdgpu-6.18-everything --version 6.18 --linux "$WORK/linux-6.18" --stack amdgpu --every-configuration --whole-directories
count amdgpu-freebsd-port-6.12 --version 6.12 --linux "$WORK/linux-6.12" --set-root "$WORK/drm-kmod-6.12" --drm-kmod "$WORK/drm-kmod-6.12" --stack amdgpu --as-ported
count amdgpu-freebsd-port-6.14 --version 6.14 --linux "$WORK/linux-6.14" --set-root "$WORK/drm-kmod" --stack amdgpu --as-ported
count virtio-6.12 --version 6.12 --linux "$WORK/linux-6.12" --drm-kmod "$WORK/drm-kmod-6.12" --stack virtio
count virtio-6.18 --version 6.18 --linux "$WORK/linux-6.18" --stack virtio --files-of-both-ports
count nouveau-6.12 --version 6.12 --linux "$WORK/linux-6.12" --drm-kmod "$WORK/drm-kmod-6.12" --stack nouveau
count nouveau-6.18 --version 6.18 --linux "$WORK/linux-6.18" --stack nouveau --files-of-both-ports
count amdgpu-6.18-openbsd --version 6.18 --linux "$WORK/linux-6.18" --layer openbsd --stack amdgpu
count nouveau-6.18-openbsd --version 6.18 --linux "$WORK/linux-6.18" --layer openbsd --stack nouveau
count virtio-6.18-openbsd --version 6.18 --linux "$WORK/linux-6.18" --layer openbsd --stack virtio
count amdgpu-openbsd-port-6.18 --version 6.18 --linux "$WORK/linux-6.18" --layer openbsd --layout openbsd --set-root "$WORK/openbsd-src" --stack amdgpu --as-ported

python3 "$HERE/count.py" compare "$OUT/amdgpu-6.12.json" "$OUT/amdgpu-6.18.json" > "$OUT/amdgpu-6.12-to-6.18.txt"
python3 "$HERE/count.py" extra "$OUT/amdgpu-6.18.json" "$OUT/nouveau-6.18.json" > "$OUT/nouveau-beyond-amdgpu-6.18.txt"
python3 "$HERE/count.py" extra "$OUT/amdgpu-6.12.json" "$OUT/nouveau-6.12.json" > "$OUT/nouveau-beyond-amdgpu-6.12.txt"
python3 "$HERE/count.py" extra "$OUT/amdgpu-6.18.json" "$OUT/virtio-6.18.json" > "$OUT/virtio-beyond-amdgpu-6.18.txt"

echo "results in $OUT"
