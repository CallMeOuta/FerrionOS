#!/usr/bin/env python3
import datetime
import json
import os
from pathlib import Path
import resource
import selectors
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
COMMAND = [sys.executable, str(ROOT / "scripts/inbox.py")]
ISSUES = 100
MAX_COPY_FILES = 1000
TIMEOUT = 5
FIRST_DECISION_TARGET = 0.2
PROMPT = b"q: quit\n"


def invoke(root, actions="", flags=(), **kwargs):
    return subprocess.run(COMMAND + ["--root", str(root), *flags], input=actions,
                          text=True, capture_output=True, timeout=TIMEOUT, **kwargs)


def start(root):
    started = time.perf_counter()
    process = subprocess.Popen(COMMAND + ["--root", str(root)], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    output = bytearray()
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            for _ in range(1024):
                remaining = TIMEOUT - (time.perf_counter() - started)
                assert remaining > 0 and selector.select(remaining), "First decision timed out"
                block = os.read(process.stdout.fileno(), 65536)
                assert block, process.stderr.read().decode()
                output.extend(block)
                if PROMPT in output:
                    return process, time.perf_counter() - started, output
            raise AssertionError("First decision exceeded output budget")
    except BaseException:
        process.kill()
        process.communicate()
        raise


def refuse_file_writes():
    signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
    resource.setrlimit(resource.RLIMIT_FSIZE, (1, 1))


def main():
    with tempfile.TemporaryDirectory(prefix="agent-inbox-session-") as directory:
        root = Path(directory)
        sources = list((ROOT / ".scratch").glob("**/issues/*.md"))
        assert len(sources) <= MAX_COPY_FILES
        for source in sources:
            destination = root / source.relative_to(ROOT)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(source.read_bytes())
        fixture_dir = root / ".scratch/inbox-session/issues"
        fixture_dir.mkdir(parents=True)
        assert len(sources) <= ISSUES
        for index in range(ISSUES - len(sources)):
            (fixture_dir / f"{index:03d}.md").write_text(
                f"# Session decision {index}\n\nStatus: needs-info\nCreated: 2027-01-01\n\n"
                "## Decision\n\nQuestion: Which session policy?\n\n"
                "1. Keep the policy. [recommended]\n2. Change the policy.\n\n"
                "Context: This deterministic session compares persisted answers with chosen options.\n")
        (root / "build").mkdir()
        lanes = root / "build/LANES.md"
        lanes.write_text("# Lanes\n\n## Notes\n\n"
                         "- question to the maintainer: Should the session lane become default?\n"
                         "  Created: 2026-11-22\n"
                         "  Context: The session lane is optional.\n"
                         "  1. Keep it optional. [recommended]\n"
                         "  2. Make it default.\n\n## Other\n\nUnrelated text.\n")
        files = [root / source.relative_to(ROOT) for source in sources]
        files.extend(sorted(fixture_dir.glob("*.md")))
        files.append(lanes)
        original = {path: path.read_bytes() for path in files}
        listed = invoke(root, flags=("--json",))
        assert listed.returncode == 0, listed.stderr
        queue = json.loads(listed.stdout)
        assert queue[0]["path"].endswith("01-passwd-after-admin-reset.md")
        assert queue[1]["path"].endswith("02-accounts-added-later.md")
        assert queue[2]["kind"] == "note"
        assert invoke(root, flags=("--list",)).returncode == 0
        no_answer = invoke(root, "invalid\ns\no\nq\n", env={**os.environ, "EDITOR": "true", "VISUAL": "true"})
        assert no_answer.returncode == 0 and "Saved." not in no_answer.stdout
        assert invoke(root).returncode == 0
        assert all(path.read_bytes() == data for path, data in original.items())
        process, latency, displayed = start(root)
        try:
            first = root / queue[0]["path"]
            first.write_bytes(original[first] + b"\nConcurrent editor change.\n")
            output, errors = process.communicate(b"\n", timeout=TIMEOUT)
            assert process.returncode == 1 and b"changed since display" in errors
            assert b"Saved." not in output
            assert first.read_bytes() == original[first] + b"\nConcurrent editor change.\n"
            first.write_bytes(original[first])
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()
        refused = invoke(root, "\n", preexec_fn=refuse_file_writes)
        assert refused.returncode == 1 and "Saved." not in refused.stdout, refused
        assert all(path.read_bytes() == data for path, data in original.items())
        assert not list(first.parent.glob(".inbox-*"))
        process, latency, displayed = start(root)
        try:
            output, errors = process.communicate(b"\n3\n2\nq\n", timeout=TIMEOUT)
            transcript = (displayed + output).decode()
            assert process.returncode == 0, errors.decode()
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()
        assert transcript.count("Saved.") == 3, transcript
        date = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
        choices = [(queue[0], 1, "ready-for-agent"), (queue[1], 3, "wontfix"), (queue[2], 2, "ready-for-agent")]
        answered = set()
        for item, number, status in choices:
            path = root / item["path"]
            text = path.read_text()
            selected = item["options"][number - 1]["text"]
            comments = text.split("## Comments\n", 1)[1]
            assert f"- {date}, maintainer: {item['question']} Answer: {number}. {selected} Status: {status}." in comments
            if item["kind"] == "issue":
                assert f"Status: {status}\n" in text.split("## Decision", 1)[0]
            else:
                assert "question to the maintainer:" not in text
                assert "  Status: ready-for-agent\n" in text
                assert "## Other\n\nUnrelated text." in text
            answered.add(path)
        assert all(path.read_bytes() == data for path, data in original.items() if path not in answered)
        remaining = json.loads(invoke(root, flags=("--json",)).stdout)
        assert len(remaining) == len(queue) - 3
        assert f"{len(remaining)} remaining." in transcript
        print(json.dumps({"issues": ISSUES, "first_decision_seconds": round(latency, 6),
                          "target_seconds": FIRST_DECISION_TARGET, "answers_written": len(answered),
                          "remaining": len(remaining), "stale_write_refused": True,
                          "failed_write_refused": True, "non_answers_unchanged": True}, indent=2))
        assert latency < FIRST_DECISION_TARGET, f"First decision took {latency:.6f}s"
    with tempfile.TemporaryDirectory(prefix="agent-inbox-skip-") as skip_dir:
        skip_root = Path(skip_dir)
        (skip_root / "build").mkdir()
        (skip_root / "build/LANES.md").write_text(
            "# Lanes\n\n## Notes\n\n"
            "- question to the maintainer: Question A?\n"
            "  Created: 2026-11-21\n"
            "  Context: Context A.\n"
            "  1. Opt A1. [recommended]\n"
            "  2. Opt A2.\n"
            "- question to the maintainer: Question B?\n"
            "  Created: 2026-11-20\n"
            "  Context: Context B.\n"
            "  1. Opt B1. [recommended]\n"
            "  2. Opt B2.\n")
        skipped_run = invoke(skip_root, "s\n\nq\n")
        assert skipped_run.returncode == 0, skipped_run.stderr
        assert skipped_run.stdout.count("Question B?") == 1, skipped_run.stdout
        assert "1 remaining (skipped this session)." in skipped_run.stdout, skipped_run.stdout


if __name__ == "__main__":
    main()
