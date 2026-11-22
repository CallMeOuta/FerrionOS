#!/usr/bin/env bash

set -euo pipefail

MOUNT_TIMEOUT_S=10
MOUNT_POLL_STEPS=100
UNMOUNT_RETRIES=20

die() {
    echo "image-mount: $*" >&2
    exit 1
}

usage() {
    cat >&2 <<EOF
Manages the FUSE mount lifecycle of a FerrionOS disk image.

Usage:
  $0 mount <redoxfs> <image> <mount-dir>
  $0 unmount <mount-dir>
  $0 unmount-if-mounted <mount-dir>
  $0 require-unmounted <mount-dir> <action>
  $0 with-mount <redoxfs> <image> <mount-dir> -- <command...>

Environment:
  FUMOUNT            unmount command (default: fusermount3 -u or fusermount -u)
  IMAGE_MOUNT_SIGN   set to 1 to run verified-boot-sign after unmounting an owned mount
  IMAGE_MOUNT_MAKE   make command used for verified-boot-sign (default: make)
EOF
    exit "${1:-2}"
}

is_mounted() {
    local dir="$1"
    [ -d "$dir" ] || return 1
    if command -v mountpoint >/dev/null 2>&1; then
        mountpoint -q "$dir"
        return $?
    fi
    local real
    real="$(cd "$dir" && pwd -P)"
    mount | grep -Fq " on ${real} "
}

read_state() {
    local dir="$1"
    if is_mounted "$dir"; then
        echo "user"
    elif [ -d "$dir" ]; then
        echo "stale-dir"
    elif [ -e "$dir" ]; then
        die "$dir exists and is not a directory"
    else
        echo "unmounted"
    fi
}

assert_state() {
    case "$1" in
        unmounted|ours|user|stale-dir) ;;
        *) die "assertion failed: invalid mount state '$1'" ;;
    esac
}

resolve_fumount() {
    local cmd="${FUMOUNT:-}"
    if [ -z "$cmd" ]; then
        if command -v fusermount3 >/dev/null 2>&1; then
            cmd="fusermount3 -u"
        else
            cmd="fusermount -u"
        fi
    fi
    read -r -a FUMOUNT_CMD <<< "$cmd"
}

do_mount() {
    local redoxfs="$1" image="$2" dir="$3"
    local state step
    state="$(read_state "$dir")"
    assert_state "$state"
    case "$state" in
        user)
            die "$dir is already mounted"
            ;;
        stale-dir)
            rmdir "$dir" || die "$dir is not mounted and is not empty; refusing to remove"
            ;;
        unmounted)
            ;;
    esac
    [ -x "$redoxfs" ] || die "$redoxfs is not executable"
    [ -f "$image" ] || die "$image is not a file"
    mkdir -p "$dir"
    if ! "$redoxfs" --flush-on-unmount "$image" "$dir"; then
        rmdir "$dir" 2>/dev/null || true
        die "failed to mount $image at $dir"
    fi
    for ((step = 0; step < MOUNT_POLL_STEPS; step++)); do
        if is_mounted "$dir"; then
            echo "image-mount: mounted $image at $dir"
            return 0
        fi
        sleep 0.1
    done
    rmdir "$dir" 2>/dev/null || true
    die "timed out after ${MOUNT_TIMEOUT_S}s waiting for $dir to mount"
}

do_unmount() {
    local dir="$1" quiet="${2:-0}"
    local state step
    state="$(read_state "$dir")"
    assert_state "$state"
    case "$state" in
        unmounted)
            [ "$quiet" = 1 ] || echo "image-mount: $dir is not mounted"
            return 0
            ;;
        stale-dir)
            rmdir "$dir" || die "$dir is not mounted and is not empty; refusing to remove"
            [ "$quiet" = 1 ] || echo "image-mount: removed stale directory $dir"
            return 0
            ;;
        user)
            resolve_fumount
            sync
            for ((step = 0; step < UNMOUNT_RETRIES; step++)); do
                "${FUMOUNT_CMD[@]}" "$dir" 2>/dev/null || true
                if ! is_mounted "$dir"; then
                    break
                fi
                sleep 0.1
            done
            if is_mounted "$dir"; then
                die "could not unmount $dir after ${UNMOUNT_RETRIES} attempts"
            fi
            rmdir "$dir" || die "unmounted $dir but could not remove directory"
            [ "$quiet" = 1 ] || echo "image-mount: unmounted $dir"
            return 0
            ;;
    esac
}

