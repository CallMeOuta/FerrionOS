#!/usr/bin/env python3
# The virtgpu layout check. Compiles Linux's `virtgpu_drm.h` in C and the `virtgpu` module of
# relibc's crate `redox-ioctl` in Rust, then compares every structure's size, every field's
# offset, size and type, and every number the header defines, the request numbers among them.
# Then it checks each payload the module defines for a request: its fixed part must be the
# header's structure without the array pointers, packed in the header's order, and each array's
# pointer, count and element must match the header.
#
# Prints each mismatch and a summary. Exits 0 when everything matches, 1 on a mismatch, 2 when
# the check cannot run: no C compiler or cargo, relibc's source not fetched (`make f.relibc`),
# or a copied header that differs from the one recorded below.
#
# Run from the repository root:
#   python3 scripts/virtgpu-layout-check.py [--header FILE] [--redox-ioctl DIR] [--work DIR]
# --header        another copy of virtgpu_drm.h to compare against, with its drm.h beside it,
#                 such as the one Mesa carries in include/drm-uapi/
# --redox-ioctl   the crate's directory (default: recipes/core/relibc/source/redox-ioctl)
# --work          where the generated programs are built (default: build/virtgpu-layout)
#
# Needs a C compiler that has Linux's own uapi headers (linux/types.h, asm/ioctl.h), cargo, and
# the toolchain relibc pins in its rust-toolchain.toml; cargo fetches the crate's dependencies
# on the first run. Python standard library only.

import argparse
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Copied unchanged from Linux at the tag v7.3, commit c03c1a14507dfaf8b856c2608f9ac216021fc222,
# include/uapi/drm/. Each file keeps its own license notice.
HEADER_DIR = ROOT / "scripts/virtgpu-layout/linux"
HEADER_SHA256 = {
    "virtgpu_drm.h": "297d6ac774fdcd2c912c7c615f66cb643fcdeb804262eca0e68f20d508854a55",
    "drm.h": "51fb1105a52c2085e6bdf4d6a3be347dfe8097d2375d8720d63106cc59f3fb6b",
    "drm_mode.h": "cfa0f3dff969178ec1f30abcb2072feb1a0b9f3aabb55580f3b08a77fd13e1e8",
}

C_TYPES = {
    "__u8": "u8", "__u16": "u16", "__u32": "u32", "__u64": "u64",
    "__s8": "i8", "__s16": "i16", "__s32": "i32", "__s64": "i64",
}
PRIMITIVE_SIZES = {"u8": 1, "u16": 2, "u32": 4, "u64": 8, "i8": 1, "i16": 2, "i32": 4, "i64": 8}

RUST_KEYWORDS = set(
    "as async await box break const continue crate do dyn else enum extern false final fn for gen"
    " if impl in let loop macro match mod move mut override priv pub ref return self static struct"
    " super trait true try type typeof unsafe unsized use virtual where while yield abstract become"
    .split()
)


class CannotRun(Exception):
    pass


def parse_header(text):
    """The structures, object-like macros and request numbers that a virtgpu_drm.h defines."""
    text = re.sub(r"\\\n", " ", text)
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    text = re.sub(r"//[^\n]*", " ", text)

    constants = []
    requests = {}
    for m in re.finditer(r"^[ \t]*#[ \t]*define[ \t]+(\w+)(\(?)[ \t]*(.*)$", text, flags=re.M):
        name, function_like, value = m.groups()
        if function_like or not value.strip():
            continue
        constants.append(name)
        target = re.search(r"\bstruct\s+(\w+)", value)
        if name.startswith("DRM_IOCTL_") and target:
            requests[name] = target.group(1)

    structs = {}
    code = re.sub(r"^[ \t]*#.*$", "", text, flags=re.M)
    for m in re.finditer(r"\bstruct\s+(\w+)\s*\{(.*?)\}\s*;", code, flags=re.S):
        fields = []
        for decl in m.group(2).split(";"):
            decl = " ".join(decl.split())
            if not decl:
                continue
            f = re.fullmatch(r"(__[us](?:8|16|32|64)|struct \w+) (\w+)(?:\[(\d+)\])?", decl)
            if not f:
                raise CannotRun(f"cannot read the field `{decl}` of struct {m.group(1)}")
            ctype, name, count = f.groups()
            rtype = C_TYPES.get(ctype, ctype.removeprefix("struct "))
            if count:
                rtype = f"[{rtype}; {count}]"
            fields.append((name, ctype, rtype))
        structs[m.group(1)] = fields
    if not structs or not constants:
        raise CannotRun("the header defines no structure or no number")
    return structs, constants, requests


