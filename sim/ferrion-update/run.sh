#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
cargo build --release --locked --quiet
seeds=${1:?usage: run.sh SEEDS STEPS [FIRST_SEED]}
steps=${2:?usage: run.sh SEEDS STEPS [FIRST_SEED]}
exec target/release/ferrion-update-sim "$seeds" "$steps" "${3:-0}"
