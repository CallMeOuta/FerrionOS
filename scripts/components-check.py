#!/usr/bin/env python3
import hashlib
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RECORD = ROOT / "components" / "upstream.toml"
COMMENT_CONFIG = ROOT / "scripts" / "comment-check.config.json"
COMMENT_BASELINE = ROOT / "scripts" / "comment-check.baseline.json"
NAMING_BASELINE = ROOT / "scripts" / "naming-check.baseline.jsonl"
HEX40 = re.compile(r"[0-9a-f]{40}")
HEX64 = re.compile(r"[0-9a-f]{64}")
INNER_ALLOW_UNSAFE = re.compile(
    r"#!\[allow\([^]]*\b(?:clippy::)?undocumented_unsafe_blocks\b[^]]*\)\]",
    re.DOTALL,
)
ALLOW_UNSAFE_TOKEN = re.compile(r"\b(?:clippy::)?undocumented_unsafe_blocks\b")
FN_DECL = re.compile(r"\bfn\b")
UNSAFE_BLOCK = re.compile(r"\bunsafe\s*\{")
OWN_ADDRESS = "github.com/CallMeOuta/FerrionOS"
USAGE = f"""Checks the source trees under components/ against {RECORD.relative_to(ROOT)} (ADRs 0027
and 0047): every tree is recorded with its upstream repository and revision, its license files
are present and unchanged, its recipe builds it in place and carries no patch file (unless
kind = "subset"), and no tree holds a submodule link. For kind = "subset": every include path's
git object id matches the commit that first added the tree, nothing tracked exists under the
tree outside include and added, and no recipe is required. For every tree beyond the four core
components: each FerrionOS helper carrying #[allow(clippy::undocumented_unsafe_blocks)] holds
exactly one unsafe block and matches its file's line count in components/<name>.unsafe.md. For
base, whose drivers live in ferrion-hardware: the tree has no drivers/ folder, the drivers
recipe names ferrion-hardware by a full revision and carries no patch file, its lock file takes
no crate of FerrionOS's from the network, and no Cargo manifest of this repository depends on
ferrion-hardware or on upstream's base repository.

Usage: {sys.argv[0]} [--rebaseline <name>]
  --rebaseline <name>  rewrite the entries of components/<name>/ in the comment and naming
                       baselines to the tree as it is (for kind = "subset", include paths only).
                       Only two commits may do this: one that merges an upstream revision
                       (scripts/component-upstream.sh merge), and one that imports a tree or
                       brings in a patch written before the trees moved into this repository
Exit code 1 when a check fails."""


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def tracked(*args):
    out = subprocess.run(["git", "ls-files", *args], cwd=ROOT, check=True, capture_output=True, text=True)
    return out.stdout.splitlines()


def covered_by(rel_path, allowed):
    return any(rel_path == entry or rel_path.startswith(entry + "/") for entry in allowed)


def valid_rel_path(value):
    if not isinstance(value, str) or not value or value.startswith("/") or value.endswith("/"):
        return False
    parts = value.split("/")
    return all(part and part not in (".", "..") for part in parts)