def rust_ident(name):
    return f"r#{name}" if name in RUST_KEYWORDS else name


def c_program(header, structs, constants):
    lines = [
        "#include <stddef.h>",
        "#include <stdio.h>",
        f'#include "{header.name}"',
        "int main(void) {",
    ]
    for s, fields in structs.items():
        lines.append(f'    printf("struct {s} %zu\\n", sizeof(struct {s}));')
        for name, _, rtype in fields:
            lines.append(
                f'    printf("field {s} {name} %zu %zu {rtype}\\n",'
                f" offsetof(struct {s}, {name}), sizeof(((struct {s} *)0)->{name}));"
            )
    for c in constants:
        lines.append(f'    printf("const {c} %llu\\n", (unsigned long long)({c}));')
    lines += ["    return 0;", "}", ""]
    return "\n".join(lines)


def rust_program(structs, constants):
    lines = [
        "use redox_ioctl::virtgpu as v;",
        "",
        "fn type_of<F>(_: &F) -> &'static str {",
        "    let name = core::any::type_name::<F>();",
        "    name.rsplit(\"::\").next().unwrap_or(name)",
        "}",
        "",
        "fn main() {",
    ]
    for s, fields in structs.items():
        lines.append(f'    println!("struct {s} {{}}", size_of::<v::{s}>());')
        lines.append(f"    let x: v::{s} = unsafe {{ core::mem::zeroed() }};")
        for name, _, _ in fields:
            r = rust_ident(name)
            lines.append(
                f'    println!("field {s} {name} {{}} {{}} {{}}", core::mem::offset_of!(v::{s}, {r}),'
                f" size_of_val(&x.{r}), type_of(&x.{r}));"
            )
    for c in constants:
        lines.append(f'    println!("const {c} {{}}", v::{c} as u64);')
    lines += [
        "    for (nr, name, layout) in v::PAYLOADS {",
        '        println!("payload {name} {nr} {}", layout.fixed_size);',
        "        for (field, offset, size) in layout.fixed {",
        '            println!("fixed {name} {field} {offset} {size}");',
        "        }",
        "        for (field, count, element, size) in layout.arrays {",
        '            println!("array {name} {field} {count} {element} {size}");',
        "        }",
        "    }",
        "}",
        "",
    ]
    return "\n".join(lines)


def read_records(output):
    records = {"struct": {}, "field": {}, "const": {}, "payload": {}, "fixed": {}, "array": {}}
    for line in output.splitlines():
        kind, *rest = line.split(" ")
        if kind == "struct":
            records["struct"][rest[0]] = int(rest[1])
        elif kind == "field":
            s, f, offset, size, *rtype = rest
            records["field"][(s, f)] = (int(offset), int(size), " ".join(rtype))
        elif kind == "const":
            records["const"][rest[0]] = int(rest[1])
        elif kind == "payload":
            records["payload"][rest[0]] = (int(rest[1]), int(rest[2]))
        elif kind == "fixed":
            records["fixed"].setdefault(rest[0], []).append((rest[1], int(rest[2]), int(rest[3])))
        elif kind == "array":
            records["array"].setdefault(rest[0], []).append(
                (rest[1], rest[2], " ".join(rest[3:-1]), int(rest[-1]))
            )
    return records


