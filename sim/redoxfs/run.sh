#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
cargo build --release --locked --quiet
if [ "${1:-}" = replay ]; then
    exec target/release/redoxfs-sim replay "${2:?usage: run.sh replay FILE}"
fi
if [ "${1:-}" = maps ]; then
    shift
    exec target/release/redoxfs-sim maps "$@"
fi
if [ "${1:-}" = keyroll ]; then
    shift
    exec target/release/redoxfs-sim keyroll "$@"
fi
if [ "${1:-}" = ringdrain ]; then
    shift
    exec target/release/redoxfs-sim ringdrain "$@"
fi
if [ "${1:-}" = readonly ]; then
    shift
    exec target/release/redoxfs-sim readonly "$@"
fi
seeds=${1:?usage: run.sh SEEDS OPS [--first SEED] [--threads N] [--shrink N] [--flush-on-unmount], or run.sh replay FILE}
ops=${2:?usage: run.sh SEEDS OPS [--first SEED] [--threads N] [--shrink N] [--flush-on-unmount], or run.sh replay FILE}
shift 2
exec target/release/redoxfs-sim run "$seeds" "$ops" "$@"
