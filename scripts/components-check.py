#!/usr/bin/env python3
import hashlib
import io
import json
import os
import re
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RECORD = ROOT / "components" / "upstream.toml"
COMMENT_CONFIG = ROOT / "scripts" / "comment-check.config.json"
COMMENT_BASELINE = ROOT / "scripts" / "comment-check.baseline.json"
NAMING_BASELINE = ROOT / "scripts" / "naming-check.baseline.jsonl"
HEX40 = re.compile(r"[0-9a-f]{40}")
HEX64 = re.compile(r"[0-9a-f]{64}")
MAX_ARCHIVE_BYTES = 64 * 1024 * 1024
MAX_ARCHIVE_UNPACKED_BYTES = 256 * 1024 * 1024
ARCHIVE_CHUNK_BYTES = 64 * 1024
ARCHIVE_TIMEOUT_S = 60
MAX_ARCHIVE_MEMBERS = 10_000
INNER_ALLOW_UNSAFE = re.compile(
    r"#!\[allow\([^]]*\b(?:clippy::)?undocumented_unsafe_blocks\b[^]]*\)\]",
    re.DOTALL,
)
ALLOW_UNSAFE_TOKEN = re.compile(r"\b(?:clippy::)?undocumented_unsafe_blocks\b")
FN_DECL = re.compile(r"\bfn\b")
UNSAFE_BLOCK = re.compile(r"\bunsafe\s*\{")
OWN_ADDRESS = "github.com/CallMeOuta/FerrionOS"
USAGE = f"""Checks the source trees under components/ against {RECORD.relative_to(ROOT)} (ADRs 0027
and 0047). The plain check stays offline: for component entries it checks the upstream repository
and revision, license file digests, dropped paths, and in-place core recipe with no patch file;
for kind = "subset" it checks every include path's git object id at the import commit, tracked
stray paths outside include and added, and license digests; for kind = "archives" it checks
format and presence only (https url, 64-hex sha256 syntax, dropped paths, per-archive license
file presence and sha256 digests, no unrecorded subdirectories, and no rev or core recipe). No
tree holds a submodule link. For every tree beyond the four core components: each FerrionOS
helper carrying #[allow(clippy::undocumented_unsafe_blocks)] holds exactly one unsafe block and
matches its file's line count in components/<name>.unsafe.md. For base, whose drivers live in
ferrion-hardware: the tree has no drivers/ folder, the drivers recipe names ferrion-hardware by a
full revision and carries no patch file, its lock file takes no crate of FerrionOS's from the
network, and no Cargo manifest of this repository depends on ferrion-hardware or on upstream's
base repository.

Usage: {sys.argv[0]} [--rebaseline <name> | --verify-archives [name]]
  --rebaseline <name>  rewrite the entries of components/<name>/ in the comment and naming
                       baselines to the tree as it is (for kind = "subset", include paths only).
                       Only two commits may do this: one that merges an upstream revision
                       (scripts/component-upstream.sh merge), and one that imports a tree, bumps
                       a vendored archive version, or brings in a patch written before the trees
                       moved into this repository
  --verify-archives [name]
                       online check: download each recorded archive of kind = "archives" entries
                       (or of <name>) with bounded size and timeout, verify its sha256 digest
                       against {RECORD.relative_to(ROOT)}, unpack it into a scratch directory
                       with bounded file count and total bytes (refusing absolute paths, ..,
                       symlinks and hard links), and compare every file byte-for-byte against the
                       import commit that recorded that sha256 in {RECORD.relative_to(ROOT)}
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


def check_licenses(label, base, licenses):
    failures = []
    if not isinstance(licenses, dict) or not licenses:
        failures.append(f"{label}: no license file is recorded")
        return failures
    base_resolved = base.resolve()
    for file, digest in sorted(licenses.items()):
        if not HEX64.fullmatch(str(digest)):
            failures.append(f"{label}: license file {file} needs a 64-hex sha256")
            continue
        path = (base / file).resolve()
        if not path.is_relative_to(base_resolved) or not path.is_file():
            failures.append(f"{label}: license file {file} is missing")
        elif sha256(path) != digest:
            failures.append(f"{label}: license file {file} changed; review it and record its sha256")
    return failures


def check_archives_entry(name, tree, entry):
    failures = []
    for forbidden in ("url", "rev", "submodules", "split"):
        if forbidden in entry:
            failures.append(f"{name}: kind = \"archives\" entry must not set {forbidden}")
    archives = entry.get("archives", {})
    if not isinstance(archives, dict) or not archives:
        failures.append(f"{name}: kind = \"archives\" requires at least one archive entry")
        return failures
    tree_resolved = tree.resolve()
    seen_paths = set()
    for archive, archive_entry in sorted(archives.items()):
        rel = str(archive_entry.get("path", "")).strip()
        archive_dir = (tree / rel).resolve() if rel else tree_resolved
        if not rel or archive_dir == tree_resolved or not archive_dir.is_relative_to(tree_resolved) or not archive_dir.is_dir():
            failures.append(f"{name}: the tree of archive {archive} is missing")
            continue
        if rel in seen_paths:
            failures.append(f"{name}: duplicate archive path {rel}")
        seen_paths.add(rel)
        if not HEX64.fullmatch(str(archive_entry.get("sha256", ""))) or not str(archive_entry.get("url", "")).startswith("https://"):
            failures.append(f"{name}: archive {archive} needs an https url and a sha256")
        failures.extend(check_licenses(f"{name} archive {archive}", archive_dir, archive_entry.get("licenses", {})))
    for child in sorted(tree.iterdir()):
        if child.is_dir() and child.name not in seen_paths:
            failures.append(f"{name}: components/{name}/{child.name} has no archive entry in {RECORD.relative_to(ROOT)}")
    if "licenses" in entry:
        failures.extend(check_licenses(name, tree, entry["licenses"]))
    return failures


def check_dropped(name, entry):
    failures = []
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
        failures.extend(check_dropped(name, entry))
        kind = entry.get("kind", "component")
        if kind == "archives":
            failures.extend(check_archives_entry(name, tree, entry))
            failures.extend(check_unsafe_justifications(name))
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


def download_bounded(url):
    req = urllib.request.Request(url, headers={"User-Agent": "FerrionOS-components-check/1"})
    chunks = []
    total = 0
    with urllib.request.urlopen(req, timeout=ARCHIVE_TIMEOUT_S) as resp:
        while True:
            chunk = resp.read(ARCHIVE_CHUNK_BYTES)
            if not chunk:
                break
            total += len(chunk)
            if total > MAX_ARCHIVE_BYTES:
                raise ValueError(f"download exceeded {MAX_ARCHIVE_BYTES} byte limit")
            chunks.append(chunk)
    return b"".join(chunks)


def unpack_archive_dir(data, dest):
    dest_resolved = dest.resolve()
    roots = set()
    unpacked_bytes = 0
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:*") as tf:
        members = tf.getmembers()
        if len(members) > MAX_ARCHIVE_MEMBERS:
            raise ValueError(f"archive has {len(members)} members, exceeding limit {MAX_ARCHIVE_MEMBERS}")
        for member in members:
            rel = Path(member.name)
            if rel.is_absolute() or ".." in rel.parts or not rel.parts:
                raise ValueError(f"unsafe archive member path {member.name!r}")
            if member.issym() or member.islnk():
                raise ValueError(f"refused symlink or hard link in archive: {member.name!r}")
            roots.add(rel.parts[0])
            target = (dest / rel).resolve()
            if not target.is_relative_to(dest_resolved):
                raise ValueError(f"archive member escapes scratch dir: {member.name!r}")
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            elif member.isfile():
                target.parent.mkdir(parents=True, exist_ok=True)
                extracted = tf.extractfile(member)
                if extracted is None:
                    raise ValueError(f"could not read archive file {member.name!r}")
                content = extracted.read()
                unpacked_bytes += len(content)
                if unpacked_bytes > MAX_ARCHIVE_UNPACKED_BYTES:
                    raise ValueError(
                        f"unpacked archive exceeded {MAX_ARCHIVE_UNPACKED_BYTES} byte limit"
                    )
                target.write_bytes(content)
            else:
                raise ValueError(f"unsupported archive member type for {member.name!r}")
    if len(roots) != 1:
        raise ValueError(f"expected one top-level archive directory, found {sorted(roots)}")
    return dest / next(iter(roots))


def tree_files(root_dir):
    found = {}
    for dirpath, _dirnames, filenames in os.walk(root_dir):
        for fname in filenames:
            full = Path(dirpath) / fname
            found[full.relative_to(root_dir).as_posix()] = full
    return found


def compare_trees(label, expected_dir, actual_dir):
    failures = []
    expected = tree_files(expected_dir)
    actual = tree_files(actual_dir)
    missing = sorted(set(expected) - set(actual))
    extra = sorted(set(actual) - set(expected))
    if missing:
        failures.append(f"{label}: missing {len(missing)} file(s) from archive (first: {missing[0]})")
    if extra:
        failures.append(f"{label}: {len(extra)} extra file(s) not in archive (first: {extra[0]})")
    for rel in sorted(set(expected) & set(actual)):
        if expected[rel].read_bytes() != actual[rel].read_bytes():
            failures.append(f"{label}: file differs from archive: {rel}")
    return failures


def find_import_commit(expected_sha):
    out = subprocess.run(
        ["git", "log", "-n1", "--format=%H", f"-S{expected_sha}", "--", str(RECORD.relative_to(ROOT))],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    rev = out.stdout.strip()
    return rev or None


def verify_archives(target_name=None):
    failures = []
    record = tomllib.loads(RECORD.read_text())
    if target_name is not None:
        if target_name not in record:
            return [f"{target_name} is not recorded in {RECORD.relative_to(ROOT)}"]
        if record[target_name].get("kind", "component") != "archives":
            return [f"{target_name}: --verify-archives requires kind = \"archives\""]
        names = [target_name]
    else:
        names = sorted(name for name, entry in record.items() if entry.get("kind", "component") == "archives")
        if not names:
            return []
    with tempfile.TemporaryDirectory(prefix="components-verify-archives-") as tmp:
        tmp_path = Path(tmp)
        for name in names:
            entry = record[name]
            archives = entry.get("archives", {})
            if not isinstance(archives, dict) or not archives:
                failures.append(f"{name}: no archives recorded")
                continue
            for archive, archive_entry in sorted(archives.items()):
                rel = str(archive_entry.get("path", "")).strip()
                url = str(archive_entry.get("url", "")).strip()
                expected_sha = str(archive_entry.get("sha256", "")).strip()
                label = f"{name} archive {archive}"
                if not url.startswith("https://") or not HEX64.fullmatch(expected_sha) or not rel:
                    failures.append(f"{label}: requires path, https url, and 64-hex sha256")
                    continue
                try:
                    data = download_bounded(url)
                except (urllib.error.URLError, OSError, ValueError) as err:
                    failures.append(f"{label}: download failed: {err}")
                    continue
                actual_sha = hashlib.sha256(data).hexdigest()
                if actual_sha != expected_sha:
                    failures.append(f"{label}: sha256 mismatch: expected {expected_sha}, got {actual_sha}")
                    continue
                rev = find_import_commit(expected_sha)
                if rev is None:
                    failures.append(f"{label}: no commit in {RECORD.relative_to(ROOT)} history introduces sha256 {expected_sha}")
                    continue
                unpack_root = tmp_path / "unpacked" / name / archive
                unpack_root.mkdir(parents=True, exist_ok=True)
                try:
                    expected_dir = unpack_archive_dir(data, unpack_root)
                except (tarfile.TarError, OSError, ValueError) as err:
                    failures.append(f"{label}: unpack failed: {err}")
                    continue
                git_out = subprocess.run(
                    ["git", "archive", "--format=tar", rev, f"components/{name}/{rel}"],
                    cwd=ROOT,
                    capture_output=True,
                )
                if git_out.returncode != 0:
                    failures.append(f"{label}: git archive at {rev} failed: {git_out.stderr.decode(errors='replace').strip()}")
                    continue
                actual_root = tmp_path / "git" / name / archive
                actual_root.mkdir(parents=True, exist_ok=True)
                with tarfile.open(fileobj=io.BytesIO(git_out.stdout), mode="r:") as tf:
                    tf.extractall(actual_root, filter="data")
                actual_dir = actual_root / "components" / name / rel
                failures.extend(compare_trees(f"{label} (import {rev[:9]})", expected_dir, actual_dir))
    return failures


def main(argv):
    if argv[:1] == ["--rebaseline"] and len(argv) == 2:
        if argv[1] not in tomllib.loads(RECORD.read_text()):
            print(f"{argv[1]} is not recorded in {RECORD.relative_to(ROOT)}", file=sys.stderr)
            return 2
        rebaseline(argv[1])
        return 0
    if argv[:1] == ["--verify-archives"] and len(argv) in (1, 2):
        target = argv[1] if len(argv) == 2 else None
        failures = verify_archives(target)
        for failure in failures:
            print(f"FAIL {failure}")
        if not failures:
            label = target if target is not None else "all kind = \"archives\" entries"
            print(f"{label}: every archive sha256 and unpacked import tree matches.")
        return 1 if failures else 0
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
