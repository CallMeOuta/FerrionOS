#!/usr/bin/env python3
# Lists every line of a tracked file (`git ls-files`) that contains the upstream project's
# name, case-insensitive, and classifies it by the exceptions in AGENTS.md "Naming":
#   legal        the LICENSE copyright line, license headers in code and patches, Cargo.toml authors
#   upstream     files applied byte-exact from upstream: *.patch and anything under a recipe's source/
#   identifier   every occurrence on the line is an identifier or a string other software matches
#   readme-fork  the one README.md sentence saying FerrionOS began as a fork
#   fail         anything else: text that should say "upstream" or "FerrionOS" instead
# Prints one JSON object per line with --json, a summary otherwise. Exits 1 when a `fail`
# line is missing from scripts/naming-check.baseline.jsonl. Run from the repository root.
# Python standard library only.

import json
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "scripts" / "naming-check.baseline.jsonl"
CLASSES = ("legal", "upstream", "identifier", "readme-fork", "fail")
USAGE = f"""Lists the lines of tracked files that name the upstream project, classified by AGENTS.md "Naming".

Usage: {sys.argv[0]} [--json]
  --json  one JSON object per line: path, line, class, text
Exit code 1 when a `fail` line is absent from {BASELINE.relative_to(ROOT)}."""

NAME = re.compile(r"redox", re.IGNORECASE)

# Rules ------------------------------------------------------------------------
# This checker and its baseline quote the name as data and are not scanned.
SKIP_PATHS = {"scripts/naming-check.py", "scripts/naming-check.baseline.jsonl"}

# The rules below are checked in this order; the first that matches gives the class.

# Files applied byte-exact from upstream (exception 3), by path.
UPSTREAM_PATHS = [
    r"\.patch$",
    r"(^|/)recipes/.*/source/",
]

# Legal notices (exception 1): (path regex, line regex).
LEGAL_LINES = [
    (r"^LICENSE$", r"(?i)copyright"),
    (r"", r"(?i)\b(copyright|spdx-license-identifier)\b|\(c\)\s*\d{4}"),
    (r"(^|/)Cargo\.toml$", r"^\s*authors\s*="),
]

# The fork sentence, the one other place the name appears: the README and the book's
# introduction carry it once each. (path regex, line regex).
README_FORK = [
    (r"^(README\.md|book/src/introduction\.md)$", r"(?i)\bfork\b.*\bnot affiliated\b|\bnot affiliated\b.*\bfork\b"),
]

# Identifiers and machine-matched strings (exception 2). Case-sensitive. Every occurrence of
# the name on a line must sit inside one of these matches for the line to count as `identifier`.
IDENTIFIERS = [
    # target triples, GNU triples and the variables cargo and bindgen derive from them
    r"[\w$(){}.-]*-unknown-redox\b",
    # make qemu's share=redox value, which shares the build tree under the tag "redox"
    r"\|redox>",
    r"\b\w+_unknown_redox\b",
    # URLs that code or a recipe fetches: GitLab, static files, packages, GitHub mirrors
    r"https?://[\w.-]*redox-os\.org[^\s\"'`)>\]]*",
    r"\b[\w.-]*redox-os\.org[^\s\"'`)>\]]*",
    r"https?://github\.com/redox-os/[^\s\"'`)>\]]*",
    r"\bredox-os/[\w.-]+",
    r"\bdocker\.io/redoxos/[\w$().-]*",
    # crate, package, recipe, file, branch and directory names
    r"\b[\w-]*redox[\w-]*\.(rs|toml|patch|sh|mk|py|md|img|iso|ion|ipxe|cmake|c|h|json|lock|zst)\b",
    r"[\w-]*redox-live\.iso",
    r"[\w.-]*Redox[\w.-]*\.patch\b",
    r"\b\w*redox_[\w$(@)]*",
    r"\b\w+_redox\b",
    r"\bredox-[a-z0-9][\w-]*",
    r"\b[a-z0-9][a-z0-9-]*-redox\b",
    r"\b(lib|is_os_)redox\w*\b",
    r"\bredox(fs|er|rt|os|0)[\w-]*",
    r"\b\w*_redoxer\w*",
    r"\bRedox(FS|er)\b",
    r"--[\w-]*redox[\w-]*",
    r"[\w./$()-]*/redox\b[\w./-]*",
    r"\bredox/[\w./-]*",
    # make and environment variables, C macros and Rust cfgs
    r"\b[A-Z0-9_]*REDOX[A-Z0-9_]*\b",
    r"__redox__",
    r"\btarget_os\s*=\s*\\?\"redox\\?\"",
    r"\bshare=redox\b",
    r"\bfilter redox redox:ro\b",
    r"\bredox(=|:ro\b)",
    # Rust type and variant names from upstream's crates, such as `RedoxDrmEvent`
    r"\bRedox[A-Z]\w*",
    # shell functions, make targets and directories named after the upstream project
    r"\bredox\(\)",
    r"^\s*redox \"\$",
    r"\bcd redox\b",
    r"-C src redox\b",
    r"\*-redox\*",
    # the uname -s value, values that build systems compare, os-release
    r"ID_LIKE=\"?redox[\w-]*\"?",
    r"[\"'][Rr]edox[\"']",
    r"\b[\w-]+=[Rr]edox\b(:\$\w+)?",
    r"\bredox\)",
    r",Redox\)",
    # a backtick-quoted value in prose names an identifier
    r"`[^`]*[Rr]edox[^`]*`",
]

