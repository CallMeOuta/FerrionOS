#!/usr/bin/env python3
import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_REPO = ROOT / "recipes/wip/web/firefox/source"
DEFAULT_OUT = ROOT / "build/firefox-area-split"
DEFAULT_BASE = "2a775683d38643e7d7372faec42b865c8a3c4d68"
DEFAULT_HEAD = "9bf8f0abb65ac35fff3dfd5bf9aef395e4e5497e"

MAX_CHANGED_FILES = 1024
MAX_AREAS = 32
CMD_TIMEOUT_S = 120

AREAS = (
    (
        "01-build-target.patch",
        "Build system, target OS detection, system headers, xptcall and browser app",
        (
            "build/moz.configure/init.configure",
            "python/mozbuild/mozbuild/configure/constants.py",
            "config/system-headers.mozbuild",
            "xpcom/reflect/xptcall/md/unix/moz.build",
            "browser/app/nsBrowserApp.cpp",
        ),
    ),
    (
        "02-ipc-libevent.patch",
        "IPC Chromium base, libevent config and POSIX shared memory",
        (
            "ipc/chromium/",
            "ipc/glue/SharedMemoryPlatform_posix.cpp",
        ),
    ),
    (
        "03-threads-time.patch",
        "Threads, mutexes, timestamps and nsProcessCommon",
        (
            "js/src/threading/posix/PosixThread.cpp",
            "mozglue/misc/PlatformMutex.h",
            "mozglue/misc/TimeStamp_posix.cpp",
            "xpcom/threads/nsProcessCommon.cpp",
        ),
    ),
    (
        "04-memory-madvise.patch",
        "GC memory and Poison madvise handling",
        (
            "js/src/gc/Memory.cpp",
            "mfbt/Poison.cpp",
        ),
    ),
    (
        "05-binary-path.patch",
        "XPCOM BinaryPath fallback",
        (
            "xpcom/build/BinaryPath.h",
        ),
    ),
    (
        "06-sqlite-alloca-platform.patch",
        "SQLite compat, ANGLE, DRM and NSS karamel platform headers",
        (
            "third_party/sqlite3/src/moz.build",
            "gfx/angle/checkout/src/common/platform.h",
            "third_party/drm/",
            "security/nss/lib/freebl/verified/karamel/include/krml/internal/builtin.h",
        ),
    ),
    (
        "07-cargo-workspace-supply-chain.patch",
        "Cargo workspace, lockfile, supply-chain audit and stub crate removal",
        (
            ".cargo/config.toml.in",
            "Cargo.toml",
            "Cargo.lock",
            "netwerk/socket/neqo_glue/Cargo.toml",
            "netwerk/test/http3server/Cargo.toml",
            "supply-chain/",
            "build/rust/redox_syscall/",
            "build/rust/redox_users/",
        ),
    ),
    (
        "08-crate-redox-syscall.patch",
        "Vendored redox_syscall crate and cargo checksum",
        ("third_party/rust/redox_syscall/",),
    ),
    (
        "09-crate-redox-users.patch",
        "Vendored redox_users crate and cargo checksum",
        ("third_party/rust/redox_users/",),
    ),
    (
        "10-crate-libredox.patch",
        "Vendored libredox crate and cargo checksum",
        ("third_party/rust/libredox/",),
    ),
    (
        "11-crate-mtu.patch",
        "Vendored mtu crate patch and cargo checksum",
        ("third_party/rust/mtu/",),
    ),
    (
        "12-crate-neqo.patch",
        "Vendored neqo crates patch and cargo checksums",
        (
            "third_party/rust/neqo-bin/",
            "third_party/rust/neqo-crypto/",
            "third_party/rust/neqo-udp/",
        ),
    ),
    (
        "13-crate-quinn-udp.patch",
        "Vendored quinn-udp crate patch and cargo checksum",
        ("third_party/rust/quinn-udp/",),
    ),
)

OFFSET_OR_FUZZ_RE = re.compile(r"\b(offset|fuzz)\b", re.IGNORECASE)


def run_cmd(cmd, cwd=None, input_bytes=None):
    proc = subprocess.run(
        cmd,
        cwd=cwd,
        input=input_bytes,
        capture_output=True,
        timeout=CMD_TIMEOUT_S,
    )
    if proc.returncode != 0:
        err = proc.stderr.decode("utf-8", errors="replace").strip()
        out = proc.stdout.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"{cmd} exited {proc.returncode}: {err or out}")
    return proc.stdout


def matches_rule(path, rule):
    if rule.endswith("/"):
        return path.startswith(rule)
    return path == rule


def classify_files(changed_files):
    assert len(AREAS) <= MAX_AREAS
    assert len(changed_files) <= MAX_CHANGED_FILES
    by_area = {name: [] for name, _, _ in AREAS}
    unassigned = []
    duplicates = []
    for path in changed_files:
        hits = [name for name, _, rules in AREAS if any(matches_rule(path, r) for r in rules)]
        if not hits:
            unassigned.append(path)
        elif len(hits) > 1:
            duplicates.append((path, hits))
        else:
            by_area[hits[0]].append(path)
    if unassigned:
        raise RuntimeError(f"unassigned files ({len(unassigned)}): {unassigned[:10]}")
    if duplicates:
        raise RuntimeError(f"files matched multiple areas: {duplicates[:10]}")
    return by_area


def list_changed_files(repo, base_rev, head_rev):
    raw = run_cmd(
        ["git", "-C", str(repo), "diff", "--name-only", "-z", f"{base_rev}..{head_rev}"]
    )
    files = sorted(p.decode("utf-8") for p in raw.split(b"\x00") if p)
    if not files:
        raise RuntimeError(f"no changed files between {base_rev} and {head_rev}")
    if len(files) > MAX_CHANGED_FILES:
        raise RuntimeError(f"changed file count {len(files)} exceeds {MAX_CHANGED_FILES}")
    return files


