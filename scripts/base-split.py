#!/usr/bin/env python3
import hashlib
import re
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
RULES = next((file for file in (HERE / "upstream.toml", HERE.parent / "components" / "upstream.toml") if file.is_file()),
             HERE / "upstream.toml")
USAGE = f"""Splits a patch to base by path (ADR 0047). Each file section of the patch goes to
exactly one of three places, whole and unchanged:
  drivers  the file is under the moved prefix: a commit in ferrion-hardware
  rest     every other file, and a file under a kept prefix: a commit in components/base
  dropped  the file is under a dropped prefix: in neither repository
So applying both parts gives the bytes the whole patch gives, outside the dropped paths.

Usage: {sys.argv[0]} [--rules <file>] split [--renamed] <patch> <drivers-part> <rest-part>
       {sys.argv[0]} [--rules <file>] report <patch>...
       {sys.argv[0]} [--rules <file>] filter <side>
       {sys.argv[0]} [--rules <file>] layout
       {sys.argv[0]} [--rules <file>] prefixes kept|dropped
       {sys.argv[0]} [--rules <file>] subject <patch name>
  split   writes the two parts (an empty file when a part has no section) and prints the patch's
          class (drivers, rest or mixed) and the number of dropped sections. The patch's own
          description is not copied: the commit message carries it. With --renamed the rest
          part names a kept file by the path [base.split.renamed] gives it in components/base
  report  prints one line per patch (class, sections in the drivers part, in the rest part,
          dropped) and the totals
  filter  copies from standard input to standard output the lines whose path is on <side>
          (drivers, rest or dropped). A line is a path of base's tree, or a line of
          git ls-tree, whose path follows a tab
  layout  copies the lines of git ls-tree or git ls-files -s of a whole tree of base that
          components/base keeps, each kept file under the path [base.split.renamed] gives it
  prefixes prints the kept or the dropped prefixes of the rules, one per line
  subject prints the description [base.split.subjects] gives for the drivers commit of a patch
          whose own subject would pass 72 columns there, or an empty line
  --rules the file whose [base.split] table gives moved, kept and dropped; by default
          upstream.toml beside this script, or components/upstream.toml of its repository
Exit code 1 when a patch has a section this script cannot place: a rename or a copy across a
boundary, a binary file, or a section with no path."""
SECTION = re.compile(rb"^diff --git a/(\S+) b/(\S+)$")
HUNK = re.compile(rb"^@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@")
PLAIN_OLD = re.compile(rb"^--- (?:a/)?(\S+)")
PLAIN_NEW = re.compile(rb"^\+\+\+ (?:b/)?(\S+)")


class Rules:
    def __init__(self, file):
        table = tomllib.loads(Path(file).read_text())["base"]["split"]
        self.moved = table["moved"].rstrip("/") + "/"
        self.kept = tuple(path.rstrip("/") + "/" for path in table["kept"])
        self.dropped = tuple(path.rstrip("/") + "/" for path in table["dropped"])
        self.subjects = table.get("subjects", {})
        self.renamed = {old.rstrip("/") + "/": new.rstrip("/") + "/" for old, new in table.get("renamed", {}).items()}
        for path in self.kept + self.dropped:
            if not path.startswith(self.moved):
                raise ValueError(f"{path} is not under {self.moved}")

    def side(self, path):
        if not path.startswith(self.moved) or path.startswith(self.kept):
            return "rest"
        return "dropped" if path.startswith(self.dropped) else "drivers"

    def rename(self, section):
        out = []
        header = True
        for line in section:
            header = header and not HUNK.match(line)
            if header:
                for old, new in self.renamed.items():
                    for mark in (b"a/", b"b/", b"from ", b"to "):
                        line = line.replace(mark + old.encode(), mark + new.encode())
            out.append(line)
        return out


def sections(data):
    lines = data.splitlines(keepends=True)
    starts = []
    in_header = False
    index = 0
    while index < len(lines):
        line = lines[index]
        hunk = HUNK.match(line)
        if hunk and starts:
            old = int(hunk.group(1) or 1)
            new = int(hunk.group(2) or 1)
            index += 1
            while index < len(lines) and (old > 0 or new > 0):
                mark = lines[index][:1]
                if mark in (b" ", b"\n"):
                    old -= 1
                    new -= 1
                elif mark == b"-":
                    old -= 1
                elif mark == b"+":
                    new -= 1
                elif mark != b"\\":
                    raise ValueError(f"line {index + 1}: not a hunk line")
                index += 1
            if old or new:
                raise ValueError("a hunk shorter than its header says")
            in_header = False
            continue
        if SECTION.match(line.rstrip(b"\n")):
            starts.append(index)
            in_header = True
        elif line.startswith(b"--- ") and index + 1 < len(lines) and lines[index + 1].startswith(b"+++ "):
            if not in_header:
                starts.append(index)
            in_header = True
        index += 1
    if not starts:
        raise ValueError("no file section")
    return [lines[start:end] for start, end in zip(starts, starts[1:] + [len(lines)])]