cmd_unmount_if_mounted() {
    local dir="$1"
    local state
    state="$(read_state "$dir")"
    assert_state "$state"
    case "$state" in
        user)
            echo "image-mount: $dir is mounted; unmounting"
            do_unmount "$dir" 1
            ;;
        stale-dir)
            rmdir "$dir" || die "$dir is not mounted and is not empty; refusing to remove"
            ;;
        unmounted)
            ;;
    esac
}

cmd_require_unmounted() {
    local dir="$1" action="$2"
    if is_mounted "$dir"; then
        die "$dir is mounted; unmount it before $action"
    fi
}

cmd_with_mount() {
    [ $# -ge 5 ] || usage 2
    local redoxfs="$1" image="$2" dir="$3" sep="$4"
    shift 4
    [ "$sep" = "--" ] || usage 2
    [ $# -ge 1 ] || usage 2

    MOUNT_STATE="$(read_state "$dir")"
    assert_state "$MOUNT_STATE"
    CMD_STATUS=1
    WITH_MOUNT_DIR="$dir"

    cleanup_with_mount() {
        local exit_code="$?"
        trap - EXIT INT TERM
        assert_state "$MOUNT_STATE"
        case "$MOUNT_STATE" in
            ours)
                if ! do_unmount "$WITH_MOUNT_DIR" 0; then
                    exit 1
                fi
                if [ "$CMD_STATUS" -eq 0 ] && [ "${IMAGE_MOUNT_SIGN:-0}" = 1 ]; then
                    if ! "${IMAGE_MOUNT_MAKE:-make}" verified-boot-sign; then
                        exit 1
                    fi
                fi
                ;;
            user)
                echo "image-mount: not unmounting $WITH_MOUNT_DIR (already mounted before this command)" >&2
                if [ "${IMAGE_MOUNT_SIGN:-0}" = 1 ]; then
                    echo "image-mount: run make verified-boot-sign after unmounting, or verified boot will refuse a pushed kernel or initfs" >&2
                fi
                ;;
            unmounted|stale-dir)
                ;;
        esac
        exit "$exit_code"
    }

    trap cleanup_with_mount EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM

    case "$MOUNT_STATE" in
        unmounted|stale-dir)
            do_mount "$redoxfs" "$image" "$dir"
            if ! is_mounted "$dir"; then
                die "assertion failed: $dir is not mounted after mount succeeded"
            fi
            MOUNT_STATE="ours"
            ;;
        user)
            ;;
    esac
    assert_state "$MOUNT_STATE"
    if "$@"; then
        CMD_STATUS=0
    else
        CMD_STATUS=$?
    fi
    exit "$CMD_STATUS"
}

[ $# -ge 1 ] || usage 2
case "$1" in
    -h|--help)
        usage 0
        ;;
    mount)
        [ $# -eq 4 ] || usage 2
        do_mount "$2" "$3" "$4"
        ;;
    unmount)
        [ $# -eq 2 ] || usage 2
        do_unmount "$2" 0
        ;;
    unmount-if-mounted)
        [ $# -eq 2 ] || usage 2
        cmd_unmount_if_mounted "$2"
        ;;
    require-unmounted)
        [ $# -eq 3 ] || usage 2
        cmd_require_unmounted "$2" "$3"
        ;;
    with-mount)
        shift
        cmd_with_mount "$@"
        ;;
    *)
        usage 2
        ;;
esac
