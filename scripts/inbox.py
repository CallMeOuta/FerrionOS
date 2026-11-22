#!/usr/bin/env python3
import argparse
from dataclasses import dataclass
import datetime
import enum
import fcntl
import json
import os
from pathlib import Path
import re
import shlex
import stat
import subprocess
import sys
import tempfile
import termios
import tty

MAX_ENTRIES = 20000
MAX_DEPTH = 32
MAX_BYTES = 1024 * 1024
MAX_DECISIONS = 1000
MAX_ACTIONS = 10000
MAX_OPTIONS = 9
EDITOR_TIMEOUT = 3600
STATUS = re.compile(r"^Status: ([a-z-]+)[ \t]*$", re.MULTILINE)
HEADING = re.compile(r"^## .+$", re.MULTILINE)
NOTE = re.compile(r"^(?:- )?question to the maintainer: (.+)$")


class IssueStatus(str, enum.Enum):
    READY_FOR_AGENT = "ready-for-agent"
    READY_FOR_HUMAN = "ready-for-human"
    NEEDS_INFO = "needs-info"
    WONTFIX = "wontfix"


class DecisionKind(str, enum.Enum):
    ISSUE = "issue"
    NOTE = "note"


PENDING = {IssueStatus.READY_FOR_HUMAN.value, IssueStatus.NEEDS_INFO.value}


@dataclass(frozen=True)
class Option:
    number: int
    text: str
    recommended: bool
    status: IssueStatus

    def to_dict(self):
        return {
            "number": self.number,
            "text": self.text,
            "recommended": self.recommended,
            "status": self.status.value,
        }


@dataclass(frozen=True)
class Answerable:
    options: tuple[Option, ...]
    recommended: int


@dataclass(frozen=True)
class Malformed:
    error: str


@dataclass(frozen=True)
class Decision:
    path: str
    line: int
    kind: DecisionKind
    created: str
    question: str
    context: str
    body: Answerable | Malformed
    source_text: str

    @property
    def key(self):
        return (self.path, self.kind, self.question, self.created)

    def to_dict(self):
        if isinstance(self.body, Answerable):
            options = [option.to_dict() for option in self.body.options]
            recommended = self.body.recommended
            error = None
        else:
            options = []
            recommended = None
            error = self.body.error
        return {
            "path": self.path,
            "line": self.line,
            "kind": self.kind.value,
            "created": self.created,
            "question": self.question,
            "context": self.context,
            "options": options,
            "recommended": recommended,
            "error": error,
        }


def section(text, name):
    matches = list(HEADING.finditer(text))
    for index, match in enumerate(matches):
        if match.group() == "## " + name:
            end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
            return match.end(), end
    return None


def read_file(path):
    with path.open("rb") as stream:
        data = stream.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        raise ValueError(f"{path}: exceeds {MAX_BYTES} bytes")
    return data.decode("utf-8")


def issue_paths(root):
    scratch = root / ".scratch"
    if not scratch.exists():
        return
    pending = [(scratch, 0)]
    count = 0
    while pending:
        directory, depth = pending.pop()
        if directory.is_symlink():
            raise ValueError(f"{directory}: symbolic links are not supported")
        with os.scandir(directory) as entries:
            for entry in entries:
                count += 1
                if count > MAX_ENTRIES:
                    raise ValueError(f".scratch exceeds {MAX_ENTRIES} entries")
                if entry.is_dir(follow_symlinks=False):
                    if depth >= MAX_DEPTH:
                        raise ValueError(f".scratch exceeds depth {MAX_DEPTH}")
                    pending.append((Path(entry.path), depth + 1))
                elif directory.name == "issues" and entry.name.endswith(".md"):
                    if not entry.is_file(follow_symlinks=False):
                        raise ValueError(f"{entry.path}: expected a regular file")
                    yield Path(entry.path)


def decision_fields(body):
    question = re.findall(r"^Question: (.+)$", body, re.MULTILINE)
    context = re.findall(r"^Context: (.+)$", body, re.MULTILINE)
    rows = re.findall(r"^([1-9])\. (.+)$", body, re.MULTILINE)
    if len(question) != 1 or len(context) != 1 or not 2 <= len(rows) <= MAX_OPTIONS:
        raise ValueError("Use one Question:, one Context: paragraph and 2–9 numbered options")
    options = []
    for index, (number, text) in enumerate(rows, 1):
        if int(number) != index:
            raise ValueError("Number options consecutively from 1")
        status = IssueStatus.WONTFIX if re.search(r"\bwontfix\b", text, re.I) else IssueStatus.READY_FOR_AGENT
        options.append(Option(number=index, text=text, recommended="[recommended]" in text.lower(), status=status))
    recommended = [option.number for option in options if option.recommended]
    if len(recommended) != 1:
        raise ValueError("Mark exactly one option [recommended]")
    return question[0], context[0], Answerable(options=tuple(options), recommended=recommended[0])