def run(cmd, **kwargs):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, **kwargs)
    except FileNotFoundError as e:
        raise CannotRun(f"{cmd[0]} is not installed") from e


def build_c(header, structs, constants, work):
    source = work / "layout.c"
    binary = work / "layout-c"
    source.write_text(c_program(header, structs, constants))
    cc = os.environ.get("CC", "cc")
    # Linux's `make headers_install` strips the kernel's `__user` annotation; the copies in
    # Linux's tree still carry it.
    flags = ["-std=c11", "-Wall", "-Werror", "-D__user=", "-I", str(header.parent)]
    built = run([cc, *flags, "-o", str(binary), str(source)])
    if built.returncode != 0:
        raise CannotRun(f"the C program does not compile:\n{built.stderr}")
    return run([str(binary)], check=True).stdout


def build_rust(crate, structs, constants, work):
    project = work / "rust"
    (project / "src").mkdir(parents=True, exist_ok=True)
    (project / "Cargo.toml").write_text(
        "[package]\n"
        'name = "virtgpu-layout"\n'
        'version = "0.0.0"\n'
        'edition = "2024"\n'
        "publish = false\n\n"
        "[dependencies]\n"
        f'redox-ioctl = {{ path = "{crate.as_posix()}" }}\n\n'
        "[workspace]\n"
    )
    (project / "src/main.rs").write_text(rust_program(structs, constants))
    lock = crate.parent / "Cargo.lock"
    if lock.is_file():
        shutil.copyfile(lock, project / "Cargo.lock")

    env = dict(os.environ, CARGO_TARGET_DIR=str(project / "target"))
    pin = crate.parent / "rust-toolchain.toml"
    if pin.is_file():
        env["RUSTUP_TOOLCHAIN"] = tomllib.loads(pin.read_text())["toolchain"]["channel"]
    built = run(["cargo", "run", "--quiet", "--manifest-path", str(project / "Cargo.toml")], env=env)
    if built.returncode != 0:
        if re.search(r"^error(\[E\d+\])?:", built.stderr, flags=re.M) and "could not compile" in built.stderr:
            return None, built.stderr
        raise CannotRun(f"cargo failed:\n{built.stderr}")
    return built.stdout, None


def compare(structs, constants, requests, c, r):
    """Yields (item, mismatch or None) for every item compared."""
    for s, fields in structs.items():
        if s not in r["struct"]:
            yield f"struct {s}", "missing from the module"
            continue
        yield f"struct {s}: size", None if c["struct"][s] == r["struct"][s] else (
            f"{c['struct'][s]} in the header, {r['struct'][s]} in the module"
        )
        for name, _, _ in fields:
            want = c["field"][(s, name)]
            have = r["field"].get((s, name))
            yield f"{s}.{name}: offset, size, type", None if want == have else (
                f"{want} in the header, {have} in the module"
            )

    for name in constants:
        want, have = c["const"][name], r["const"].get(name)
        yield f"{name}", None if want == have else f"{want:#x} in the header, {have} in the module"

    request_of = {target: c["const"][name] for name, target in requests.items()}
    for s, (nr, fixed_size) in r["payload"].items():
        prefix = f"payload {s}"
        if s not in structs:
            yield prefix, "names no structure of the header"
            continue
        request = request_of.get(s)
        if request is None:
            yield prefix, "no request of the header takes this structure"
            continue
        yield f"{prefix}: function number", None if nr == request & 0xFF else (
            f"{nr:#x}, the request's low byte is {request & 0xFF:#x}"
        )
        size_in_request = (request >> 16) & 0x3FFF
        yield f"{prefix}: size in the request", None if size_in_request == c["struct"][s] else (
            f"the request encodes {size_in_request}, the structure is {c['struct'][s]}"
        )

        header_fields = {name: c["field"][(s, name)] for name, _, _ in structs[s]}
        ctype_of = {name: ctype for name, ctype, _ in structs[s]}
        arrays = r["array"].get(s, [])
        pointers = {a[0] for a in arrays}
        for pointer, count, element, size in arrays:
            item = f"{prefix}: array {pointer}"
            if ctype_of.get(pointer) != "__u64":
                yield item, f"{pointer} is not a __u64 field of the header"
            elif ctype_of.get(count) not in C_TYPES:
                yield item, f"its count {count} is not an integer field of the header"
            else:
                want = c["struct"].get(element, PRIMITIVE_SIZES.get(element))
                yield item, None if want == size else f"element {element} is {size} bytes, the header's is {want}"

        expected, offset = [], 0
        for name, _, _ in structs[s]:
            if name not in pointers:
                expected.append((name, offset, header_fields[name][1]))
                offset += header_fields[name][1]
        have = r["fixed"].get(s, [])
        yield f"{prefix}: fixed part", None if have == expected and fixed_size == offset else (
            f"expected {expected} ({offset} bytes), the module has {have} ({fixed_size} bytes)"
        )


