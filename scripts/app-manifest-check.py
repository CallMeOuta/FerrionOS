#!/usr/bin/env python3

import argparse
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATE_MANIFEST = ROOT / "recipes" / "core" / "ferrion-manifest" / "crate" / "Cargo.toml"
TARGET_DIR = ROOT / "build" / "ferrion-manifest-target"
CHECKER_BIN = TARGET_DIR / "debug" / "ferrion-manifest-check"
NON_APPS_PATH = ROOT / "scripts" / "app-manifest-non-apps.toml"
FIXTURES_DIR = ROOT / "scripts" / "app-manifest-fixtures"
DEFAULT_CONFIG = ROOT / "config" / "x86_64" / "desktop.toml"
MIN_REASON_LEN = 8
MAX_REASON_LEN = 160


def tracked(*patterns):
    out = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", *patterns],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return [ROOT / p for p in out.stdout.splitlines() if (ROOT / p).is_file()]


def build_checker():
    TARGET_DIR.mkdir(parents=True, exist_ok=True)
    cmd = [
        "cargo",
        "build",
        "--quiet",
        "--locked",
        "--manifest-path",
        str(CRATE_MANIFEST),
        "--target-dir",
        str(TARGET_DIR),
    ]
    res = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
    if res.returncode != 0:
        sys.stderr.write(res.stdout)
        sys.stderr.write(res.stderr)
        raise RuntimeError(f"failed to build ferrion-manifest-check (exit {res.returncode})")
    if not CHECKER_BIN.is_file():
        raise RuntimeError(f"missing checker binary {CHECKER_BIN}")
    return CHECKER_BIN


def load_limits(checker):
    res = subprocess.run([str(checker), "limits"], capture_output=True, text=True, check=True)
    limits = {}
    for line in res.stdout.splitlines():
        key, _, raw = line.partition("=")
        if key and raw:
            limits[key.strip()] = int(raw.strip())
    for required in ("MAX_MANIFEST_BYTES", "MAX_RESERVED_IDS", "MAX_NON_APP_PACKAGES"):
        if required not in limits:
            raise RuntimeError(f"checker limits output missing {required}")
    return limits


def parse_error_code(stderr):
    line = stderr.strip().splitlines()[0] if stderr.strip() else ""
    if line.startswith("ERR "):
        code, _, _ = line[4:].partition(":")
        return code.strip()
    return "unknown-error"


def run_checker(checker, args):
    res = subprocess.run([str(checker), *args], capture_output=True, text=True)
    if res.returncode == 0:
        return True, res.stdout.strip()
    return False, parse_error_code(res.stderr), res.stderr.strip()


def recipe_dirs_by_name(recipes_root):
    mapping = {}
    duplicates = []
    for path in sorted(recipes_root.glob("**/recipe.toml")):
        rel_parts = path.relative_to(recipes_root).parts
        if "source" in rel_parts or "target" in rel_parts:
            continue
        pkg = path.parent.name
        if pkg in mapping:
            duplicates.append((pkg, mapping[pkg], path.parent))
            continue
        mapping[pkg] = path.parent
    return mapping, duplicates


def resolve_config_packages(config_path, seen=None):
    seen = seen if seen is not None else set()
    resolved = config_path.resolve()
    if resolved in seen:
        return {}
    seen.add(resolved)
    data = tomllib.loads(resolved.read_text(encoding="utf-8"))
    packages = {}
    for inc in data.get("include", []):
        packages.update(resolve_config_packages(resolved.parent / inc, seen))
    for pkg, rule in data.get("packages", {}).items():
        if rule == "ignore":
            packages.pop(pkg, None)
        else:
            packages[pkg] = rule
    return packages


