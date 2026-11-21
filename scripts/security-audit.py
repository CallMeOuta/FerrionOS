#!/usr/bin/env python3
# Audits the tree for security findings without building anything:
#   accounts   users with an empty or written password, and sudo members, per filesystem config
#   sources    recipes outside recipes/wip: git without rev, tar without blake3
#   downloads  build inputs fetched with no pinned hash (a fixed list that shrinks as each is pinned)
# Prints one JSON object per line with --json, a summary otherwise. Exits 1 when a
# `fail` line is missing from scripts/security-audit.baseline.jsonl. Run from the repository root.
# Python standard library only.

import json
import sys
import tomllib
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "scripts" / "security-audit.baseline.jsonl"
CHECKS = ("accounts", "sources", "downloads")
USAGE = f"""Audits FerrionOS's configs, recipes and build files for security findings.

Usage: {sys.argv[0]} [--json] [--check <name>]...
  --json          one JSON object per line: check, subject, status, note, fix
  --check <name>  run only this check ({", ".join(CHECKS)}); repeatable
Exit code 1 when a `fail` line is absent from {BASELINE.relative_to(ROOT)}."""


def line(check, subject, status, note, fix=""):
    return {"check": check, "subject": subject, "status": status, "note": note, "fix": fix}


def rel(path):
    return path.resolve().relative_to(ROOT).as_posix()


# accounts -------------------------------------------------------------------

def load_config(path, stack=()):
    """Users and groups of one config with its includes merged, as redox_installer does."""
    path = path.resolve()
    if path in stack:
        raise ValueError(f"include cycle at {rel(path)}")
    data = tomllib.loads(path.read_text())
    users, groups = {}, {}
    for inc in data.get("include", []):
        inc_users, inc_groups = load_config(path.parent / inc, stack + (path,))
        users.update(inc_users)
        groups.update(inc_groups)
    for name, table in data.get("users", {}).items():
        users[name] = dict(table, _from=rel(path))
    for name, table in data.get("groups", {}).items():
        groups[name] = dict(table, _from=rel(path))
    return users, groups


def check_accounts():
    out = []
    for path in sorted((ROOT / "config").rglob("*.toml")):
        subject = rel(path)
        try:
            users, groups = load_config(path)
        except (OSError, ValueError, tomllib.TOMLDecodeError) as err:
            out.append(line("accounts", subject, "unknown", f"cannot resolve config: {err}"))
            continue
        found = []
        for name in sorted(users):
            user = users[name]
            src = user["_from"]
            if "password" not in user:
                continue
            if user["password"] == "":
                found.append(line("accounts", f"{subject}: user {name}", "fail",
                                  f"empty password, so login needs none (set in {src})",
                                  f"remove [users.{name}] from {src} or give it no login"))
            else:
                found.append(line("accounts", f"{subject}: user {name}", "fail",
                                  f"password written in the config (set in {src})",
                                  f"remove the password from [users.{name}] in {src}"))
        sudo = groups.get("sudo")
        for member in sorted(sudo.get("members", []) if sudo else []):
            found.append(line("accounts", f"{subject}: sudo member {member}", "warn",
                              f"member of sudo (set in {sudo['_from']})",
                              f"drop {member} from [groups.sudo] in {sudo['_from']} unless it is the owner's account"))
        out += found or [line("accounts", subject, "pass", "no passwordless, password-in-file or sudo accounts")]
    return out


# sources --------------------------------------------------------------------

def check_sources():
    out = []
    recipes = ROOT / "recipes"
    for path in sorted(recipes.rglob("recipe.toml")):
        if path.relative_to(recipes).parts[0] == "wip":
            continue
        subject = rel(path.parent)
        try:
            source = tomllib.loads(path.read_text()).get("source", {})
        except (OSError, tomllib.TOMLDecodeError) as err:
            out.append(line("sources", subject, "unknown", f"cannot parse recipe.toml: {err}"))
            continue
        if "git" in source:
            if "rev" in source:
                out.append(line("sources", subject, "pass", "git source pinned by rev"))
            else:
                out.append(line("sources", subject, "fail", "git source without rev: it follows a branch",
                                f"add rev = \"<commit>\" to [source] in {rel(path)}"))
        elif "tar" in source:
            if "blake3" in source:
                out.append(line("sources", subject, "pass", "tar source pinned by blake3"))
            else:
                out.append(line("sources", subject, "fail", "tar source without blake3: any content is accepted",
                                f"add blake3 = \"<hash>\" to [source] in {rel(path)}"))
    return out


