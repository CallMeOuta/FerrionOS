#!/usr/bin/env bash
set -eu

WORK="${1:-/tmp/ferrion-linux-driver-l0}"
HERE="$(cd "$(dirname "$0")" && pwd)"

LINUX_VERSIONS="6.6 6.10 6.12 6.14 6.18"
LINUX_PATCHES="6.7 6.8 6.9"
FREEBSD_SRC_REV=c9f9c4fe47bf17bb39d7ff3a200aadab6b69e60e
DRM_KMOD_REV=98be24dc99949e0c040e02def52d125e6ddad179
DRM_KMOD_612_REV=5a12132886cc17b8180532b0dcc94ca64694a8b7
OPENBSD_SRC_REV=bcd8432a8efc37418af5fdbbeadb735a884de630
LINUX_FIRMWARE_REV=a1ebb5ad7eae3caca60b8b3db5792cf43339ba58
KERNEL_CDN=https://cdn.kernel.org/pub/linux/kernel/v6.x

sha_of() {
    case "$1" in
        linux-6.6.tar.xz) echo d926a06c63dd8ac7df3f86ee1ffc2ce2a3b81a2d168484e76b5b389aba8e56d0 ;;
        linux-6.10.tar.xz) echo 774698422ee54c5f1e704456f37c65c06b51b4e9a8b0866f34580d86fef8e226 ;;
        linux-6.12.tar.xz) echo b1a2562be56e42afb3f8489d4c2a7ac472ac23098f1ef1c1e40da601f54625eb ;;
        linux-6.14.tar.xz) echo a294b683e7b161bb0517bb32ec7ed1d2ea7603dfbabad135170ed12d00c47670 ;;
        linux-6.18.tar.xz) echo 9106a4605da9e31ff17659d958782b815f9591ab308d03b0ee21aad6c7dced4b ;;
        patch-6.7.xz) echo 494b0c225cbc9c5c61262f5b180b651c915859ecd09ae012177eb77bcd30b52e ;;
        patch-6.8.xz) echo 952e18e65bec243805a0416e0f916fd3be1cf71c05e0ccf386fd5a853a1cf536 ;;
        patch-6.9.xz) echo 4edb18ea03173df90a55fb0799090b981718a26c5b29d0bc6a5bb82931f15169 ;;
        *) echo "no pinned sha256 for $1" >&2; exit 1 ;;
    esac
}

download() {
    local name="$1" dir="$2"
    mkdir -p "$dir"
    if [ ! -s "$dir/$name" ]; then
        curl -sS -m 1800 -o "$dir/$name.part" "$KERNEL_CDN/$name"
        mv "$dir/$name.part" "$dir/$name"
    fi
    echo "$(sha_of "$name")  $dir/$name" | sha256sum -c - >/dev/null
}

checkout() {
    local dir="$1" url="$2" rev="$3"
    shift 3
    if [ ! -d "$dir/.git" ]; then
        mkdir -p "$dir"
        git -C "$dir" init -q
        git -C "$dir" remote add origin "$url"
    fi
    if [ "$#" -gt 0 ]; then
        git -C "$dir" sparse-checkout init --no-cone
        git -C "$dir" sparse-checkout set --no-cone "$@"
        git -C "$dir" fetch -q --depth 1 --filter=blob:none origin "$rev"
    else
        git -C "$dir" fetch -q --depth 1 origin "$rev"
    fi
    git -C "$dir" checkout -q --detach "$rev"
    test "$(git -C "$dir" rev-parse HEAD)" = "$rev"
}

mkdir -p "$WORK"

for v in $LINUX_VERSIONS; do
    download "linux-$v.tar.xz" "$WORK/tar"
    if [ ! -d "$WORK/linux-$v/include" ]; then
        mkdir -p "$WORK/linux-$v"
        tar -xJf "$WORK/tar/linux-$v.tar.xz" -C "$WORK/linux-$v" --strip-components=1 --wildcards \
            "linux-$v/include" \
            "linux-$v/arch/x86/include" \
            "linux-$v/drivers/dma-buf" \
            "linux-$v/drivers/gpu/drm/Makefile" \
            "linux-$v/drivers/gpu/drm/Kconfig" \
            "linux-$v/drivers/gpu/drm/*.c" \
            "linux-$v/drivers/gpu/drm/*.h" \
            "linux-$v/drivers/gpu/drm/amd" \
            "linux-$v/drivers/gpu/drm/display" \
            "linux-$v/drivers/gpu/drm/ttm" \
            "linux-$v/drivers/gpu/drm/scheduler" \
            "linux-$v/drivers/gpu/drm/virtio" \
            "linux-$v/drivers/gpu/drm/nouveau" \
            "linux-$v/drivers/virtio" \
            "linux-$v/MAINTAINERS" \
            "linux-$v/Makefile"
        if tar -tJf "$WORK/tar/linux-$v.tar.xz" "linux-$v/drivers/gpu/drm/clients/Makefile" >/dev/null 2>&1; then
            tar -xJf "$WORK/tar/linux-$v.tar.xz" -C "$WORK/linux-$v" --strip-components=1 "linux-$v/drivers/gpu/drm/clients"
        fi
    fi
    if [ ! -s "$WORK/exports-$v.txt" ]; then
        python3 "$HERE/count.py" exports "$WORK/tar/linux-$v.tar.xz" "$WORK/exports-$v.txt"
    fi
done

for v in $LINUX_PATCHES; do
    download "patch-$v.xz" "$WORK/patches"
done

checkout "$WORK/freebsd-src" https://github.com/freebsd/freebsd-src "$FREEBSD_SRC_REV" \
    /sys/compat/linuxkpi/ /sys/sys/ /sys/libkern/ /sys/x86/include/ /sys/amd64/include/ /sys/vm/ /sys/dev/pci/
checkout "$WORK/drm-kmod" https://github.com/freebsd/drm-kmod "$DRM_KMOD_REV"
checkout "$WORK/drm-kmod-6.12" https://github.com/freebsd/drm-kmod "$DRM_KMOD_612_REV"
checkout "$WORK/openbsd-src" https://github.com/openbsd/src "$OPENBSD_SRC_REV" \
    /sys/dev/pci/drm/ '!/sys/dev/pci/drm/i915/' '!/sys/dev/pci/drm/radeon/' '!/sys/dev/pci/drm/amd/include/asic_reg/' \
    /sys/sys/ /sys/lib/libkern/ /sys/arch/amd64/include/ /sys/uvm/ /sys/dev/pci/pcivar.h /sys/dev/pci/pcireg.h \
    /sys/dev/pci/pcidevs.h
checkout "$WORK/linux-firmware" https://gitlab.com/kernel-firmware/linux-firmware.git "$LINUX_FIRMWARE_REV" \
    /WHENCE /LICENSES/LICENSE.amdgpu

echo "inputs ready in $WORK"