def main():
    parser = argparse.ArgumentParser(description="Compare Linux's virtgpu_drm.h with redox-ioctl's virtgpu module.")
    parser.add_argument("--header", type=Path, default=HEADER_DIR / "virtgpu_drm.h")
    parser.add_argument("--redox-ioctl", type=Path, default=ROOT / "recipes/core/relibc/source/redox-ioctl")
    parser.add_argument("--work", type=Path, default=ROOT / "build/virtgpu-layout")
    args = parser.parse_args()
    header, crate, work = args.header.resolve(), args.redox_ioctl.resolve(), args.work.resolve()

    try:
        if not header.is_file():
            raise CannotRun(f"no header at {header}")
        if header.parent == HEADER_DIR:
            for name, digest in HEADER_SHA256.items():
                if hashlib.sha256((HEADER_DIR / name).read_bytes()).hexdigest() != digest:
                    raise CannotRun(f"{HEADER_DIR / name} differs from Linux v7.3's")
        if not (crate / "src/virtgpu.rs").is_file():
            raise CannotRun(f"no virtgpu module in {crate}: fetch relibc's source with `make f.relibc`")
        structs, constants, requests = parse_header(header.read_text())
        work.mkdir(parents=True, exist_ok=True)
        c = read_records(build_c(header, structs, constants, work))
        output, errors = build_rust(crate, structs, constants, work)
    except CannotRun as e:
        print(f"virtgpu layout: cannot run: {e}", file=sys.stderr)
        return 2

    print(f"header: {header} (sha256 {hashlib.sha256(header.read_bytes()).hexdigest()})")
    print(f"module: {crate / 'src/virtgpu.rs'}")
    if output is None:
        first_place = {}
        for why, place in re.findall(r"^error(?:\[E\d+\])?: (.*)\n\s*--> (\S+)", errors, flags=re.M):
            first_place.setdefault(why, place)
        for why, place in first_place.items():
            print(f"MISMATCH {why} ({place})")
        if "could not compile `redox-ioctl`" in errors:
            print("FAIL: redox-ioctl does not compile")
        else:
            print("FAIL: the module lacks names the header defines")
        return 1

    r = read_records(output)
    results = list(compare(structs, constants, requests, c, r))
    mismatches = [(item, why) for item, why in results if why]
    for item, why in mismatches:
        print(f"MISMATCH {item}: {why}")
    served = set(r["payload"])
    unserved = sorted(n for n, s in requests.items() if s not in served)
    print(f"structures {len(structs)}, numbers {len(constants)}, requests {len(requests)}, payloads {len(served)}")
    if unserved:
        print(f"requests without a payload: {', '.join(unserved)}")
    if mismatches:
        print(f"FAIL: {len(mismatches)} of {len(results)} items differ")
        return 1
    print(f"PASS: {len(results)} items match")
    return 0


if __name__ == "__main__":
    sys.exit(main())