def make_item(root, path, text, body, kind, line, created_body):
    created = re.search(r"^Created: (.+)$", created_body, re.MULTILINE)
    timestamp = path.stat().st_mtime
    if created:
        value = datetime.datetime.fromisoformat(created[1].strip().replace("Z", "+00:00"))
        timestamp = value.replace(tzinfo=value.tzinfo or datetime.timezone.utc).timestamp()
    created_iso = datetime.datetime.fromtimestamp(timestamp, datetime.timezone.utc).isoformat()
    try:
        question, context, parsed = decision_fields(body)
    except ValueError as error:
        question = "Decision needs options"
        context = body.strip()
        parsed = Malformed(error=str(error))
    return Decision(
        path=str(path.relative_to(root)),
        line=line,
        kind=kind,
        created=created_iso,
        question=question,
        context=context,
        body=parsed,
        source_text=text,
    )


def gather(root):
    queue = []
    for path in issue_paths(root):
        text = read_file(path)
        header = text.split("\n## ", 1)[0]
        statuses = STATUS.findall(header)
        if not any(value in PENDING for value in statuses):
            continue
        span = section(text, "Decision")
        if span is None:
            continue
        if len(statuses) != 1:
            raise ValueError(f"{path}: expected one Status: in the header")
        queue.append(make_item(root, path, text, text[slice(*span)], DecisionKind.ISSUE, 1, header))
        if len(queue) > MAX_DECISIONS:
            raise ValueError(f"Queue exceeds {MAX_DECISIONS} decisions")
    path = root / "build/LANES.md"
    if path.exists():
        if path.is_symlink() or path.parent.is_symlink():
            raise ValueError(f"{path}: symbolic links are not supported")
        text = read_file(path)
        span = section(text, "Notes")
        if span:
            lines = text.splitlines(keepends=True)
            offset = 0
            for index, line in enumerate(lines):
                match = NOTE.fullmatch(line.rstrip("\r\n")) if span[0] <= offset < span[1] else None
                offset += len(line)
                if not match:
                    continue
                continuation = []
                for following in lines[index + 1:]:
                    if not following.startswith("  "):
                        break
                    continuation.append(following[2:])
                body = "Question: " + match[1] + "\n" + "".join(continuation)
                queue.append(make_item(root, path, text, body, DecisionKind.NOTE, index + 1, body))
                if len(queue) > MAX_DECISIONS:
                    raise ValueError(f"Queue exceeds {MAX_DECISIONS} decisions")
    queue.sort(key=lambda item: (item.created, item.path, item.line))
    return queue


def append_comment(text, comment):
    span = section(text, "Comments")
    if span is None:
        return text.rstrip() + "\n\n## Comments\n\n" + comment + "\n"
    end = span[1]
    return text[:end].rstrip() + "\n\n" + comment + "\n\n" + text[end:]


