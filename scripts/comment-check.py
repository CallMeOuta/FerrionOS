#!/usr/bin/env python3
"""Fail when a tracked file has more comment lines than scripts/comment-check.baseline.json allows.

Run from the repository root: python3 -I scripts/comment-check.py
The baseline's counts only go down: --update lowers them after comments are deleted.
Required comments (AGENTS.md, Rules) are exempt through scripts/comment-check.config.json.
From the delete-comment skill of the maintainer's agents repository.
"""
import argparse
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from comment_extract import inventory, read_config

HERE = Path(__file__).resolve().parent


def counts_for(records, files):
    counts = {path: 0 for path in files}
    lines = {path: set() for path in files}
    for rec in records:
        if not rec["exempt"]:
            lines[rec["file"]].update(range(rec["line"], rec["end_line"] + 1))
    counts.update({path: len(numbers) for path, numbers in lines.items()})
    return counts


def check(root, config, baseline_path, update=False, initialize=False):
    records, _, files = inventory(root, config)
    counts = counts_for(records, files)
    baseline_path = Path(baseline_path)
    if initialize:
        if baseline_path.exists():
            raise ValueError("baseline already exists; use --update to lower it")
        baseline = counts
    else:
        baseline = json.loads(baseline_path.read_text())
        if not isinstance(baseline, dict) or any(type(v) is not int or v < 0 for v in baseline.values()):
            raise ValueError("baseline must be a path-to-nonnegative-integer object")
    offenders = {path for path, count in counts.items() if count > baseline.get(path, 0)}
    for rec in records:
        if rec["file"] in offenders and not rec["exempt"]:
            for offset, line in enumerate(rec["text"].splitlines()):
                print(f"{rec['file']}:{rec['line'] + offset}: {line.strip()}")
    if offenders:
        return 1
    if update or initialize:
        baseline_path.write_text(json.dumps(counts, sort_keys=True, indent=2) + "\n")
    return 0


def main():
    parser = argparse.ArgumentParser(description="Reject increases in non-required comment lines per tracked file.")
    parser.add_argument("root", nargs="?", default=".")
    parser.add_argument("--config", default=str(HERE / "comment-check.config.json"))
    parser.add_argument("--baseline", default=str(HERE / "comment-check.baseline.json"))
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--update", action="store_true")
    mode.add_argument("--init", action="store_true", help="create the first reviewed baseline; refuses to overwrite")
    args = parser.parse_args()
    try:
        return check(args.root, read_config(args.config), args.baseline, args.update, args.init)
    except (ValueError, OSError) as error:
        parser.exit(1, f"check: {error}\n")


if __name__ == "__main__":
    sys.exit(main())
