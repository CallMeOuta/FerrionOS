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
import re
import stat
import struct
import subprocess
import sys
import tomllib
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "scripts" / "security-audit.baseline.jsonl"
CHECKS = ("accounts", "sources", "downloads")
ALL_CHECKS = ("accounts", "sources", "downloads", "files")
CLOSURE_CONFIGS = (
    "config/minimal.toml",
    "config/desktop-minimal.toml",
    "config/server.toml",
    "config/x86_64/desktop.toml",
    "config/x86_64/dev-parity.toml",
)
MAX_CLOSURE_RECIPES = 10_000
GIT_REV_RE = re.compile(r"^[0-9a-fA-F]{40}$")
BLAKE3_RE = re.compile(r"^[0-9a-fA-F]{64}$")
MAX_IMAGE_ENTRIES = 200_000
MAX_ELF_PHDRS = 256
MAX_DYN_ENTRIES = 4096
MAX_STATIC_ELF_SCAN_BYTES = 64 * 1024 * 1024
RELIBC_START_MARKER = b"no thread fd present"
STACK_CHK_GUARD_INIT_64 = struct.pack("<Q", 0xD048C37519FCADFE)
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

Usage: {sys.argv[0]} [--json] [--check <name>]... [--closure <config|all>]... [--repo-bin <path>] [--root <dir>]
  --json               one JSON object per line: check, subject, status, note, fix
  --check <name>       run only this check ({", ".join(ALL_CHECKS)}); repeatable
  --closure <cfg|all>  check every source cooked for this config (or 'all' for the 5 release configs),
                       including wip, with zero baseline; repeatable; requires target/release/repo
  --repo-bin <path>    path to cookbook repo binary (default: target/release/repo)
  --root <dir>         mounted image root for the `files` check
