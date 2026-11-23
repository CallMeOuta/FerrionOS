#!/usr/bin/env python3
import argparse
import os
import pty
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RECIPE = "ferrion-hardware"
RECIPE_DIR = ROOT / "recipes/wip/drivers" / RECIPE
LOG_DIR = ROOT / "build/lxhost-link"
READ_CHUNK = 65536


def cook(arch, log_path):
    env = dict(os.environ, CI="1", COOKBOOK_LOGS="true")
    primary, secondary = pty.openpty()
    proc = subprocess.Popen(
        ["make", f"ARCH={arch}", f"cr.{RECIPE}"],
        cwd=ROOT, env=env, stdin=secondary, stdout=secondary, stderr=secondary, close_fds=True)
    os.close(secondary)
    with open(log_path, "wb") as log:
        while True:
            try:
                chunk = os.read(primary, READ_CHUNK)
            except OSError:
                break
            if not chunk:
                break
            log.write(chunk)
    os.close(primary)
    return proc.wait()


def lines(path):
    if not path.exists():
        return []
    return [line for line in path.read_text().splitlines() if line]


def main():
    parser = argparse.ArgumentParser(
        description="Cook the driver host (recipe ferrion-hardware) and fail on any symbol its link leaves "
                    "unresolved. Exit 0 green, 1 red.")
    parser.add_argument("--arch", default="x86_64")
    parser.add_argument("--no-cook", action="store_true", help="read the census of the last cook")
    args = parser.parse_args()

    census = RECIPE_DIR / "target" / f"{args.arch}-unknown-redox" / "build" / "census"
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    log_path = LOG_DIR / "cook.log"
    status = 0
    if not args.no_cook:
        if not (RECIPE_DIR / "recipe.toml").exists():
            print(f"RED: no host: there is no recipe {RECIPE}")
            return 1
        shutil.rmtree(census, ignore_errors=True)
        status = cook(args.arch, log_path)
        print(f"cook: exit {status}, log {log_path.relative_to(ROOT)}")

    summary = lines(census / "summary.txt")
    if not summary:
        print("RED: no host: the cook left no link census")
        return 1
    for line in summary:
        print(f"  {line}")
    unresolved = lines(census / "unresolved.txt")
    unneeded = lines(census / "refused-unneeded.txt")
    if unresolved:
        print(f"RED: {len(unresolved)} unresolved symbols:")
        for name in unresolved:
            print(f"  {name}")
    if unneeded:
        print(f"RED: {len(unneeded)} refusals name symbols nothing needs:")
        for name in unneeded:
            print(f"  {name}")
    if unresolved or unneeded:
        return 1
    if status != 0:
        print("RED: the link census is clean but the cook failed; see the log")
        return 1
    print("GREEN: the driver host links with no unresolved symbol")
    return 0


if __name__ == "__main__":
    sys.exit(main())
