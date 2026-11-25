#!/usr/bin/env bash

set -euo pipefail

MAX_ARGS=64

die() {
    echo "glib-compile-schemas-container: $*" >&2
    exit 1
}

usage() {
    cat >&2 <<EOF
Runs the FerrionOS build container's glib-compile-schemas against an image schemas directory.

Usage:
  $0 [glib-compile-schemas options...] <schemas-dir>

Environment:
  IMAGE_TAG          Podman image tag to run (required outside a container)
  PODMAN_VOLUME_FLAG Optional volume flag such as :Z from mk/podman.mk
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
    exec glib-compile-schemas "$@"
fi

if [ "$#" -gt "$MAX_ARGS" ]; then
    die "too many arguments ($# > $MAX_ARGS)"
fi

args=("$@")
schemas_dir="${args[$(( $# - 1 ))]}"

case "$schemas_dir" in
    -*)
        usage 2
        ;;
    *:*)
        die "schemas directory path must not contain ':': $schemas_dir"
        ;;
esac

if [ ! -d "$schemas_dir" ]; then
    die "schemas directory does not exist: $schemas_dir"
fi

image_tag="${IMAGE_TAG:?IMAGE_TAG must be set}"
vol_opts="rw"
vol_flag="${PODMAN_VOLUME_FLAG:-}"
vol_flag="${vol_flag#:}"
if [ -n "$vol_flag" ]; then
    vol_opts="${vol_opts},${vol_flag}"
fi

exec podman run \
    --rm \
    --network=none \
    --volume "${schemas_dir}:${schemas_dir}:${vol_opts}" \
    "$image_tag" \
    glib-compile-schemas "$@"