def load_non_apps(path, max_non_apps):
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    if data.get("format") != 1:
        raise ValueError(f"{path}: format must be 1")
    extra_top = set(data.keys()) - {"format", "non_app"}
    if extra_top:
        raise ValueError(f"{path}: unknown top-level keys {sorted(extra_top)}")
    table = data.get("non_app", {})
    if not isinstance(table, dict):
        raise ValueError(f"{path}: non_app must be a table")
    if len(table) > max_non_apps:
        raise ValueError(f"{path}: {len(table)} non-app entries exceeds limit {max_non_apps}")
    out = {}
    for pkg, entry in table.items():
        if not isinstance(entry, dict):
            raise ValueError(f"{path}: non_app.{pkg} must be a table")
        extra = set(entry.keys()) - {"reason"}
        if extra:
            raise ValueError(f"{path}: non_app.{pkg} has unknown keys {sorted(extra)}")
        reason = entry.get("reason")
        if not isinstance(reason, str):
            raise ValueError(f"{path}: non_app.{pkg} missing string reason")
        if not (MIN_REASON_LEN <= len(reason.encode("utf-8")) <= MAX_REASON_LEN):
            raise ValueError(f"{path}: non_app.{pkg} reason length out of bounds")
        if reason.strip() != reason or any(ord(c) < 0x20 or ord(c) == 0x7F for c in reason):
            raise ValueError(f"{path}: non_app.{pkg} reason must be single-line plain text")
        out[pkg] = reason
    return out


def check_config(checker, config_path, recipes_root, non_apps):
    packages = resolve_config_packages(config_path)
    recipe_map, duplicates = recipe_dirs_by_name(recipes_root)
    failures = []
    for pkg, first, second in duplicates:
        failures.append(("duplicate-recipe", pkg, f"duplicate recipe directories {first} and {second}"))
    app_count = 0
    non_app_count = 0
    for pkg in sorted(packages):
        pkg_dir = recipe_map.get(pkg)
        if pkg_dir is None:
            failures.append(("missing-recipe", pkg, f"no recipe.toml found for {pkg}"))
            continue
        manifest_path = pkg_dir / "manifest.toml"
        has_manifest = manifest_path.is_file()
        in_non_apps = pkg in non_apps
        if in_non_apps and has_manifest:
            failures.append(
                (
                    "conflicting-non-app",
                    pkg,
                    f"{manifest_path} exists but {pkg} is also listed in non_app",
                )
            )
            continue
        if in_non_apps:
            non_app_count += 1
            continue
        if not has_manifest:
            failures.append(
                (
                    "missing-manifest",
                    pkg,
                    f"{pkg_dir.relative_to(ROOT) if pkg_dir.is_relative_to(ROOT) else pkg_dir} has no manifest.toml and is not listed in non_app",
                )
            )
            continue
        result = run_checker(checker, ["repo", pkg, str(manifest_path)])
        if result[0]:
            app_count += 1
        else:
            failures.append((result[1], pkg, result[2]))
    return app_count, non_app_count, failures


def write_reserved_ids(recipes_root, max_reserved):
    recipe_map, duplicates = recipe_dirs_by_name(recipes_root)
    if duplicates:
        pkg, first, second = duplicates[0]
        raise ValueError(f"duplicate-recipe: {pkg} in {first} and {second}")
    names = sorted(recipe_map.keys())
    if len(names) > max_reserved:
        raise ValueError(f"repository has {len(names)} recipes, limit is {max_reserved}")
    out_path = TARGET_DIR / "reserved-ids.txt"
    out_path.write_text("\n".join(names) + "\n", encoding="utf-8")
    return out_path, len(names)