def save_answer(root, item, option):
    assert isinstance(item.body, Answerable) and option in item.body.options
    path = root / item.path
    text = item.source_text
    if item.kind is DecisionKind.ISSUE:
        header, separator, rest = text.partition("\n## ")
        header, count = STATUS.subn("Status: " + option.status.value, header)
        assert count == 1
        updated = header + separator + rest
    else:
        lines = text.splitlines(keepends=True)
        index = item.line - 1
        assert NOTE.fullmatch(lines[index].rstrip("\r\n"))
        lines[index] = lines[index].replace("question to the maintainer:", "answered by the maintainer:", 1)
        lines.insert(index + 1, "  Status: " + option.status.value + "\n")
        updated = "".join(lines)
    date = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    comment = f"- {date}, maintainer: {item.question} Answer: {option.number}. {option.text} Status: {option.status.value}."
    updated = append_comment(updated, comment)
    encoded = updated.encode("utf-8")
    if len(encoded) > MAX_BYTES:
        raise ValueError(f"{path}: answer would exceed {MAX_BYTES} bytes")
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    temporary = None
    try:
        fcntl.flock(directory, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if path.is_symlink() or read_file(path) != text:
            raise ValueError(f"{path}: changed since display; no answer saved. Reopen the inbox")
        mode = stat.S_IMODE(path.stat().st_mode)
        with tempfile.NamedTemporaryFile(prefix=".inbox-", dir=path.parent, delete=False) as stream:
            temporary = stream.name
            os.fchmod(stream.fileno(), mode)
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        if read_file(path) != text:
            raise ValueError(f"{path}: changed while saving; no answer saved. Reopen the inbox")
        os.replace(temporary, path)
        temporary = None
        os.fsync(directory)
    finally:
        if temporary is not None:
            os.unlink(temporary)
        os.close(directory)


def safe_display(text):
    return "".join(char if char in "\n\t" or char.isprintable() else "?" for char in text)


def show(item, remaining):
    print(f"\n{remaining} remaining · {item.path}:{item.line}")
    print(safe_display(item.question) + "\n")
    print(safe_display(item.context) + "\n")
    if isinstance(item.body, Answerable):
        for option in item.body.options:
            marker = ">" if option.recommended else " "
            print(safe_display(f"{marker} {option.number}. {option.text}"))
    else:
        print("Cannot answer: " + item.body.error)
    print("Enter: recommendation · number: choose · s: skip · o: open · q: quit", flush=True)


def read_key():
    if not sys.stdin.isatty():
        line = sys.stdin.readline(MAX_BYTES + 1)
        if len(line) > MAX_BYTES:
            raise ValueError("Input exceeds maximum line length")
        return line.rstrip("\r\n") if line else "q"
    descriptor = sys.stdin.fileno()
    previous = termios.tcgetattr(descriptor)
    try:
        tty.setcbreak(descriptor)
        key = sys.stdin.read(1)
        return "q" if not key else "" if key in {"\r", "\n"} else key
    finally:
        termios.tcsetattr(descriptor, termios.TCSADRAIN, previous)


class Session:
    def __init__(self, root):
        self.root = root
        self.queue = gather(root)
        self.skipped = set()

    def refresh(self, clear_skips=False):
        self.queue = gather(self.root)
        if clear_skips:
            self.skipped.clear()
        else:
            present = {item.key for item in self.queue}
            self.skipped &= present
        assert self.skipped.issubset({item.key for item in self.queue})

    def next_decision(self):
        for item in self.queue:
            if item.key not in self.skipped:
                return item
        return None

    def apply(self, item, key):
        if key in {"q", "\x04"}:
            print(f"{len(self.queue)} remaining.")
            return False
        if key == "s":
            self.skipped.add(item.key)
            return True
        if key == "o":
            editor = shlex.split(os.environ.get("VISUAL") or os.environ.get("EDITOR") or "vi")
            subprocess.run(editor + [str(self.root / item.path)], check=True, timeout=EDITOR_TIMEOUT)
            self.refresh(clear_skips=True)
            return True
        option = None
        if isinstance(item.body, Answerable):
            number = item.body.recommended if key == "" else int(key) if re.fullmatch("[1-9]", key) else None
            option = next((value for value in item.body.options if value.number == number), None)
        if option is None:
            print("Choose an option, skip, open or quit.")
            return True
        save_answer(self.root, item, option)
        print(f"Saved. {len(self.queue) - 1} remaining.", flush=True)
        self.skipped.discard(item.key)
        self.refresh()
        return True


def run(root):
    session = Session(root)
    for _ in range(MAX_ACTIONS):
        item = session.next_decision()
        if item is None:
            print(f"{len(session.queue)} remaining" + (" (skipped this session)." if session.queue else ". Inbox clear."))
            return
        show(item, len(session.queue))
        if not session.apply(item, read_key()):
            return
    raise ValueError(f"Session exceeds {MAX_ACTIONS} actions; reopen the inbox")


def main():
    parser = argparse.ArgumentParser(description="Review one maintainer decision at a time.")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent,
                        help="Repository root (default: the script's repository)")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--list", action="store_true", help="Print the queue without changing files")
    mode.add_argument("--json", action="store_true", help="Print the queue as JSON without changing files")
    args = parser.parse_args()
    try:
        root = args.root.resolve(strict=True)
        if args.json or args.list:
            queue = gather(root)
            if args.json:
                print(json.dumps([item.to_dict() for item in queue], indent=2))
            else:
                for item in queue:
                    suffix = (
                        f" [recommended: {item.body.recommended}]"
                        if isinstance(item.body, Answerable)
                        else " [needs options]"
                    )
                    print(safe_display(f"{item.created} {item.path}:{item.line} {item.question}{suffix}"))
                print(f"{len(queue)} remaining.")
        else:
            run(root)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"Inbox stopped: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\nInbox closed.", file=sys.stderr)
        return 130
    return 0


if __name__ == "__main__":
    sys.exit(main())
