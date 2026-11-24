#!/usr/bin/env bash

set -euo pipefail

MAX_ARGS=64

die() {
    echo "fc-cache-container: $*" >&2
    exit 1
}

usage() {
    cat >&2 <<EOF
Runs the FerrionOS build container's fc-cache against an image root.

Usage:
  $0 --sysroot <dir> [fc-cache options...]

Environment:
  IMAGE_TAG          Podman image tag to run (required outside a container)
  PODMAN_VOLUME_FLAG Optional volume flag such as :Z from mk/podman.mk
  FONTCONFIG_FILE    Forwarded to fc-cache when set
  FONTCONFIG_PATH    Forwarded to fc-cache when set
EOF
    exit "${1:-2}"
}

if [ "$#" -eq 0 ]; then
    usage 2
fi

case "$1" in
    -h|--help)
        usage 0
        ;;
esac

if [ -f /run/.containerenv ]; then
    exec fc-cache "$@"
fi

if [ "$#" -gt "$MAX_ARGS" ]; then
    die "too many arguments ($# > $MAX_ARGS)"
fi

sysroot=""
args=("$@")
idx=0
while [ "$idx" -lt "$#" ]; do
    arg="${args[$idx]}"
    case "$arg" in
        --sysroot)
            idx=$((idx + 1))
            if [ "$idx" -ge "$#" ]; then
                die "--sysroot requires a directory argument"
            fi
            sysroot="${args[$idx]}"
            ;;
        --sysroot=*)
            sysroot="${arg#--sysroot=}"
            ;;
    esac
    idx=$((idx + 1))
done

if [ -z "$sysroot" ]; then
    usage 2
fi

if [ ! -d "$sysroot" ]; then
    die "sysroot directory does not exist: $sysroot"
fi

case "$sysroot" in
    *:*)
        die "sysroot path must not contain ':': $sysroot"
        ;;
esac

image_tag="${IMAGE_TAG:?IMAGE_TAG must be set}"
vol_opts="rw"
vol_flag="${PODMAN_VOLUME_FLAG:-}"
vol_flag="${vol_flag#:}"
if [ -n "$vol_flag" ]; then
    vol_opts="${vol_opts},${vol_flag}"
fi

podman_args=(
    run
    --rm
    --network=none
    --volume "${sysroot}:${sysroot}:${vol_opts}"
)

if [ "${FONTCONFIG_FILE+x}" = "x" ]; then
    podman_args+=(--env "FONTCONFIG_FILE=${FONTCONFIG_FILE}")
fi

if [ "${FONTCONFIG_PATH+x}" = "x" ]; then
    podman_args+=(--env "FONTCONFIG_PATH=${FONTCONFIG_PATH}")
fi

exec podman "${podman_args[@]}" "$image_tag" fc-cache "$@"