# Matching ---------------------------------------------------------------------

UPSTREAM_RE = [re.compile(p) for p in UPSTREAM_PATHS]
LEGAL_RE = [(re.compile(p), re.compile(l)) for p, l in LEGAL_LINES]
README_RE = [(re.compile(p), re.compile(l)) for p, l in README_FORK]
IDENT_RE = [re.compile(p) for p in IDENTIFIERS]


def all_identifiers(text):
    covered = [False] * len(text)
    for rx in IDENT_RE:
        for m in rx.finditer(text):
            for i in range(m.start(), m.end()):
                covered[i] = True
    return all(all(covered[m.start():m.end()]) for m in NAME.finditer(text))


def classify(path, text):
    if any(rx.search(path) for rx in UPSTREAM_RE):
        return "upstream"
    if any(p.search(path) and l.search(text) for p, l in LEGAL_RE):
        return "legal"
    if any(p.search(path) and l.search(text) for p, l in README_RE):
        return "readme-fork"
    if all_identifiers(text):
        return "identifier"
    return "fail"


def scan():
    files = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, check=True,
                           capture_output=True).stdout.decode().split("\0")
    out = []
    for path in sorted(f for f in files if f and f not in SKIP_PATHS):
        try:
            data = (ROOT / path).read_bytes()
        except (IsADirectoryError, FileNotFoundError):
            continue
        if b"\0" in data or not NAME.search(data.decode("utf-8", "replace")):
            continue
        for num, text in enumerate(data.decode("utf-8", "replace").splitlines(), 1):
            if NAME.search(text):
                out.append({"path": path, "line": num, "class": classify(path, text),
                            "text": text.strip()})
    return out


def main(argv):
    as_json = False
    for arg in argv:
        if arg == "--json":
            as_json = True
        else:
            print(USAGE, file=sys.stderr)
            return 0 if arg in ("-h", "--help") else 2
    results = scan()

    known = set()
    if BASELINE.exists():
        for raw in BASELINE.read_text().splitlines():
            if raw.strip():
                b = json.loads(raw)
                if b["class"] == "fail":
                    known.add((b["path"], b["text"]))
    new = [r for r in results if r["class"] == "fail" and (r["path"], r["text"]) not in known]

    if as_json:
        for r in results:
            print(json.dumps(r, ensure_ascii=False))
    else:
        counts = Counter(r["class"] for r in results)
        print(", ".join(f"{c} {counts[c]}" for c in CLASSES))
        for r in new:
            print(f"NEW FAIL {r['path']}:{r['line']}: {r['text']}")
        if not new:
            print("No fail outside the baseline. Details: --json")
    return 1 if new else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
