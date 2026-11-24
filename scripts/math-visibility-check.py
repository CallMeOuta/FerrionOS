#!/usr/bin/env python3
import argparse
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

XSI_SRC = """\
#include <math.h>
double probe_xsi(double x) {
    return j0(x) + j1(x) + jn(2, x) + y0(x) + y1(x) + yn(2, x) + (double)signgam;
}
"""

BSD_SRC = """\
#include <math.h>
float probe_bsd(float x) {
    int s = 0;
    return j0f(x) + j1f(x) + jnf(2, x) + y0f(x) + y1f(x) + ynf(2, x)
        + lgammaf_r(x, &s) + (float)lgamma_r((double)x, &s)
        + (float)lgammal_r((long double)x, &s);
}
"""

INT_J1_SRC = """\
#include <math.h>
int j1 = 42;
"""

DREMF_SRC = """\
#include <math.h>
float probe_dremf(float x, float y) {
    return dremf(x, y);
}
"""

C_ROWS = [
    ("c17-strict", ["-std=c17"]),
    ("gnu17", ["-std=gnu17"]),
    ("ffmpeg-xsi600", ["-std=c17", "-D_POSIX_C_SOURCE=200112", "-D_XOPEN_SOURCE=600"]),
    ("c17-xsi700", ["-std=c17", "-D_XOPEN_SOURCE=700"]),
    ("c17-default", ["-std=c17", "-D_DEFAULT_SOURCE"]),
    ("mpv-gnu", ["-std=c17", "-D_GNU_SOURCE"]),
    ("libplacebo-gnu", ["-std=c17", "-D_GNU_SOURCE", "-U__STRICT_ANSI__"]),
]

CXX_ROWS = [
    ("c++17-strict", ["-std=c++17"], ["-std=c++17", "-U_GNU_SOURCE"]),
    ("gnu++17", ["-std=gnu++17"], ["-std=gnu++17"]),
    (
        "ffmpeg-c++17-xsi600",
        ["-std=c++17", "-D_POSIX_C_SOURCE=200112", "-D_XOPEN_SOURCE=600"],
        ["-std=c++17", "-U_GNU_SOURCE", "-D_POSIX_C_SOURCE=200112", "-D_XOPEN_SOURCE=600"],
    ),
    (
        "c++17-xsi700",
        ["-std=c++17", "-D_XOPEN_SOURCE=700"],
        ["-std=c++17", "-U_GNU_SOURCE", "-D_XOPEN_SOURCE=700"],
    ),
    (
        "c++17-default",
        ["-std=c++17", "-D_DEFAULT_SOURCE"],
        ["-std=c++17", "-U_GNU_SOURCE", "-D_DEFAULT_SOURCE"],
    ),
    ("mpv-c++17-gnu", ["-std=c++17", "-D_GNU_SOURCE"], ["-std=c++17", "-D_GNU_SOURCE"]),
]


def compiles(compiler, lang, flags, src, extra_include=None):
    cmd = [compiler, *flags]
    if lang == "c":
        cmd.append("-Werror=implicit-function-declaration")
    if extra_include is not None:
        cmd.extend(["-isystem", str(extra_include)])
    cmd.extend(["-x", lang, "-c", "-", "-o", os.devnull])
    proc = subprocess.run(cmd, input=src, text=True, capture_output=True)
    return proc.returncode == 0


def default_include(arch):
    staged = ROOT / "recipes" / "core" / "relibc" / "target" / f"{arch}-unknown-redox" / "stage" / "usr" / "include"
    if (staged / "math.h").is_file():
        return staged
    return ROOT / "prefix" / f"{arch}-unknown-redox" / "sysroot" / f"{arch}-unknown-redox" / "include"


def main():
    parser = argparse.ArgumentParser(
        description="Differential check of relibc math.h Bessel and gamma visibility against host glibc."
    )
    parser.add_argument("--arch", default="x86_64")
    parser.add_argument("--include", type=Path, default=None)
    parser.add_argument("--cc", default=None)
    parser.add_argument("--cxx", default=None)
    parser.add_argument("--host-cc", default="gcc")
    parser.add_argument("--host-cxx", default="g++")
    args = parser.parse_args()

    sysroot_bin = ROOT / "prefix" / f"{args.arch}-unknown-redox" / "sysroot" / "bin"
    cc = args.cc or str(sysroot_bin / f"{args.arch}-unknown-redox-gcc")
    cxx = args.cxx or str(sysroot_bin / f"{args.arch}-unknown-redox-g++")
    inc = args.include or default_include(args.arch)
    if not (inc / "math.h").is_file():
        print(f"ERROR: math.h not found under {inc}", file=sys.stderr)
        sys.exit(2)

    print(f"include={inc}")
    mismatches = 0
    total = 0

    for name, flags in C_ROWS:
        for probe_name, src in (("xsi", XSI_SRC), ("bsd", BSD_SRC), ("int_j1", INT_J1_SRC)):
            total += 1
            glibc_ok = compiles(args.host_cc, "c", flags, src)
            ferrion_ok = compiles(cc, "c", flags, src, extra_include=inc)
            verdict = "MATCH" if glibc_ok == ferrion_ok else "DIFF"
            if verdict != "MATCH":
                mismatches += 1
            print(
                f"C   {name:22s} {probe_name:7s} glibc={'OK' if glibc_ok else 'FAIL':4s} "
                f"ferrion={'OK' if ferrion_ok else 'FAIL':4s} {verdict}"
            )
        total += 1
        dremf_ok = compiles(cc, "c", flags, DREMF_SRC, extra_include=inc)
        verdict = "PASS" if not dremf_ok else "FAIL"
        if dremf_ok:
            mismatches += 1
        print(
            f"C   {name:22s} {'dremf':7s} expected=HIDE ferrion={'OK' if dremf_ok else 'HIDE':4s} {verdict}"
        )

    for name, ferrion_flags, glibc_flags in CXX_ROWS:
        for probe_name, src in (("xsi", XSI_SRC), ("bsd", BSD_SRC)):
            total += 1
            glibc_ok = compiles(args.host_cxx, "c++", glibc_flags, src)
            ferrion_ok = compiles(cxx, "c++", ferrion_flags, src, extra_include=inc)
            verdict = "MATCH" if glibc_ok == ferrion_ok else "DIFF"
            if verdict != "MATCH":
                mismatches += 1
            print(
                f"C++ {name:22s} {probe_name:7s} glibc={'OK' if glibc_ok else 'FAIL':4s} "
                f"ferrion={'OK' if ferrion_ok else 'FAIL':4s} {verdict}"
            )

    passed = total - mismatches
    print(f"summary: {passed}/{total} matched, {mismatches} mismatches")
    sys.exit(0 if mismatches == 0 else 1)


if __name__ == "__main__":
    main()