# downloads ------------------------------------------------------------------

# (subject, file, text that marks the unpinned download, note, fix). When the text is
# gone from the file the download counts as pinned; delete the entry once it passes.
DOWNLOADS = [
    ("prebuilt toolchain", "mk/prefix.mk",
     'wget -O $@.partial "https://static.redox-os.org/toolchain/',
     "toolchain tarball downloaded with no pinned hash",
     "download it with ch_download (mk/ch.mk) and a pinned sha256"),
    ("prebuilt package key", "mk/prefix.mk",
     'wget -O $@.partial "https://static.redox-os.org/pkg/id_ed25519.pub.toml"',
     "the key that verifies prebuilt packages is downloaded with no pinned hash",
     "check the key in to the repository or pin its sha256 with ch_download"),
    ("build container base", "podman/redox-base-containerfile",
     "FROM docker.io/library/debian:trixie",
     "container starts from a tag, not a digest",
     "pin the base image as debian:trixie@sha256:<digest>"),
    ("rustup install", "podman/rustinstall.sh",
     'curl "https://sh.rustup.rs" -sSf | sh',
     "Rust is installed from a script piped to sh",
     "download rustup-init with a pinned sha256 and run it"),
]


def check_downloads():
    out = []
    for subject, file, marker, note, fix in DOWNLOADS:
        try:
            text = (ROOT / file).read_text()
        except OSError as err:
            out.append(line("downloads", subject, "unknown", f"cannot read {file}: {err}"))
            continue
        lines = [i for i, l in enumerate(text.splitlines(), 1) if marker in l]
        if lines:
            where = ", ".join(f"{file}:{i}" for i in lines)
            out.append(line("downloads", subject, "fail", f"{note} ({where})", fix))
        else:
            out.append(line("downloads", subject, "pass", f"no unpinned download left in {file}"))
    # pkgutils lives outside this repository, so this entry cannot be read from the tree.
    out.append(line("downloads", "pkgutils repository key", "fail",
                    "pkgutils fetches a repository's key from that repository when it has none "
                    "(pkg-lib/src/repo_manager.rs, outside this repository)",
                    "ship the repository key in the image and refuse a repository without one"))
    return out


# main -----------------------------------------------------------------------

def main(argv):
    as_json, only = False, []
    args = iter(argv)
    for arg in args:
        if arg == "--json":
            as_json = True
        elif arg == "--check":
            name = next(args, None)
            if name not in CHECKS:
                print(USAGE, file=sys.stderr)
                return 2
            only.append(name)
        else:
            print(USAGE, file=sys.stderr)
            return 0 if arg in ("-h", "--help") else 2
    run = {"accounts": check_accounts, "sources": check_sources, "downloads": check_downloads}
    results = [r for name in CHECKS if not only or name in only for r in run[name]()]

    known = set()
    if BASELINE.exists():
        for raw in BASELINE.read_text().splitlines():
            if raw.strip():
                b = json.loads(raw)
                if b["status"] == "fail":
                    known.add((b["check"], b["subject"]))
    new = [r for r in results if r["status"] == "fail" and (r["check"], r["subject"]) not in known]

    if as_json:
        for r in results:
            print(json.dumps(r, sort_keys=False))
    else:
        counts = Counter((r["check"], r["status"]) for r in results)
        for name in CHECKS:
            if not only or name in only:
                parts = [f"{s} {counts[(name, s)]}" for s in ("pass", "info", "warn", "fail", "unknown")
                         if counts[(name, s)]]
                print(f"{name}: {', '.join(parts) or 'nothing checked'}")
        for r in new:
            print(f"NEW FAIL {r['check']}: {r['subject']}: {r['note']}\n    fix: {r['fix']}")
        if not new:
            print("No fail outside the baseline. Details: --json")
    return 1 if new else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