def paths(section):
    found = set()
    head = SECTION.match(section[0].rstrip(b"\n"))
    if head:
        found.update(head.groups())
    for line in section:
        if HUNK.match(line):
            break
        if line.startswith((b"rename from ", b"rename to ", b"copy from ", b"copy to ")):
            found.add(line.split(b" ", 2)[2].strip())
        if line.startswith((b"GIT binary patch", b"Binary files ")):
            raise ValueError("a binary file")
        for pattern in (PLAIN_OLD, PLAIN_NEW):
            match = pattern.match(line)
            if match and match.group(1) != b"/dev/null":
                found.add(match.group(1))
    if not found:
        raise ValueError("a section with no path")
    return sorted(path.decode() for path in found)


def split(data, rules):
    parts = sections(data)
    out = {"drivers": [], "rest": [], "dropped": []}
    for section in parts:
        sides = {rules.side(path) for path in paths(section)}
        if len(sides) != 1:
            raise ValueError(f"a section that crosses a boundary: {paths(section)}")
        out[sides.pop()].append(section)
    assert sorted(map(b"".join, parts)) == sorted(b"".join(section) for half in out.values() for section in half)
    return out


def kind(out):
    if out["drivers"] and out["rest"]:
        return "mixed"
    if out["drivers"]:
        return "drivers"
    return "rest" if out["rest"] else "dropped"


def main(argv):
    file = RULES
    if argv[:1] == ["--rules"] and len(argv) >= 2:
        file, argv = argv[1], argv[2:]
    if argv[:1] in ([], ["-h"], ["--help"]):
        print(USAGE, file=sys.stderr)
        return 0 if argv else 2
    try:
        rules = Rules(file)
    except (KeyError, OSError, ValueError) as error:
        print(f"{file}: {error!r}", file=sys.stderr)
        return 2
    if len(argv) in (4, 5) and argv[0] == "split":
        renamed = argv[1] == "--renamed"
        argv = [argv[0]] + argv[2:] if renamed else argv
        if len(argv) != 4:
            print(USAGE, file=sys.stderr)
            return 2
        try:
            out = split(Path(argv[1]).read_bytes(), rules)
        except ValueError as error:
            print(f"{argv[1]}: {error}", file=sys.stderr)
            return 1
        rest = [rules.rename(section) for section in out["rest"]] if renamed else out["rest"]
        Path(argv[2]).write_bytes(b"".join(line for section in out["drivers"] for line in section))
        Path(argv[3]).write_bytes(b"".join(line for section in rest for line in section))
        print(kind(out), len(out["dropped"]))
        return 0
    if len(argv) >= 2 and argv[0] == "report":
        totals = {"drivers": 0, "rest": 0, "mixed": 0, "dropped": 0}
        dropped = 0
        failed = False
        for name in argv[1:]:
            data = Path(name).read_bytes()
            try:
                out = split(data, rules)
            except ValueError as error:
                print(f"{name}: {error}", file=sys.stderr)
                failed = True
                continue
            totals[kind(out)] += 1
            dropped += len(out["dropped"])
            digest = hashlib.sha256(data).hexdigest()[:12]
            print(f"{kind(out):7} {len(out['drivers']):3} {len(out['rest']):3} {len(out['dropped']):3}  "
                  f"{Path(name).name}  sha256 {digest}")
        print(f"{sum(totals.values())} patches: {totals['drivers']} drivers only, {totals['rest']} rest only, "
              f"{totals['mixed']} mixed, {totals['dropped']} dropped whole; {dropped} sections dropped "
              f"(columns: class, sections in the drivers part, in the rest part, dropped)")
        return 1 if failed else 0
    if len(argv) == 2 and argv[0] == "filter" and argv[1] in ("drivers", "rest", "dropped"):
        for line in sys.stdin:
            if rules.side(line.rstrip("\n").rpartition("\t")[2]) == argv[1]:
                sys.stdout.write(line)
        return 0
    if argv == ["layout"]:
        for line in sys.stdin:
            head, tab, path = line.rstrip("\n").rpartition("\t")
            if rules.side(path) != "rest":
                continue
            for old, new in rules.renamed.items():
                if path.startswith(old):
                    path = new + path[len(old):]
            sys.stdout.write(f"{head}{tab}{path}\n")
        return 0
    if len(argv) == 2 and argv[0] == "prefixes" and argv[1] in ("kept", "dropped"):
        for prefix in getattr(rules, argv[1]):
            print(prefix.rstrip("/"))
        return 0
    if len(argv) == 2 and argv[0] == "subject":
        print(rules.subjects.get(argv[1], ""))
        return 0
    print(USAGE, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
