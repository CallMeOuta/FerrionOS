#!/usr/bin/env bash
# Assembles the Linux layer's root tree from the Debian packages that lock.tsv pins
# (plans/linux-compat.md, "The Linux root"). It runs in the cookbook's container for the recipe,
# and on the build host to give the tree the expected outputs are recorded in.
#
#     assemble.sh LOCK DEB_DIRECTORY EMPTY_OUTPUT_DIRECTORY
#
# Every package file must be in DEB_DIRECTORY under its pool file name with the lock's SHA-256;
# nothing is fetched here. Packages are unpacked, not installed: no maintainer script runs. The
# FerrionOS overlay adds the /usr merge links and the mount points, removes documentation,
# manuals and translations, clears set-user-id and set-group-id bits (never honored on FerrionOS)
# and sets every time to the lock's source date, so the tree is the same on every run.
set -euo pipefail
if [[ $# != 3 ]]; then
    echo "usage: $0 LOCK DEB_DIRECTORY EMPTY_OUTPUT_DIRECTORY" >&2
    exit 2
fi
lock=$(realpath "$1")
debs=$(realpath "$2")
root=$(realpath -m "$3")
epoch=$(awk '$2 == "source-date-epoch" { print $3 }' "$lock")
[[ -n "$epoch" ]] || { echo "$lock names no source-date-epoch" >&2; exit 1; }
mkdir "$root"

while IFS=$'\t' read -r name version arch path size sha256; do
    [[ -z "$name" || "$name" == \#* ]] && continue
    case "$arch" in
        amd64 | all) ;;
        *) echo "$name: architecture $arch is not taken into the root" >&2; exit 1 ;;
    esac
    deb="$debs/$(basename "$path")"
    printf '%s  %s\n' "$sha256" "$deb" | sha256sum --check --quiet -
    [[ $(stat -c %s "$deb") == "$size" ]] || { echo "$deb: size differs from the lock" >&2; exit 1; }
    dpkg-deb --fsys-tarfile "$deb" | tar -x --no-same-owner -C "$root" -f -
done < "$lock"

cd "$root"
# trixie's packages install under /usr; the root links the old top-level names to it.
for dir in bin sbin lib lib64; do
    if [[ ! -e "$dir" && ! -L "$dir" ]]; then
        ln -s "usr/$dir" "$dir"
    fi
done
rm -rf usr/share/doc usr/share/man usr/share/info usr/share/locale usr/share/lintian usr/share/bug
mkdir -p dev proc sys run home root var/tmp tmp
chmod 1777 tmp var/tmp
if [[ -d usr/share/ca-certificates ]]; then
    mkdir -p etc/ssl/certs
    find usr/share/ca-certificates -name '*.crt' | LC_ALL=C sort | while read -r crt; do
        cat "$crt"
        [[ $(tail -c 1 "$crt" | od -An -c | tr -d ' ') == '\n' ]] || echo
    done > etc/ssl/certs/ca-certificates.crt
fi
chmod -R ug-s .
find . -exec touch -h -d "@$epoch" {} +
