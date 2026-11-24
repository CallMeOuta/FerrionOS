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
OWN_ADDRESS = "github.com/CallMeOuta/FerrionOS"
USAGE = f"""Checks the source trees under components/ against {RECORD.relative_to(ROOT)} (ADRs 0027
and 0047): every tree is recorded with its upstream repository and revision, its license files
are present and unchanged, its recipe builds it in place and carries no patch file, and no tree
holds a submodule link. For base, whose drivers live in ferrion-hardware: the tree has no
drivers/ folder, the drivers recipe names ferrion-hardware by a full revision and carries no
patch file, its lock file takes no crate of FerrionOS's from the network, and no Cargo manifest
of this repository depends on ferrion-hardware or on upstream's base repository.

Usage: {sys.argv[0]} [--rebaseline <name>]
  --rebaseline <name>  rewrite the entries of components/<name>/ in the comment and naming
                       baselines to the tree as it is. Only two commits may do this: one that
                       merges an upstream revision (scripts/component-upstream.sh merge), and one
                       that brings in a patch written before the trees moved into this repository
Exit code 1 when a check fails."""


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def tracked(*args):
    out = subprocess.run(["git", "ls-files", *args], cwd=ROOT, check=True, capture_output=True, text=True)
    return out.stdout.splitlines()


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
        recipe_dir = ROOT / "recipes" / "core" / name
        source = tomllib.loads((recipe_dir / "recipe.toml").read_text()).get("source", {})
        if set(source) != {"path"} or (recipe_dir / source["path"]).resolve() != tree.resolve():
            failures.append(f"{name}: recipes/core/{name}/recipe.toml must have [source] path = "
                            f'"../../../components/{name}" and no other source key')
        for patch in tracked(f"recipes/core/{name}/*.patch"):
            failures.append(f"{patch}: a change to {name} is a commit in components/{name}, not a patch file")
        if "split" in entry:
            failures.extend(check_split(name, entry["split"]))
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
    prefix = f"components/{name}/"
    sys.path.insert(0, str(ROOT / "scripts"))
    from comment_extract import inventory, read_config
    records, _, files = inventory(str(ROOT), read_config(str(COMMENT_CONFIG)))
    lines = {path: set() for path in files if path.startswith(prefix)}
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
             if raw.strip() and json.loads(raw)["class"] == "fail" and json.loads(raw)["path"].startswith(prefix)]
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
