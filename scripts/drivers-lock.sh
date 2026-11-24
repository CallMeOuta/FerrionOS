#!/usr/bin/env bash
set -euo pipefail

usage="usage: scripts/drivers-lock.sh patch [<repository root>] | write | check
The drivers recipe builds ferrion-hardware's drivers against this repository's own crates
(ADR 0047): ferrion-hardware names them as git dependencies on FerrionOS's repository, and the
build replaces each with the crate under components/.
  patch  prints the Cargo [patch] table that does it, with paths under <repository root>
  write  writes recipes/core/drivers/Cargo.lock, the lock file that build uses: ferrion-hardware's
         own Cargo.lock at the fetched revision with those crates taken from components/. When
         the crates under components/ need other packages than that lock holds, cargo resolves
         again, which reads FerrionOS's repository at the address ferrion-hardware names
  check  exit code 1 when a fresh write would change that file
write and check need the drivers fetched at the recipe's revision (make f.drivers) and cargo; they
fail when a crate would still come from FerrionOS's repository by git, which means the list of
crates in this script lacks it."
root=$(cd "$(dirname "$0")/.." && pwd)
url=https://github.com/CallMeOuta/FerrionOS.git
crates="base/audiod base/config base/daemon base/graphics-ipc base/inputd base/scheme-utils relibc/redox-ioctl"
recipe="$root/recipes/core/drivers"

patch_table() {
    echo "[patch.\"$url\"]"
    for crate in $crates; do
        printf '%s = { path = "%s/components/%s" }\n' "${crate##*/}" "$1" "$crate"
    done
}

case "${1:-}" in
    patch)
        patch_table "${2:-$root}"
        ;;
    write | check)
        pinned=$(python3 -c 'import sys, tomllib
from pathlib import Path
folder = Path(sys.argv[1])
source = tomllib.loads((folder / "recipe.toml").read_text())["source"]
if "same_as" in source:
    source = tomllib.loads((folder / source["same_as"] / "recipe.toml").read_text())["source"]
print(source["rev"])' "$recipe")
        fetched=$(git -C "$recipe/source" rev-parse HEAD 2> /dev/null || true)
        [ "$fetched" = "$pinned" ] || { echo "recipes/core/drivers/source is not at $pinned: run make f.drivers" >&2; exit 1; }
        work=$(mktemp -d "$root/build/drivers-lock.XXXXXX")
        trap 'rm -r "$work"' EXIT
        cp -r "$recipe/source/drivers" "$work/drivers"
        patch_table "$root" > "$work/ferrion.toml"
        awk -v source="git+$url" '
            index($0, "source = \"" source) == 1 { next }
            {
                while ((start = index($0, " (" source)) > 0) {
                    $0 = substr($0, 1, start - 1) substr($0, start + index(substr($0, start), ")"))
                }
                print
            }' "$recipe/source/drivers/Cargo.lock" > "$work/drivers/Cargo.lock"
        if ! (cd "$work/drivers" && cargo metadata --format-version 1 --locked --config "$work/ferrion.toml" > /dev/null 2> "$work/locked.log"); then
            echo "the crates under components/ need packages ferrion-hardware's lock lacks; resolving again" >&2
            (cd "$work/drivers" && cargo metadata --format-version 1 --config "$work/ferrion.toml" > /dev/null)
        fi
        if grep -q "git+$url" "$work/drivers/Cargo.lock"; then
            echo "these crates would come from $url and not from components/; add them to the list in $0:" >&2
            grep -B2 "git+$url" "$work/drivers/Cargo.lock" | grep '^name' >&2
            exit 1
        fi
        if [ "$1" = write ]; then
            cp "$work/drivers/Cargo.lock" "$recipe/Cargo.lock"
            echo "recipes/core/drivers/Cargo.lock written for ferrion-hardware at $pinned"
        else
            cmp "$work/drivers/Cargo.lock" "$recipe/Cargo.lock" \
                || { echo "recipes/core/drivers/Cargo.lock is stale: run $0 write" >&2; exit 1; }
            echo "recipes/core/drivers/Cargo.lock is current for ferrion-hardware at $pinned"
        fi
        ;;
    *)
        echo "$usage" >&2
        [ "${1:-}" = -h ] || [ "${1:-}" = --help ] || exit 2
        ;;
esac
