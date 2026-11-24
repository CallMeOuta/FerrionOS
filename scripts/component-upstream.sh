#!/usr/bin/env bash
set -euo pipefail

usage="Brings a later upstream revision into a tree under components/ by a three-way merge.

usage: scripts/component-upstream.sh fetch <name>
       scripts/component-upstream.sh check <name> [<rev>]
       scripts/component-upstream.sh merge <name> <rev>

fetch  fetches upstream's branches into refs/upstream/<name>/ of this clone; they are never pushed
check  merges, in memory, what upstream changed between the recorded revision and <rev>
       (default: upstream's default branch) into HEAD, prints each conflicting path and exits 1
       when there is one
merge  does the same merge in the index and the working tree, writes <rev> into
       components/upstream.toml and leaves the result uncommitted

The record of each tree's upstream repository and revision is components/upstream.toml.
Paths listed under a component's dropped key in that record are stripped from both
upstream trees before merging so deleted upstream project files do not conflict or return.
base is merged without the files [base.split] of that record sends to ferrion-hardware, and
with its kept files at their new paths (ADR 0047); the same upstream revision is merged into
ferrion-hardware's drivers/ by its own scripts/drivers-upstream.sh.
Run from a clean checkout."

case "${1:-}" in
    -h|--help) echo "$usage"; exit 0 ;;
esac
[ $# -ge 2 ] || { echo "$usage" >&2; exit 2; }
action=$1 name=$2 target=${3:-}
cd "$(git rev-parse --show-toplevel)"
record=components/upstream.toml
prefix=components/$name

read_record() {
    python3 -c 'import sys, tomllib
node = tomllib.load(open(sys.argv[1], "rb"))
for key in sys.argv[2:]:
    node = node.get(key, {})
print(node if isinstance(node, str) else "\n".join(node))' "$record" "$@"
}

write_record() {
    python3 -c 'import sys
path, old, new = sys.argv[1:]
text = open(path, encoding="utf-8").read()
assert text.count(old) == 1, f"{old} is not written exactly once in {path}"
open(path, "w", encoding="utf-8").write(text.replace(old, new))' "$record" "$1" "$2"
}

url=$(read_record "$name" url)
old=$(read_record "$name" rev)
[ -n "$url" ] && [ -n "$old" ] || { echo "$record has no url and rev for $name" >&2; exit 1; }
submodules=$(read_record "$name" submodules)
moved=$(read_record "$name" split moved)
mapfile -t dropped < <(read_record "$name" dropped | sed '/^$/d')

fetch_refs() {
    git fetch --quiet --no-tags "$1" "+refs/heads/*:refs/upstream/$2/*" "+HEAD:refs/upstream/$2/HEAD"
}

fetch_all() {
    local sub
    fetch_refs "$url" "$name"
    for sub in $submodules; do
        fetch_refs "$(read_record "$name" submodules "$sub" url)" "$name-$sub"
    done
}

wrap() {
    local commit=$1 index links link path laid
    index=$(mktemp -u)
    GIT_INDEX_FILE=$index git read-tree --prefix="$prefix/" "$commit"
    links=$(GIT_INDEX_FILE=$index git ls-files -s -- "$prefix" | awk '$1 == "160000" { print $2 ":" $4 }')
    for link in $links; do
        path=${link#*:}
        link=${link%%:*}
        git cat-file -e "$link^{commit}" || { echo "$path: $link is in no fetched branch" >&2; exit 1; }
        GIT_INDEX_FILE=$index git update-index --force-remove -- "$path"
        GIT_INDEX_FILE=$index git read-tree --prefix="$path/" "$link"
    done
    if [ -n "$moved" ]; then
        laid=$(mktemp -u)
        GIT_INDEX_FILE=$index git ls-files -s -- "$prefix" | sed "s#\t$prefix/#\t#" \
            | python3 scripts/base-split.py layout | sed "s#\t#\t$prefix/#" \
            | GIT_INDEX_FILE=$laid git update-index --index-info
        rm -f "$index"
        index=$laid
    fi
    if [ "${#dropped[@]}" -gt 0 ]; then
        laid=$(mktemp -u)
        GIT_INDEX_FILE=$index git ls-files -s -- "$prefix" \
            | python3 -c 'import sys
prefix = sys.argv[1] + "/"
dropped = tuple(p.rstrip("/") for p in sys.argv[2:] if p)
for line in sys.stdin:
    path = line.rstrip("\n").split("\t", 1)[1]
    rel = path[len(prefix):] if path.startswith(prefix) else path
    if any(rel == d or rel.startswith(d + "/") for d in dropped):
        continue
    sys.stdout.write(line)' "$prefix" "${dropped[@]}" \
            | GIT_INDEX_FILE=$laid git update-index --index-info
        rm -f "$index"
        index=$laid
    fi
    GIT_INDEX_FILE=$index git write-tree
    rm -f "$index"
}

case "$action" in
    fetch)
        fetch_all
        ;;
    check|merge)
        [ "$action" = check ] || [ -n "$target" ] || { echo "$usage" >&2; exit 2; }
        fetch_all
        new=$(git rev-parse --verify --quiet "${target:-refs/upstream/$name/HEAD}^{commit}") \
            || { echo "$name: unknown revision ${target:-HEAD}" >&2; exit 1; }
        git merge-base --is-ancestor "$old" "$new" \
            || { echo "$name: $new does not descend from the recorded $old" >&2; exit 1; }
        base=$(git commit-tree -m "upstream $name $old" "$(wrap "$old")")
        theirs=$(git commit-tree -p "$base" -m "upstream $name $new" "$(wrap "$new")")
        echo "$name: $(git rev-list --count "$old..$new") upstream commits and" \
            "$(git diff --name-only "$old" "$new" | wc -l) changed files from $old to $new"
        [ -z "$moved" ] || echo "$name: $(git diff --name-only "$old" "$new" | python3 scripts/base-split.py filter drivers | wc -l)" \
            "of those files are ferrion-hardware's: merge $new there with scripts/drivers-upstream.sh"
        if [ "$action" = check ]; then
            if merged=$(git merge-tree --write-tree --name-only --no-messages --merge-base="$base" HEAD "$theirs"); then
                echo "$name: merges without a conflict"
            else
                echo "$merged" | tail -n +2 | sed 's/^/conflict: /'
                exit 1
            fi
        else
            [ -z "$(git status --porcelain)" ] || { echo "commit or stash your changes first" >&2; exit 1; }
            conflicts=0
            git cherry-pick --no-commit --allow-empty "$theirs" || conflicts=1
            write_record "$old" "$new"
            for sub in $submodules; do
                sub_old=$(read_record "$name" submodules "$sub" rev)
                sub_new=$(git rev-parse "$new:$sub")
                [ "$sub_old" = "$sub_new" ] || write_record "$sub_old" "$sub_new"
            done
            git add -- "$record"
            if [ "$conflicts" = 1 ]; then
                echo "$name: resolve the conflicts below, then git add each file:"
                git diff --name-only --diff-filter=U
            fi
            echo "$name: next, python3 scripts/components-check.py --rebaseline $name," \
                "the checks of docs/upstream.md, and one commit that names $old and $new"
        fi
        ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
esac
