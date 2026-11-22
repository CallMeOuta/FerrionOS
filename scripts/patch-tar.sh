#!/usr/bin/env bash

set -euo pipefail

if [ "${1:-}" = "" ] || [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]
then
    echo "Generates a 'redox.patch' for a tar-based recipe"
    echo ""
    echo "Usage: $0 <recipe-name>"
    exit 1
fi

RECIPE_PATH=$(make "find.$1")
RECIPE_PATH=${RECIPE_PATH//$'\r'/}

if [ ! -d "$RECIPE_PATH/source" ]; then
    echo "Error: 'source' directory not found" >&2
    exit 1
fi

if [ ! -f "$RECIPE_PATH/source.tar" ]; then
    echo "Error: 'source.tar' not found" >&2
    exit 1
fi

if [ -e "$RECIPE_PATH/source-new" ]; then
    echo "Error: '$RECIPE_PATH/source-new' already exists; inspect or remove it first" >&2
    exit 1
fi

WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/ferrion-patch-tar.XXXXXX")
cleanup() {
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

export TZ=UTC
set -x

SOURCE_ABS=$(cd "$RECIPE_PATH/source" && pwd)
mkdir "$WORK_DIR/source"
tar xf "$RECIPE_PATH/source.tar" -C "$WORK_DIR/source" --strip-components=1
ln -s "$SOURCE_ABS" "$WORK_DIR/source-new"

DIFF_STATUS=0
(cd "$WORK_DIR" && diff -ruwN source source-new) > "$WORK_DIR/redox.patch" || DIFF_STATUS=$?
if [ "$DIFF_STATUS" -gt 1 ]; then
    exit "$DIFF_STATUS"
fi
mv "$WORK_DIR/redox.patch" "$RECIPE_PATH/redox.patch"