Exit code 1 for any unpinned or unreadable closure source, otherwise when a `fail` line is absent from {BASELINE.relative_to(ROOT)}."""


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

def check_sources(closures=(), repo_bin=None):
    out = []
    recipes = ROOT / "recipes"
    use_closure = bool(closures)
    if use_closure:
        binary = Path(repo_bin) if repo_bin else (ROOT / "target/release/repo")
        paths = set()
        for cfg in closures:
            try:
                result = subprocess.run(
                    [str(binary), "cook-list",
                     f"--filesystem={cfg}", "--with-package-deps", "--display=path"],
                    cwd=ROOT, text=True, capture_output=True, check=True)
                cfg_paths = {ROOT / p / "recipe.toml" for p in result.stdout.splitlines() if p.strip()}
                if not cfg_paths:
                    raise ValueError("cook-list returned no recipes")
                paths.update(cfg_paths)
            except (OSError, ValueError, subprocess.CalledProcessError) as err:
                detail = err.stderr.strip() if isinstance(err, subprocess.CalledProcessError) else str(err)
                out.append(line("sources", str(cfg), "fail", f"cannot resolve closure: {detail}",
                                "build the cookbook and check the filesystem config"))
    else:
        paths = {p for p in recipes.rglob("recipe.toml")
                 if p.relative_to(recipes).parts[0] != "wip"}
    seen = set()
    while paths:
        if len(seen) >= MAX_CLOSURE_RECIPES:
            out.append(line("sources", "recipes", "fail",
                            f"recipe count exceeded bound {MAX_CLOSURE_RECIPES}"))
            break
        path = min(paths)
        paths.remove(path)
        path = path.resolve()
        if path in seen:
            continue
        seen.add(path)
        try:
            path.relative_to(recipes)
            subject = rel(path.parent)
            source = tomllib.loads(path.read_text()).get("source", {})
        except (OSError, ValueError) as err:
            try:
                err_subject = rel(path)
            except ValueError:
                err_subject = path.name
            out.append(line("sources", err_subject, "fail" if use_closure else "unknown",
                            f"cannot parse recipe.toml: {err}"))
            continue
        if "git" in source:
            rev = source.get("rev", "")
            if isinstance(rev, str) and GIT_REV_RE.match(rev):
                out.append(line("sources", subject, "pass", "git source pinned by rev"))
            elif isinstance(rev, str) and rev:
                out.append(line("sources", subject, "fail",
                                f"git source rev is not a full 40-hex commit: {rev}",
                                f'set rev = "<40-hex commit>" in [source] in {rel(path)}'))
            else:
                out.append(line("sources", subject, "fail", "git source without rev: it follows a branch",
                                f'add rev = "<40-hex commit>" to [source] in {rel(path)}'))
        elif "tar" in source:
            digest = source.get("blake3", "")
            if isinstance(digest, str) and BLAKE3_RE.match(digest):
                out.append(line("sources", subject, "pass", "tar source pinned by blake3"))
            elif isinstance(digest, str) and digest:
                out.append(line("sources", subject, "fail",
                                f"tar source blake3 is not a 64-hex hash: {digest}",
                                f'set blake3 = "<64-hex hash>" in [source] in {rel(path)}'))
            else:
                out.append(line("sources", subject, "fail", "tar source without blake3: any content is accepted",
                                f'add blake3 = "<64-hex hash>" to [source] in {rel(path)}'))
        elif "path" in source:
            target = (path.parent / source["path"]).resolve()
            if target.is_dir() and target.is_relative_to(ROOT):
                out.append(line("sources", subject, "pass", "path source inside repository"))
            else:
                out.append(line("sources", subject, "fail",
                                f"path source does not resolve to a directory inside this repository: {source['path']}",
                                f"point [source] path in {rel(path)} at a directory under this repository"))
        elif use_closure and "same_as" in source:
            paths.add(path.parent / source["same_as"] / "recipe.toml")
            out.append(line("sources", subject, "info", f"shared source {source['same_as']}"))
        elif use_closure:
            out.append(line("sources", subject, "info", "local source; no remote download"))
    return sorted(out, key=lambda row: row["subject"])


# downloads ------------------------------------------------------------------

# (subject, file, text that marks the unpinned download, note, fix). When the text is
# gone from the file the download counts as pinned; delete the entry once it passes.
DOWNLOADS = [
    ("prebuilt package key", "mk/prefix.mk",
     'wget -O $@.partial "https://static.redox-os.org/pkg/id_ed25519.pub.toml"',
     "the key that verifies prebuilt packages is downloaded with no pinned hash",
     "check the key in to the repository or pin its sha256 with ch_download"),
    ("upstream Rust archives", "mk/prefix.mk",
     'wget -O $@.partial "https://static.rust-lang.org/dist/',
     "optional upstream compiler archives lack checksums",
     "pin every archive before enabling PREFIX_USE_UPSTREAM_RUST_COMPILER"),
    ("virtiofsd", "mk/virtiofs.mk",
     "VIRTIOFSD_URL?=https://gitlab.com/virtio-fs/virtiofsd/",
     "optional host virtiofsd download follows a branch without a checksum",
     "use a distribution's virtiofsd or pin the downloaded binary"),
    ("U-Boot firmware", "mk/qemu.mk",
     "wget -O $@ https://gitlab.redox-os.org/Ivan/redox_firmware/",
     "optional ARM U-Boot downloads follow a branch without checksums",
     "pin each firmware download by sha256"),
]

DOWNLOAD_PINS = [
    ("build container base", "podman/redox-base-containerfile",
     [r"^FROM docker\.io/library/debian:trixie@sha256:[0-9a-f]{64}$"]),
    ("container apt snapshot", "podman/redox-base-containerfile",
     [r"^ARG APT_SNAPSHOT=\d{8}T\d{6}Z$",
      r"archive/debian/\$\{APT_SNAPSHOT\}/",
      r"archive/debian-security/\$\{APT_SNAPSHOT\}/",
      r"check-valid-until=no",
      r"rm /etc/apt/sources\.list\.d/debian\.sources"]),
    ("container layer cache", "mk/config.mk",
     [r"^PODMAN_CACHE_PULL\?=0$"]),
    ("prebuilt toolchain", "mk/prefix.mk",
     [rf"^PREFIX_SHA256_{name}-install\.tar\.gz=[0-9a-f]{{64}}$"
      for name in ("clang", "gcc", "rust")] +
     [r"sha256sum --check --quiet", r"\$\(call prefix_verify,\$@,\$@\.partial,1\)",
      r"\$\(call prefix_verify,\$<,\$<\)"]),
    ("rustup install", "podman/rustinstall.sh",
     [r"^RUSTUP_VERSION=\d+\.\d+\.\d+$", r"^RUST_TOOLCHAIN=\d+\.\d+\.\d+$",
      r"RUSTUP_SHA256=[0-9a-f]{64}", r"rustup/archive/",
      r'sha256sum --check', r'--default-toolchain "\$RUST_TOOLCHAIN"']),
    ("sccache", "podman/rustinstall.sh",
     [r"^SCCACHE_VERSION=\d+\.\d+\.\d+$", r"SCCACHE_SHA256=[0-9a-f]{64}",
      r"mozilla/sccache/", r"sha256sum --check"]),
    ("just", "podman/rustinstall.sh",
     [r"^JUST_VERSION=\d+\.\d+\.\d+$", r"JUST_SHA256=[0-9a-f]{64}",
      r"casey/just/", r"sha256sum --check"]),
    ("cbindgen", "podman/rustinstall.sh",
     [r"^CBINDGEN_VERSION=\d+\.\d+\.\d+$", r"CBINDGEN_SHA256=[0-9a-f]{64}",
      r"mozilla/cbindgen/", r"sha256sum --check"]),
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
    for subject, file, patterns in DOWNLOAD_PINS:
        try:
            text = (ROOT / file).read_text()
            pinned = all(re.search(pattern, text, re.MULTILINE) for pattern in patterns)
        except OSError as err:
            out.append(line("downloads", subject, "fail", f"cannot read {file}: {err}"))
            continue
        out.append(line("downloads", subject, "pass" if pinned else "fail",
                        f"pinned input and verification present in {file}" if pinned else
                        f"missing pin or verification in {file}",
                        "" if pinned else f"restore the pinned input and verification in {file}"))
    # pkgutils lives outside this repository, so this entry cannot be read from the tree.
    out.append(line("downloads", "pkgutils repository key", "fail",
                    "pkgutils fetches a repository's key from that repository when it has none "
                    "(pkg-lib/src/repo_manager.rs, outside this repository)",
                    "ship the repository key in the image and refuse a repository without one"))
    return out


def _non_relro_slices(seg_start, seg_end, relro_ranges):
    slices = [(seg_start, seg_end)]
    for r_start, r_end in relro_ranges:
        next_slices = []
        for s_start, s_end in slices:
            if r_end <= s_start or r_start >= s_end:
                next_slices.append((s_start, s_end))
                continue
            if s_start < r_start:
                next_slices.append((s_start, r_start))
            if r_end < s_end:
                next_slices.append((r_end, s_end))
        slices = next_slices
    return slices


def parse_elf(path):
    with open(path, "rb") as fh:
        ident = fh.read(16)
        if len(ident) < 16 or ident[:4] != b"\x7fELF":
            return None
        ei_class = ident[4]
        ei_data = ident[5]
        if ei_class != 2 or ei_data != 1:
            return {
                "unsupported": (
                    f"unsupported ELF class={ei_class} data={ei_data} "
                    "(expected ELF64 little-endian)"
                )
            }
        hdr = fh.read(48)
        if len(hdr) < 48:
            return {"error": "truncated ELF64 header"}
        e_type, _e_mach, _e_ver, _e_entry, e_phoff, _e_shoff, _e_flags, _e_ehsize, e_phentsize, e_phnum = (
            struct.unpack("<HHIQQQIHHH", hdr[:42])
        )
        min_phent = 56
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
        relro_ranges = []
        load_segments = []
        for i in range(e_phnum):
            fh.seek(e_phoff + i * e_phentsize)
            raw = fh.read(min_phent)
            if len(raw) < min_phent:
                return {"error": f"truncated program header {i}"}
            p_type, p_flags, p_offset, _p_vaddr, _p_paddr, p_filesz, p_memsz, _p_align = (
                struct.unpack("<IIQQQQQQ", raw)
            )
            if p_type == PT_GNU_RELRO and p_memsz > 0:
                has_relro = True
                if p_filesz > 0:
                    relro_ranges.append((p_offset, p_offset + p_filesz))
            elif p_type == PT_DYNAMIC:
                has_dynamic = True
                dyn_offset = p_offset
                dyn_filesz = p_filesz
            elif p_type == PT_INTERP:
                has_interp = True
            elif p_type == PT_GNU_STACK:
                gnu_stack = "RWE" if (p_flags & PF_X) else "RW"
            elif p_type == PT_LOAD:
                if (p_flags & PF_W) and (p_flags & PF_X):
                    wx_load += 1
                if p_filesz > 0:
                    load_segments.append((p_flags, p_offset, p_filesz))
        bind_now = False
        if has_dynamic and dyn_filesz > 0:
            ent_size = 16
            count = min(dyn_filesz // ent_size, MAX_DYN_ENTRIES)
            fh.seek(dyn_offset)
            dyn_bytes = fh.read(count * ent_size)
            actual = len(dyn_bytes) // ent_size
            for i in range(actual):
                d_tag, d_val = struct.unpack_from("<qQ", dyn_bytes, i * ent_size)
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
        stack_guard = "dynamic" if has_dynamic else "n/a"
        if not has_dynamic and e_type != 1:
            scanned = 0
            is_relibc = False
            has_rw_guard = False
            for p_flags, p_offset, p_filesz in load_segments:
                if p_filesz > MAX_STATIC_ELF_SCAN_BYTES - scanned:
                    return {
                        "error": (
                            f"static ELF PT_LOAD scan exceeds bound "
                            f"{MAX_STATIC_ELF_SCAN_BYTES}"
                        )
                    }
                fh.seek(p_offset)
                seg_bytes = fh.read(p_filesz)
                scanned += len(seg_bytes)
                if not is_relibc and RELIBC_START_MARKER in seg_bytes:
                    is_relibc = True
                if (p_flags & PF_W) and not has_rw_guard:
                    seg_end = p_offset + len(seg_bytes)
                    for sl_start, sl_end in _non_relro_slices(
                        p_offset, seg_end, relro_ranges
                    ):
                        if STACK_CHK_GUARD_INIT_64 in seg_bytes[sl_start - p_offset : sl_end - p_offset]:
                            has_rw_guard = True
                            break
            if is_relibc:
                stack_guard = "yes" if has_rw_guard else "no"
        return {
            "type": type_name,
            "has_relro": has_relro,
            "has_dynamic": has_dynamic,
            "bind_now": bind_now,
            "gnu_stack": gnu_stack,
            "wx_load": wx_load,
            "stack_guard": stack_guard,
        }


def check_files(image_root):
    root = Path(image_root).resolve()
    if not root.is_dir():
        return [line("files", str(image_root), "unknown", "image root is not a directory")]
    out = []
    seen = 0
    static_relibc_count = 0
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
            if "unsupported" in info:
                out.append(line(
                    "files",
                    subject,
                    "fail",
                    info["unsupported"],
                    "build 64-bit little-endian ELF binaries only",
                ))
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
                f"gnu_stack={info['gnu_stack']} wx_load={info['wx_load']} "
                f"stack_guard={info['stack_guard']}"
            )
            problems = []
            fixes = []
            if not info["has_relro"]:
                problems.append("no PT_GNU_RELRO")
            if info["has_dynamic"] and not info["bind_now"]:
                problems.append("no bind-now")
            if info["gnu_stack"] == "RWE":
                problems.append("executable PT_GNU_STACK")
            if info["wx_load"] > 0:
                problems.append(f"{info['wx_load']} W+X PT_LOAD")
            if problems:
                fixes.append("link with -Wl,-z,relro -Wl,-z,now -Wl,-z,noexecstack and no W+X segments")
            if info["stack_guard"] in ("yes", "no"):
                static_relibc_count += 1
            if info["stack_guard"] == "no":
                problems.append("no random stack guard")
                fixes.append("relink statically linked program against current relibc")
            if problems:
                out.append(line(
                    "files",
                    subject,
                    "fail",
                    f"{note} ({', '.join(problems)})",
                    "; ".join(fixes),
                ))
            else:
                out.append(line("files", subject, "pass", note))
    has_base_image = (root / "usr/bin").is_dir() and (
        (root / "usr/lib/init.d").is_dir() or (root / "usr/bin/getty").is_file()
    )
    if has_base_image and static_relibc_count == 0:
        out.append(line(
            "files",
            "/usr/bin: stack_guard",
            "fail",
            "0 static relibc ELFs recognized in base image",
            "verify RELIBC_START_MARKER in scripts/security-audit.py matches relibc startup",
        ))
    return out


# main -----------------------------------------------------------------------

def main(argv):
    as_json, only, closures, repo_bin, image_root = False, [], [], None, None
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
        elif arg == "--closure":
            cfg = next(args, None)
            if not cfg:
                print(USAGE, file=sys.stderr)
                return 2
            if cfg == "all":
                closures.extend(CLOSURE_CONFIGS)
            else:
                closures.append(cfg)
        elif arg == "--repo-bin":
            repo_bin = next(args, None)
            if not repo_bin:
                print(USAGE, file=sys.stderr)
                return 2
        elif arg == "--root":
            image_root = next(args, None)
            if not image_root:
                print(USAGE, file=sys.stderr)
                return 2
        else:
            print(USAGE, file=sys.stderr)
            return 0 if arg in ("-h", "--help") else 2
    if closures and not only:
        only = ["sources"]
    if "files" in only and not image_root:
        print(USAGE, file=sys.stderr)
        return 2
    active = list(only) if only else list(CHECKS) + (["files"] if image_root else [])
    run = {
        "accounts": check_accounts,
        "sources": lambda: check_sources(closures, repo_bin),
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
    new = [r for r in results if r["status"] == "fail"
           and ((closures and r["check"] == "sources") or (r["check"], r["subject"]) not in known)]

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
        if "files" in active:
            exempt = [
                r["subject"]
                for r in results
                if r["check"] == "files" and "stack_guard=n/a" in r["note"]
            ]
            if exempt:
                print(f"files stack_guard=n/a ({len(exempt)}):")
                for subj in exempt:
                    print(f"  {subj}")
        for r in new:
            label = "FAIL" if (closures and r["check"] == "sources") else "NEW FAIL"
            print(f"{label} {r['check']}: {r['subject']}: {r['note']}\n    fix: {r['fix']}")
        if not new:
            print("Closure sources pinned. Details: --json" if closures else
                  "No fail outside the baseline. Details: --json")
    return 1 if new else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
