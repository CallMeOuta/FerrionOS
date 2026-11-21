#!/usr/bin/env python3

import re
import subprocess
import sys

URL_LINE = re.compile(r"^\s*[a-z]+://\S+$")
RULES = [
    (None, lambda text: "\r" not in text, "the message contains CRLF line breaks; only LF is allowed"),
    (2, lambda line: line == "", "the line between the subject and the body is not empty"),
    (1, lambda line: not line.startswith("Merge branch "), "this is a merge commit; rebase instead"),
    (1, lambda line: re.match(r'^(Revert "|\S+: )', line), 'the subject has no category ("Category: Description")'),
    (1, lambda line: re.match(r"^\S.*?: [A-Z0-9]", line) or line.startswith('Revert "'),
     "the first word after the category is not capitalized"),
    (1, lambda line: not line.endswith("."), "the subject ends with a period"),
    (None, lambda text: all(len(line) <= 72 or URL_LINE.match(line) for line in text.split("\n")),
     "a line is longer than 72 characters (a URL may stand alone on a longer line)"),
    (None, lambda text: not re.search(r"^Signed-off-by: ", text, re.M), "the message contains a Signed-off-by line"),
]


def strip(message):
    """The message as git stores it: comment lines and trailing blank lines removed."""
    lines = [line for line in message.split("\n") if not line.startswith("#")]
    return "\n".join(lines).rstrip("\n")


def problems(message):
    text = strip(message)
    lines = text.split("\n")
    found = []
    for line_number, check, error in RULES:
        if line_number is None:
            ok = check(text)
        elif line_number <= len(lines):
            ok = check(lines[line_number - 1])
        else:
            ok = True
        if not ok:
            found.append(error)
    return found


def main():
    args = sys.argv[1:]
    if len(args) == 1 and not args[0].startswith("-"):
        with open(args[0], encoding="utf-8") as file:
            messages = {"this commit": file.read()}
    elif len(args) == 2 and args[0] == "--range":
        shas = subprocess.run(["git", "rev-list", "--no-merges", args[1]], capture_output=True, text=True,
                              check=True).stdout.split()
        messages = {sha[:9]: subprocess.run(["git", "log", "-1", "--format=%B", sha], capture_output=True,
                                            text=True, check=True).stdout for sha in shas}
    else:
        print("Checks commit messages against docs/code-policy.md.\n"
              "Usage: scripts/lint-commit-message.py <message-file> | --range <rev-range>", file=sys.stderr)
        sys.exit(2)
    failed = False
    for name, message in messages.items():
        for error in problems(message):
            failed = True
            print(f"{name}: {error}", file=sys.stderr)
    if failed:
        print("See docs/code-policy.md, section Commits.", file=sys.stderr)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
