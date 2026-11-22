#!/usr/bin/env python3
# Audits the tree for security findings without building anything:
#   accounts   users with an empty or written password, and sudo members, per filesystem config
#   sources    recipes outside recipes/wip: git without rev, tar without blake3
#   downloads  build inputs fetched with no pinned hash (a fixed list that shrinks as each is pinned)
# Prints one JSON object per line with --json, a summary otherwise. Exits 1 when a
# `fail` line is missing from scripts/security-audit.baseline.jsonl. Run from the repository root.
# Python standard library only.

import json
import os
import stat
import struct
import sys
import tomllib
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "scripts" / "security-audit.baseline.jsonl"
CHECKS = ("accounts", "sources", "downloads")
ALL_CHECKS = ("accounts", "sources", "downloads", "files")
MAX_IMAGE_ENTRIES = 200_000
MAX_ELF_PHDRS = 256
MAX_DYN_ENTRIES = 4096
PT_LOAD = 1
PT_DYNAMIC = 2
PT_INTERP = 3
PT_GNU_STACK = 0x6474E551
PT_GNU_RELRO = 0x6474E552
PF_X = 0x1
PF_W = 0x2
DT_NULL = 0
DT_BIND_NOW = 24
DT_FLAGS = 30
DT_FLAGS_1 = 0x6FFFFFFB
DF_BIND_NOW = 0x8
DF_1_NOW = 0x1
ELF_TYPES = {0: "ET_NONE", 1: "ET_REL", 2: "ET_EXEC", 3: "ET_DYN", 4: "ET_CORE"}
USAGE = f"""Audits FerrionOS's configs, recipes, build files and mounted images for security findings.

Usage: {sys.argv[0]} [--json] [--check <name>]... [--root <dir>]
  --json          one JSON object per line: check, subject, status, note, fix
  --check <name>  run only this check ({", ".join(ALL_CHECKS)}); repeatable
  --root <dir>    mounted image root for the `files` check
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


def parse_elf(path):
    with open(path, "rb") as fh:
        ident = fh.read(16)
        if len(ident) < 16 or ident[:4] != b"\x7fELF":
            return None
        ei_class = ident[4]
        ei_data = ident[5]
        if ei_class not in (1, 2) or ei_data not in (1, 2):
            return {"error": f"unsupported ELF class={ei_class} data={ei_data}"}
        endian = "<" if ei_data == 1 else ">"
        is_64 = ei_class == 2
        if is_64:
            hdr = fh.read(48)
            if len(hdr) < 48:
                return {"error": "truncated ELF64 header"}
            e_type, _e_mach, _e_ver, _e_entry, e_phoff, _e_shoff, _e_flags, _e_ehsize, e_phentsize, e_phnum = (
                struct.unpack(endian + "HHIQQQIHHH", hdr[:42])
            )
            min_phent = 56
        else:
            hdr = fh.read(36)
            if len(hdr) < 36:
                return {"error": "truncated ELF32 header"}
            e_type, _e_mach, _e_ver, _e_entry, e_phoff, _e_shoff, _e_flags, _e_ehsize, e_phentsize, e_phnum = (
                struct.unpack(endian + "HHIIIIIHHH", hdr[:30])
            )
            min_phent = 32
        if e_phnum > MAX_ELF_PHDRS:
            return {"error": f"e_phnum {e_phnum} exceeds bound {MAX_ELF_PHDRS}"}
        if e_phnum > 0 and e_phentsize < min_phent:
            return {"error": f"e_phentsize {e_phentsize} smaller than {min_phent}"}
        has_relro = False
        has_dynamic = False
        has_interp = False
        dyn_offset = 0
        dyn_filesz = 0
        gnu_stack = "none"
        wx_load = 0
        for i in range(e_phnum):
            fh.seek(e_phoff + i * e_phentsize)
            raw = fh.read(min_phent)
            if len(raw) < min_phent:
                return {"error": f"truncated program header {i}"}
            if is_64:
                p_type, p_flags, p_offset, _p_vaddr, _p_paddr, p_filesz, p_memsz, _p_align = (
                    struct.unpack(endian + "IIQQQQQQ", raw)
                )
            else:
                p_type, p_offset, _p_vaddr, _p_paddr, p_filesz, p_memsz, p_flags, _p_align = (
                    struct.unpack(endian + "IIIIIIII", raw)
                )
            if p_type == PT_GNU_RELRO and p_memsz > 0:
                has_relro = True
            elif p_type == PT_DYNAMIC:
                has_dynamic = True
                dyn_offset = p_offset
                dyn_filesz = p_filesz
            elif p_type == PT_INTERP:
                has_interp = True
            elif p_type == PT_GNU_STACK:
                gnu_stack = "RWE" if (p_flags & PF_X) else "RW"
            elif p_type == PT_LOAD and (p_flags & PF_W) and (p_flags & PF_X):
                wx_load += 1
        bind_now = False
        if has_dynamic and dyn_filesz > 0:
            ent_size = 16 if is_64 else 8
            count = min(dyn_filesz // ent_size, MAX_DYN_ENTRIES)
            fh.seek(dyn_offset)
            dyn_bytes = fh.read(count * ent_size)
            actual = len(dyn_bytes) // ent_size
            fmt = endian + ("qQ" if is_64 else "iI")
            for i in range(actual):
                d_tag, d_val = struct.unpack_from(fmt, dyn_bytes, i * ent_size)
                if d_tag == DT_NULL:
                    break
                if d_tag == DT_BIND_NOW:
                    bind_now = True
                elif d_tag == DT_FLAGS and (d_val & DF_BIND_NOW):
                    bind_now = True
                elif d_tag == DT_FLAGS_1 and (d_val & DF_1_NOW):
                    bind_now = True
        type_name = ELF_TYPES.get(e_type, f"ET_{e_type}")
        if e_type == 3:
            type_name = "ET_DYN(PIE)" if has_interp else "ET_DYN(DSO)"
        return {
            "type": type_name,
            "has_relro": has_relro,
            "has_dynamic": has_dynamic,
            "bind_now": bind_now,
            "gnu_stack": gnu_stack,
            "wx_load": wx_load,
        }


def check_files(image_root):
    root = Path(image_root).resolve()
    if not root.is_dir():
        return [line("files", str(image_root), "unknown", "image root is not a directory")]
    out = []
    seen = 0
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames.sort()
        filenames.sort()
        entries = [(d, True) for d in dirnames] + [(f, False) for f in filenames]
        for name, is_dir in entries:
            seen += 1
            if seen > MAX_IMAGE_ENTRIES:
                out.append(line("files", str(root), "unknown",
                                f"entry count exceeded bound {MAX_IMAGE_ENTRIES}"))
                return out
            full = Path(dirpath) / name
            try:
                st = os.lstat(full)
            except OSError as err:
                out.append(line("files", f"/{full.relative_to(root).as_posix()}", "unknown",
                                f"cannot stat: {err}"))
                continue
            if stat.S_ISLNK(st.st_mode):
                continue
            subject = "/" + full.relative_to(root).as_posix()
            mode = stat.S_IMODE(st.st_mode)
            if mode & stat.S_ISUID:
                out.append(line("files", f"{subject}: mode", "fail",
                                f"setuid bit set (mode 0o{mode:04o})",
                                f"remove setuid bit from {subject}"))
            if mode & stat.S_ISGID:
                out.append(line("files", f"{subject}: mode", "fail",
                                f"setgid bit set (mode 0o{mode:04o})",
                                f"remove setgid bit from {subject}"))
            if mode & 0o0002:
                if is_dir and (mode & stat.S_ISVTX):
                    out.append(line("files", f"{subject}: mode", "info",
                                    f"world-writable sticky directory (mode 0o{mode:04o})"))
                else:
                    out.append(line("files", f"{subject}: mode", "fail",
                                    f"world-writable without sticky bit (mode 0o{mode:04o})",
                                    f"tighten permissions on {subject}"))
            if not stat.S_ISREG(st.st_mode):
                continue
            try:
                info = parse_elf(full)
            except OSError as err:
                out.append(line("files", subject, "unknown", f"cannot read file: {err}"))
                continue
            if info is None:
                continue
            if "error" in info:
                out.append(line("files", subject, "unknown", f"malformed ELF: {info['error']}"))
                continue
            if info["type"] == "ET_REL":
                out.append(line("files", subject, "info", "type=ET_REL (relocatable object)"))
                continue
            bind_str = ("yes" if info["bind_now"] else "no") if info["has_dynamic"] else "n/a"
            relro_str = "yes" if info["has_relro"] else "no"
            note = (
                f"type={info['type']} relro={relro_str} bind_now={bind_str} "
                f"gnu_stack={info['gnu_stack']} wx_load={info['wx_load']}"
            )
            problems = []
            if not info["has_relro"]:
                problems.append("no PT_GNU_RELRO")
            if info["has_dynamic"] and not info["bind_now"]:
                problems.append("no bind-now")
            if info["gnu_stack"] == "RWE":
                problems.append("executable PT_GNU_STACK")
            if info["wx_load"] > 0:
                problems.append(f"{info['wx_load']} W+X PT_LOAD")
            if problems:
                out.append(line(
                    "files",
                    subject,
                    "fail",
                    f"{note} ({', '.join(problems)})",
                    "link with -Wl,-z,relro -Wl,-z,now -Wl,-z,noexecstack and no W+X segments",
                ))
            else:
                out.append(line("files", subject, "pass", note))
    return out


# main -----------------------------------------------------------------------

def main(argv):
    as_json, only, image_root = False, [], None
    args = iter(argv)
    for arg in args:
        if arg == "--json":
            as_json = True
        elif arg == "--check":
            name = next(args, None)
            if name not in ALL_CHECKS:
                print(USAGE, file=sys.stderr)
                return 2
            only.append(name)
        elif arg == "--root":
            image_root = next(args, None)
            if not image_root:
                print(USAGE, file=sys.stderr)
                return 2
        else:
            print(USAGE, file=sys.stderr)
            return 0 if arg in ("-h", "--help") else 2
    if "files" in only and not image_root:
        print(USAGE, file=sys.stderr)
        return 2
    active = list(only) if only else list(CHECKS) + (["files"] if image_root else [])
    run = {
        "accounts": check_accounts,
        "sources": check_sources,
        "downloads": check_downloads,
        "files": lambda: check_files(image_root),
    }
    results = [r for name in ALL_CHECKS if name in active for r in run[name]()]

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
        for name in ALL_CHECKS:
            if name in active:
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