def check_fixtures(checker, reserved_ids_path, non_apps):
    spec = tomllib.loads((FIXTURES_DIR / "fixtures.toml").read_text(encoding="utf-8"))
    if spec.get("format") != 1:
        raise ValueError("fixtures.toml format must be 1")
    cases = spec.get("case", [])
    if not cases:
        raise ValueError("fixtures.toml defines no cases")
    problems = []
    results = []
    for case in cases:
        name = case["name"]
        mode = case["mode"]
        expected = case["expect"]
        if mode == "repo":
            pkg = case["package"]
            path = FIXTURES_DIR / case["path"]
            res = run_checker(checker, ["repo", pkg, str(path)])
            actual = "ok" if res[0] else res[1]
            detail = res[1] if res[0] else res[2]
        elif mode == "outside":
            path = FIXTURES_DIR / case["path"]
            res = run_checker(checker, ["outside", str(reserved_ids_path), str(path)])
            actual = "ok" if res[0] else res[1]
            detail = res[1] if res[0] else res[2]
        elif mode == "grants":
            path = FIXTURES_DIR / case["path"]
            res = run_checker(checker, ["grants", str(path)])
            actual = "ok" if res[0] else res[1]
            detail = res[1] if res[0] else res[2]
        elif mode == "config":
            cfg_path = FIXTURES_DIR / case["config"]
            recipes_root = FIXTURES_DIR / case["recipes"]
            _, _, failures = check_config(checker, cfg_path, recipes_root, non_apps)
            if not failures:
                actual = "ok"
                detail = "no failures"
            else:
                actual = failures[0][0]
                detail = f"{failures[0][1]}: {failures[0][2]}"
        else:
            raise ValueError(f"unknown fixture mode {mode!r}")
        results.append((name, expected, actual, detail))
        if actual != expected:
            problems.append(f"fixture {name}: expected {expected}, got {actual} ({detail})")
    return results, problems


def check_all_repo_manifests(checker):
    failures = []
    checked = 0
    for path in sorted(tracked("recipes/**/manifest.toml")):
        pkg = path.parent.name
        res = run_checker(checker, ["repo", pkg, str(path)])
        checked += 1
        if not res[0]:
            failures.append((res[1], pkg, res[2]))
    return checked, failures


def main():
    parser = argparse.ArgumentParser(
        description="Validate FerrionOS format-1 app manifests using ferrion-manifest-check."
    )
    parser.add_argument("--config", default=str(DEFAULT_CONFIG))
    parser.add_argument("--non-apps", default=str(NON_APPS_PATH))
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--fixtures-only", action="store_true")
    mode.add_argument("--desktop-only", action="store_true")
    args = parser.parse_args()

    checker = build_checker()
    limits = load_limits(checker)
    non_apps = load_non_apps(Path(args.non_apps), limits["MAX_NON_APP_PACKAGES"])
    reserved_ids_path, repo_recipe_count = write_reserved_ids(
        ROOT / "recipes", limits["MAX_RESERVED_IDS"]
    )

    exit_code = 0

    if not args.desktop_only:
        fixture_results, fixture_problems = check_fixtures(checker, reserved_ids_path, non_apps)
        for name, expected, actual, detail in fixture_results:
            status = "PASS" if expected == actual else "FAIL"
            print(f"fixture {status} {name}: {actual} (expected {expected}) [{detail}]")
        if fixture_problems:
            for line in fixture_problems:
                print(line, file=sys.stderr)
            exit_code = 1
        else:
            print(
                f"Fixtures: {len(fixture_results)}/{len(fixture_results)} matched expected outcomes "
                f"(reserved repo ids: {repo_recipe_count})."
            )

    if not args.fixtures_only:
        config_path = Path(args.config)
        app_count, non_app_count, desktop_failures = check_config(
            checker, config_path, ROOT / "recipes", non_apps
        )
        all_checked, all_failures = check_all_repo_manifests(checker)
        for code, pkg, detail in desktop_failures:
            print(f"desktop FAIL {pkg}: {code} ({detail})", file=sys.stderr)
        for code, pkg, detail in all_failures:
            print(f"recipe FAIL {pkg}: {code} ({detail})", file=sys.stderr)
        if desktop_failures or all_failures:
            print(
                f"Desktop ({config_path.relative_to(ROOT) if config_path.is_relative_to(ROOT) else config_path}): "
                f"{app_count} apps valid, {non_app_count} non-apps listed, "
                f"{len(desktop_failures)} desktop failure(s), {len(all_failures)} repo-manifest failure(s)."
            )
            exit_code = 1
        else:
            print(
                f"Desktop ({config_path.relative_to(ROOT) if config_path.is_relative_to(ROOT) else config_path}): "
                f"{app_count} apps valid, {non_app_count} non-apps listed, "
                f"0 failures ({all_checked} repo manifest(s) checked)."
            )

    return exit_code


if __name__ == "__main__":
    sys.exit(main())
