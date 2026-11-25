#!/usr/bin/env python3

import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

RUST_LCONV_RE = re.compile(
    r"\b(?:libc::localeconv|libc::lconv|localeconv\s*\(|lconv)\b"
)
VENDORED_LIBC_DIR_RE = re.compile(r"(^|/)(?:ferrion-vendor|vendor)/libc(?:-[^/]+)?/")
VENDORED_LIBC_DEP_RE = re.compile(
    r"""^\s*libc\s*=\s*\{[^}]*\bpath\s*=\s*["'][^"']+["']""", re.MULTILINE
)
PATCH_RUST_FILE_RE = re.compile(r"^\+\+\+\s+(?:b/)?(.+\.rs)(?:\s|$)")
SKIP_RECIPES = {"recipes/core/relibc/recipe.toml"}


def tracked(*paths):
    out = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", *paths],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return [ROOT / p for p in out.stdout.splitlines() if (ROOT / p).is_file()]


def scan_patch_for_rust_lconv(patch_path):
    in_rust_file = False
    try:
        lines = patch_path.read_text(errors="ignore").splitlines()
    except OSError:
        return False
    for line in lines:
        if line.startswith("+++ "):
            in_rust_file = bool(PATCH_RUST_FILE_RE.match(line))
            continue
        if line.startswith("--- ") or line.startswith("diff --git "):
            continue
        if in_rust_file and not line.startswith("-") and RUST_LCONV_RE.search(line):
            return True
    return False


def recipe_uses_rust_lconv(recipe_dir):
    hits = []
    for path in sorted(recipe_dir.rglob("*.rs")):
        rel = path.relative_to(recipe_dir).as_posix()
        if rel.startswith("target/") or "/target/" in f"/{rel}/":
            continue
        if VENDORED_LIBC_DIR_RE.search(rel):
            continue
        try:
            text = path.read_text(errors="ignore")
        except OSError:
            continue
        if RUST_LCONV_RE.search(text):
            hits.append(rel)
    for patch_path in sorted(recipe_dir.glob("*.patch")):
        if scan_patch_for_rust_lconv(patch_path):
            hits.append(patch_path.name)
    return hits


def recipe_vendors_libc(recipe_dir, recipe):
    for cargo_toml in sorted(recipe_dir.rglob("Cargo.toml")):
        rel = cargo_toml.relative_to(recipe_dir).as_posix()
        if rel.startswith("target/") or "/target/" in f"/{rel}/":
            continue
        if VENDORED_LIBC_DIR_RE.search(rel):
            continue
        try:
            text = cargo_toml.read_text(errors="ignore")
        except OSError:
            continue
        if VENDORED_LIBC_DEP_RE.search(text):
            return True
    for patch_path in sorted(recipe_dir.glob("*.patch")):
        try:
            text = patch_path.read_text(errors="ignore")
        except OSError:
            continue
        for line in text.splitlines():
            if line.startswith("+") and not line.startswith("+++") and VENDORED_LIBC_DEP_RE.search(line[1:]):
                return True
    source = recipe.get("source", {})
    if isinstance(source, dict):
        script = source.get("script", "") or ""
        if "rust-lang/libc" in script or "ferrion-vendor/libc" in script:
            return True
    return False


def main():
    failures = []
    for recipe_path in sorted(tracked("recipes/**/recipe.toml")):
        rel = recipe_path.relative_to(ROOT).as_posix()
        if rel in SKIP_RECIPES:
            continue
        recipe = tomllib.loads(recipe_path.read_text())
        build = recipe.get("build", {})
        crate_patches = build.get("crate_patches", [])

        for crate in crate_patches:
            manifest = ROOT / "patches" / "crates" / crate / "manifest.toml"
            if not manifest.is_file():
                failures.append(
                    f"{rel}: crate_patches lists {crate!r} without {manifest.relative_to(ROOT).as_posix()}"
                )

        hits = recipe_uses_rust_lconv(recipe_path.parent)
        if not hits:
            continue

        if "libc" not in crate_patches:
            failures.append(
                f"{rel}: Rust code references localeconv/lconv ({', '.join(hits[:3])}) without [build] crate_patches = [\"libc\"]"
            )

        if recipe_vendors_libc(recipe_path.parent, recipe):
            source = recipe.get("source", {})
            script = source.get("script", "") if isinstance(source, dict) else ""
            if not re.search(r"\bcookbook_crate_patch\s+libc\b", script):
                failures.append(
                    f"{rel}: recipe vendors libc and uses localeconv/lconv without calling `cookbook_crate_patch libc` in [source].script"
                )

    for line in failures:
        print(line)
    if failures:
        print(f"{len(failures)} crate-patches check failure(s)")
        return 1
    print("All Rust localeconv/lconv recipes opt into crate_patches = [\"libc\"].")
    return 0


if __name__ == "__main__":
    sys.exit(main())