def generate_patches(repo, base_rev, head_rev, out_dir):
    changed = list_changed_files(repo, base_rev, head_rev)
    by_area = classify_files(changed)
    patches_dir = out_dir / "patches"
    patches_dir.mkdir(parents=True, exist_ok=True)
    for stale in patches_dir.glob("*.patch"):
        stale.unlink()

    manifest_areas = []
    for name, description, _ in AREAS:
        files = sorted(by_area[name])
        if not files:
            continue
        diff_bytes = run_cmd(
            ["git", "-C", str(repo), "diff", "--binary", f"{base_rev}..{head_rev}", "--", *files]
        )
        if not diff_bytes:
            raise RuntimeError(f"empty diff for non-empty area {name}")
        patch_path = patches_dir / name
        patch_path.write_bytes(diff_bytes)
        manifest_areas.append({
            "patch": name,
            "description": description,
            "file_count": len(files),
            "bytes": len(diff_bytes),
            "sha256": hashlib.sha256(diff_bytes).hexdigest(),
            "files": files,
        })
    return changed, manifest_areas


def verify_roundtrip(repo, base_rev, head_rev, out_dir, manifest_areas):
    verify_dir = out_dir / f"verify-{os.getpid()}"
    if verify_dir.exists():
        shutil.rmtree(verify_dir)
    verify_dir.mkdir(parents=True, exist_ok=True)
    try:
        all_files = sorted(f for area in manifest_areas for f in area["files"])
        base_files = set(
            p.decode("utf-8")
            for p in run_cmd(
                ["git", "-C", str(repo), "ls-tree", "-r", "--name-only", "-z", base_rev, "--", *all_files]
            ).split(b"\x00")
            if p
        )
        for rel in sorted(base_files):
            blob = run_cmd(["git", "-C", str(repo), "show", f"{base_rev}:{rel}"])
            dest = verify_dir / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(blob)

        patch_results = []
        patches_dir = out_dir / "patches"
        for area in manifest_areas:
            patch_path = patches_dir / area["patch"]
            proc = subprocess.run(
                ["patch", "--strip=1", "--fuzz=0", "--batch", "--no-backup-if-mismatch"],
                cwd=verify_dir,
                input=patch_path.read_bytes(),
                capture_output=True,
                timeout=CMD_TIMEOUT_S,
            )
            stdout = proc.stdout.decode("utf-8", errors="replace")
            stderr = proc.stderr.decode("utf-8", errors="replace")
            if proc.returncode != 0:
                raise RuntimeError(f"patch {area['patch']} failed (rc={proc.returncode}): {stdout} {stderr}")
            if OFFSET_OR_FUZZ_RE.search(stdout) or OFFSET_OR_FUZZ_RE.search(stderr):
                raise RuntimeError(f"patch {area['patch']} had fuzz or offset:\n{stdout}\n{stderr}")
            patch_results.append({"patch": area["patch"], "applied_clean": True})

        head_files = set(
            p.decode("utf-8")
            for p in run_cmd(
                ["git", "-C", str(repo), "ls-tree", "-r", "--name-only", "-z", head_rev, "--", *all_files]
            ).split(b"\x00")
            if p
        )
        actual_files = set(
            str(p.relative_to(verify_dir))
            for p in verify_dir.rglob("*")
            if p.is_file() or p.is_symlink()
        )
        if actual_files != head_files:
            missing = sorted(head_files - actual_files)
            extra = sorted(actual_files - head_files)
            raise RuntimeError(f"file set mismatch vs {head_rev}: missing={missing[:10]} extra={extra[:10]}")

        mismatches = []
        for rel in sorted(head_files):
            expected = run_cmd(["git", "-C", str(repo), "show", f"{head_rev}:{rel}"])
            got = (verify_dir / rel).read_bytes()
            if got != expected:
                mismatches.append(rel)
        if mismatches:
            raise RuntimeError(f"byte mismatch vs {head_rev} in {len(mismatches)} files: {mismatches[:10]}")

        return {
            "base_rev": base_rev,
            "head_rev": head_rev,
            "checked_files": len(head_files),
            "deleted_files": len(base_files - head_files),
            "added_files": len(head_files - base_files),
            "empty_diff": True,
            "patches": patch_results,
        }
    finally:
        shutil.rmtree(verify_dir, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description="Split Firefox fork commits into one patch file per area.")
    parser.add_argument("--repo", type=Path, default=DEFAULT_REPO)
    parser.add_argument("--base", default=DEFAULT_BASE)
    parser.add_argument("--head", default=DEFAULT_HEAD)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--verify", action="store_true", help="Apply patches to base and prove empty diff vs head")
    args = parser.parse_args()

    changed, manifest_areas = generate_patches(args.repo, args.base, args.head, args.out)
    manifest = {
        "repo": str(args.repo),
        "base_rev": args.base,
        "head_rev": args.head,
        "total_changed_files": len(changed),
        "area_count": len(manifest_areas),
        "areas": manifest_areas,
    }
    if args.verify:
        manifest["verification"] = verify_roundtrip(
            args.repo, args.base, args.head, args.out, manifest_areas
        )
    args.out.mkdir(parents=True, exist_ok=True)
    manifest_path = args.out / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    ver_note = ""
    if args.verify:
        v = manifest["verification"]
        ver_note = (
            f"; verified clean --fuzz=0 apply and empty diff across "
            f"{v['checked_files']} head files (+{v['added_files']}/-{v['deleted_files']})"
        )
    print(
        f"OK: split {len(changed)} files into {len(manifest_areas)} area patches under {args.out / 'patches'}"
        f"{ver_note} (manifest: {manifest_path})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