def check_subset(name, entry):
    failures = []
    include = entry.get("include")
    if not isinstance(include, dict) or not include:
        failures.append(f"{name}: subset entry must define a non-empty [include] table")
        return failures
    added = entry.get("added", [])
    if not isinstance(added, list):
        failures.append(f"{name}: subset added must be a list of relative paths")
        return failures
    for rel, oid in sorted(include.items()):
        if not valid_rel_path(rel):
            failures.append(f"{name}: invalid include path {rel!r}")
        if not HEX40.fullmatch(str(oid)):
            failures.append(f"{name}: include {rel} does not record a 40-hex git object id")
    for rel in added:
        if not valid_rel_path(rel):
            failures.append(f"{name}: invalid added path {rel!r}")
    if failures:
        return failures

    licenses = entry.get("licenses", {})
    anchor = "LICENSE" if "LICENSE" in licenses else (sorted(licenses)[0] if licenses else "LICENSE")
    anchor_rel = f"components/{name}/{anchor}"
    log = subprocess.run(
        ["git", "log", "--diff-filter=A", "--reverse", "--format=%H", "--", anchor_rel],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    commits = [line.strip() for line in log.stdout.splitlines() if line.strip()]
    if not commits:
        shallow = subprocess.run(
            ["git", "rev-parse", "--is-shallow-repository"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        if shallow == "true":
            failures.append(f"{name}: history is shallow; the subset proof needs the import commit")
        else:
            failures.append(f"{name}: could not find import commit adding {anchor_rel}")
        return failures
    import_commit = commits[0]

    for rel, expected_oid in sorted(include.items()):
        rev = subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", f"{import_commit}:components/{name}/{rel}"],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        actual_oid = rev.stdout.strip() if rev.returncode == 0 else ""
        if actual_oid != expected_oid:
            found = actual_oid or "missing"
            failures.append(
                f"{name}: include {rel} at import commit {import_commit[:9]} is {found}, expected {expected_oid}"
            )

    prefix = f"components/{name}/"
    allowed = list(include.keys()) + list(added)
    for full in sorted(set(tracked(f"components/{name}"))):
        if not full.startswith(prefix):
            continue
        rel_file = full[len(prefix):]
        if not covered_by(rel_file, allowed):
            failures.append(f"{name}: path {rel_file} is outside include and added")
    return failures


def scan_ferrion_unsafe(path, rel, name):
    failures = []
    text = path.read_text(errors="replace")
    if INNER_ALLOW_UNSAFE.search(text):
        failures.append(
            f"{name}: {rel}: inner allow of undocumented_unsafe_blocks is forbidden; allow on a single helper"
        )
    lines = text.splitlines()
    count = 0
    i = 0
    while i < len(lines):
        stripped = lines[i].lstrip(" ")
        if not stripped.startswith("#[allow("):
            i += 1
            continue
        indent = len(lines[i]) - len(stripped)
        attr_end = i
        while "]" not in lines[attr_end] and attr_end + 1 < len(lines):
            attr_end += 1
        attr_text = " ".join(lines[i : attr_end + 1])
        if not ALLOW_UNSAFE_TOKEN.search(attr_text):
            i = attr_end + 1
            continue
        j = attr_end + 1
        while j < len(lines):
            nxt = lines[j].strip()
            if not nxt:
                j += 1
                continue
            if nxt.startswith("#["):
                while "]" not in lines[j] and j + 1 < len(lines):
                    j += 1
                j += 1
                continue
            break
        if j >= len(lines) or not FN_DECL.search(lines[j]):
            failures.append(
                f"{name}: {rel}:{i + 1}: undocumented_unsafe_blocks allow must sit on a helper function"
            )
            i = attr_end + 1
            continue
        k = j
        while k < len(lines) and "{" not in lines[k]:
            k += 1
        if k >= len(lines):
            failures.append(f"{name}: {rel}:{i + 1}: allowed helper has no function body")
            i = attr_end + 1
            continue
        brace_idx = lines[k].index("{")
        first_chunk = lines[k][brace_idx:]
        if first_chunk.count("{") == first_chunk.count("}"):
            body_text = first_chunk
        else:
            close_prefix = " " * indent + "}"
            m = k + 1
            while m < len(lines) and not lines[m].startswith(close_prefix):
                m += 1
            if m >= len(lines):
                failures.append(
                    f"{name}: {rel}:{i + 1}: could not find closing brace for allowed helper"
                )
                i = attr_end + 1
                continue
            body_text = "\n".join([first_chunk, *lines[k + 1 : m + 1]])
        n_unsafe = len(UNSAFE_BLOCK.findall(body_text))
        if n_unsafe != 1:
            failures.append(
                f"{name}: {rel}:{i + 1}: allowed helper must contain exactly one unsafe block (found {n_unsafe})"
            )
        count += 1
        i = attr_end + 1
    return count, failures


def check_unsafe_justifications(name):
    failures = []
    prefix = f"components/{name}/"
    blocks_by_file = {}
    for full in sorted(set(tracked(f"components/{name}"))):
        if not full.startswith(prefix) or not full.endswith(".rs"):
            continue
        rel = full[len(prefix):]
        path = ROOT / full
        if not path.is_file():
            continue
        count, file_failures = scan_ferrion_unsafe(path, rel, name)
        failures.extend(file_failures)
        if count > 0:
            blocks_by_file[rel] = count

    doc_rel = f"components/{name}.unsafe.md"
    doc_path = ROOT / doc_rel
    doc_tracked = bool(tracked(doc_rel))
    doc_by_file = {}
    if doc_path.is_file() and not doc_tracked:
        failures.append(f"{name}: {doc_rel} exists on disk but is not tracked")
    if doc_tracked and doc_path.is_file():
        for lineno, raw in enumerate(doc_path.read_text(errors="replace").splitlines(), 1):
            line = raw.strip()
            if not line:
                continue
            parts = [part.strip() for part in line.split("|", 2)]
            if len(parts) != 3 or any(not part for part in parts):
                failures.append(f"{name}: {doc_rel}:{lineno}: expected 'path | function | invariant'")
                continue
            rel_path, _, _ = parts
            if not valid_rel_path(rel_path) or ":" in rel_path or rel_path.startswith("components/"):
                failures.append(f"{name}: {doc_rel}:{lineno}: invalid relative path {rel_path!r}")
                continue
            doc_by_file[rel_path] = doc_by_file.get(rel_path, 0) + 1
    elif blocks_by_file:
        total = sum(blocks_by_file.values())
        failures.append(f"{name}: {doc_rel} is missing for {total} FerrionOS unsafe block(s)")
        return failures

    for rel in sorted(set(blocks_by_file) | set(doc_by_file)):
        actual = blocks_by_file.get(rel, 0)
        documented = doc_by_file.get(rel, 0)
        if actual == 0:
            failures.append(
                f"{name}: {doc_rel} lists {documented} block(s) for {rel}, which has no FerrionOS unsafe blocks"
            )
        elif actual > documented:
            failures.append(
                f"{name}: {rel} has {actual} FerrionOS unsafe block(s), but {doc_rel} lists {documented} (missing justification)"
            )
        elif actual < documented:
            failures.append(
                f"{name}: {rel} has {actual} FerrionOS unsafe block(s), but {doc_rel} lists {documented} (stale justification)"
            )
    return failures


def check():
    failures = []
    record = tomllib.loads(RECORD.read_text())
    on_disk = sorted(p.name for p in (ROOT / "components").iterdir() if p.is_dir())
    for name in on_disk:
        if name not in record:
            failures.append(f"components/{name} has no entry in {RECORD.relative_to(ROOT)}")
    for name, entry in sorted(record.items()):
        tree = ROOT / "components" / name
        if not tree.is_dir():
            failures.append(f"{name}: components/{name} is not a directory")
            continue
        if not str(entry.get("url", "")).startswith("https://"):
            failures.append(f"{name}: url is not an https address")
        if not HEX40.fullmatch(str(entry.get("rev", ""))):
            failures.append(f"{name}: rev is not a full commit hash")
        licenses = entry.get("licenses", {})
        if not licenses:
            failures.append(f"{name}: no license file is recorded")
        for file, digest in sorted(licenses.items()):
            path = tree / file
            if not path.is_file():
                failures.append(f"{name}: license file {file} is missing")
            elif sha256(path) != digest:
                failures.append(f"{name}: license file {file} changed; review it and record its sha256")
        for sub, sub_entry in sorted(entry.get("submodules", {}).items()):
            if not (tree / sub).is_dir():
                failures.append(f"{name}: the tree of submodule {sub} is missing")
            if not HEX40.fullmatch(str(sub_entry.get("rev", ""))) or not str(sub_entry.get("url", "")).startswith("https://"):
                failures.append(f"{name}: submodule {sub} needs an https url and a full commit hash")
        for archive, archive_entry in sorted(entry.get("archives", {}).items()):
            if not (tree / str(archive_entry.get("path", ""))).is_dir():
                failures.append(f"{name}: the tree of archive {archive} is missing")
            if not HEX64.fullmatch(str(archive_entry.get("sha256", ""))) or not str(archive_entry.get("url", "")).startswith("https://"):
                failures.append(f"{name}: archive {archive} needs an https url and a sha256")
        dropped = entry.get("dropped", [])
        if not isinstance(dropped, list) or any(
            not isinstance(item, str) or not item.strip("/") or item.startswith("/") or ".." in Path(item).parts
            for item in dropped
        ):
            failures.append(f"{name}: dropped must be a list of relative paths")
        else:
            for item in dropped:
                rel = item.rstrip("/")
                if tracked(f"components/{name}/{rel}"):
                    failures.append(f"components/{name}/{rel}: listed in dropped of {RECORD.relative_to(ROOT)} and must stay absent")
        kind = entry.get("kind", "component")
        if kind == "subset":
            failures.extend(check_subset(name, entry))
        elif kind == "component":
            recipe_dir = ROOT / "recipes" / "core" / name
            source = tomllib.loads((recipe_dir / "recipe.toml").read_text()).get("source", {})
            if set(source) != {"path"} or (recipe_dir / source["path"]).resolve() != tree.resolve():
                failures.append(f"{name}: recipes/core/{name}/recipe.toml must have [source] path = "
                                f'"../../../components/{name}" and no other source key')
            for patch in tracked(f"recipes/core/{name}/*.patch"):
                failures.append(f"{patch}: a change to {name} is a commit in components/{name}, not a patch file")
            if "split" in entry:
                failures.extend(check_split(name, entry["split"]))
        else:
            failures.append(f"{name}: unknown kind {kind!r}")
            continue
        if kind != "component":
            failures.extend(check_unsafe_justifications(name))
    links = subprocess.run(["git", "ls-files", "-s", "components"], cwd=ROOT, check=True, capture_output=True, text=True)
    for line in links.stdout.splitlines():
        if line.startswith("160000 "):
            failures.append(f"{line.split(chr(9), 1)[1]}: a submodule link; bring its tree in instead")
    return failures


def check_split(name, split):
    failures = []
    moved = f"components/{name}/{split['moved']}"
    if tracked(moved):
        failures.append(f"{moved} holds files: a driver's code lives in {split['repository']}")
    for old, new in sorted(split.get("renamed", {}).items()):
        if not (ROOT / "components" / name / new).is_dir():
            failures.append(f"{name}: {old} of upstream is kept as components/{name}/{new}, which is missing")
    recipe_dir = ROOT / "recipes" / "core" / "drivers"
    source = {}
    for _ in range(2):
        if (recipe_dir / "recipe.toml").is_file():
            source = tomllib.loads((recipe_dir / "recipe.toml").read_text()).get("source", {})
        if set(source) != {"same_as"}:
            break
        recipe_dir = recipe_dir / source["same_as"]
    recipe_dir = ROOT / "recipes" / "core" / "drivers"
    if set(source) != {"git", "rev"} or source["git"] != split["repository"] or not HEX40.fullmatch(str(source["rev"])):
        failures.append(f"the drivers recipe, or the one recipe it shares a source with, must have [source] git = "
                        f"\"{split['repository']}\", a full commit hash as rev and no other source key")
    for patch in tracked("recipes/core/drivers/*.patch"):
        failures.append(f"{patch}: a change to a driver is a commit in {split['repository']}, not a patch file")
    lock = recipe_dir / "Cargo.lock"
    if not tracked("recipes/core/drivers/Cargo.lock"):
        failures.append("recipes/core/drivers/Cargo.lock is missing: write it with scripts/drivers-lock.sh write")
    elif OWN_ADDRESS in lock.read_text():
        failures.append("recipes/core/drivers/Cargo.lock takes a crate from this repository's own address; "
                        "the drivers are built against components/ (scripts/drivers-lock.sh)")
    hardware = split["repository"].removesuffix(".git").removeprefix("https://")
    upstream = tomllib.loads(RECORD.read_text())[name]["url"].removesuffix(".git")
    from_upstream = re.compile(r'git\s*=\s*"' + re.escape(upstream) + r'(\.git)?/?"')
    for manifest in tracked("*Cargo.toml"):
        for number, line in enumerate((ROOT / manifest).read_text(errors="replace").splitlines(), 1):
            if hardware in line or "core/drivers/source" in line:
                failures.append(f"{manifest}:{number}: depends on a crate of ferrion-hardware's; the code here is under "
                                "the MIT license and reaches a driver only through its scheme (ADR 0047)")
            elif from_upstream.search(line):
                failures.append(f"{manifest}:{number}: takes a crate from upstream's {name} repository; "
                                f"it is under components/{name}")
    return failures


def rebaseline(name):
    record = tomllib.loads(RECORD.read_text())
    entry = record[name]
    prefix = f"components/{name}/"
    include = list(entry.get("include", {}).keys()) if entry.get("kind") == "subset" else None

    def in_scope(path):
        if not path.startswith(prefix):
            return False
        if include is None:
            return True
        return covered_by(path[len(prefix):], include)

    sys.path.insert(0, str(ROOT / "scripts"))
    from comment_extract import inventory, read_config
    records, _, files = inventory(str(ROOT), read_config(str(COMMENT_CONFIG)))
    lines = {path: set() for path in files if in_scope(path)}
    for rec in records:
        if rec["file"] in lines and not rec["exempt"]:
            lines[rec["file"]].update(range(rec["line"], rec["end_line"] + 1))
    baseline = {path: count for path, count in json.loads(COMMENT_BASELINE.read_text()).items()
                if not path.startswith(prefix)}
    baseline.update({path: len(numbers) for path, numbers in lines.items()})
    COMMENT_BASELINE.write_text(json.dumps(baseline, sort_keys=True, indent=2) + "\n")
    found = subprocess.run([sys.executable, str(ROOT / "scripts" / "naming-check.py"), "--json"], cwd=ROOT,
                           capture_output=True, text=True)
    kept = [raw for raw in NAMING_BASELINE.read_text().splitlines()
            if raw.strip() and not json.loads(raw)["path"].startswith(prefix)]
    added = [raw for raw in found.stdout.splitlines()
             if raw.strip() and json.loads(raw)["class"] == "fail" and in_scope(json.loads(raw)["path"])]
    rows = sorted(kept + added, key=lambda raw: (json.loads(raw)["path"], json.loads(raw)["line"]))
    NAMING_BASELINE.write_text("".join(raw + "\n" for raw in rows))
    print(f"{name}: {len(lines)} comment baseline entries and {len(added)} naming baseline lines rewritten")


def main(argv):
    if argv[:1] == ["--rebaseline"] and len(argv) == 2:
        if argv[1] not in tomllib.loads(RECORD.read_text()):
            print(f"{argv[1]} is not recorded in {RECORD.relative_to(ROOT)}", file=sys.stderr)
            return 2
        rebaseline(argv[1])
        return 0
    if argv:
        print(USAGE, file=sys.stderr)
        return 0 if argv[0] in ("-h", "--help") else 2
    failures = check()
    for failure in failures:
        print(f"FAIL {failure}")
    if not failures:
        print("Every tree under components/ matches its record.")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
