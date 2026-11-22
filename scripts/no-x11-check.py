#!/usr/bin/env python3

import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

X11 = re.compile(
    r"^(libx11|libx11-compat|libxcb|libxau|libxdmcp|libice|libsm|libfs|libdmx|libfontenc"
    r"|libx(aw|composite|cursor|cvt|damage|ext|fixes|font2|ft|i|inerama|kbfile|klavier|mu|pm"
    r"|randr|render|res|scrnsaver|shmfence|ss|t|tst|v|vmc|xf86vm)"
    r"|x11|x11proto.*|xcb-.*|xextproto|xtrans|xorg-.*|xserver-.*|xwayland|.*-x11.*|x11-.*)$"
)


def is_x11(dep):
    """Whether a dependency names an X11 package, or the X11 optional part (`pkg.x11`) of one."""
    return any(X11.match(part) for part in dep.removeprefix("host:").split("."))


def tracked(*paths):
    out = subprocess.run(["git", "ls-files", "--cached", "--others", "--exclude-standard", *paths],
                         cwd=ROOT, capture_output=True, text=True, check=True)
    return [ROOT / p for p in out.stdout.splitlines() if (ROOT / p).is_file()]


def main():
    found = []
    for path in tracked("recipes/**/recipe.toml"):
        rel = path.relative_to(ROOT).as_posix()
        if is_x11(path.parent.name):
            found.append(f"{rel}: recipe of an X11 package")
        recipe = tomllib.loads(path.read_text())
        build, package = recipe.get("build", {}), recipe.get("package", {})
        lists = [build.get("dependencies", []), build.get("dev-dependencies", []), package.get("dependencies", [])]
        lists += [opt.get("dependencies", []) for opt in recipe.get("optional-packages", [])]
        found += [f"{rel}: depends on {dep}" for deps in lists for dep in deps if is_x11(dep)]
    for path in tracked("config/**/*.toml"):
        packages = tomllib.loads(path.read_text()).get("packages", {})
        found += [
            f"{path.relative_to(ROOT).as_posix()}: installs {pkg}"
            for pkg, rule in packages.items()
            if is_x11(pkg) and rule != "ignore"
        ]
    for line in found:
        print(line)
    print(f"{len(found)} X11 package(s) found" if found else "No X11 package in recipes or configs.")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
