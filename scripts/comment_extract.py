#!/usr/bin/env python3
"""List every comment in a git repository as JSON lines.

Each record: file, line, end_line (1-based, inclusive), lang, kind, text, exempt, spans, id.
kind is "whole" (the comment fills its lines) or "trailing" (code before it on the line).
Consecutive whole-line comments at the same indent are one record.
exempt names why a comment must stay (shebang, license, directive), or is null.
A required pattern in the config is {"path", "text", "code", "reason"}: regexes on the file,
the comment's text, and the code it documents (the next line, or a trailing comment's own line).

From the delete-comment skill of the maintainer's agents repository.
"""
import io
import hashlib
from pathlib import Path
import json
import os
import re
import subprocess
import sys
import tokenize

DIRECTIVE = re.compile(
    r"shellcheck\s+(disable|enable|source|shell)|\bnoqa\b|type:\s*ignore|pylint:|\bfmt:\s*(off|on|skip)|"
    r"eslint-|@ts-|prettier-ignore|biome-ignore|NOLINT|rustfmt::|clippy::|-\*-.*coding|\bvim?:\s|"
    r"^\s*(//|#)\s*go:|(^|\n)\s*//\s*\+build\b|istanbul ignore|c8 ignore|#\s*region|#\s*endregion|syntax\s*=\s*docker|"
    r"escape\s*=|cython:|mypy:|pyright:|ruff:|isort:|deno-|jshint|jscs:|@license|@preserve|SPDX-License"
)
LICENSE = re.compile(r"copyright\s+(\(c\)|©|\d{4})|\(c\)\s*\d{4}|©|licensed under|permission is hereby granted|"
                     r"spdx-license|gnu general public license|mit license|apache license|in the public domain", re.I)
ORIGIN = re.compile(r"\b(?:copied|adapted|derived|borrowed|ported)(?: unchanged)? from\b[^\n]*"
                    r"(https?://|\b[0-9a-f]{12,}\b|\bv?\d+\.\d+)", re.I)

HASH_SIMPLE = {".gitignore", ".ignore", ".editorconfig", ".gitattributes", ".dockerignore"}


def lang_of(path, head):
    base = os.path.basename(path)
    ext = os.path.splitext(base)[1].lower()
    if base in ("Makefile", "GNUmakefile") or ext == ".mk":
        return "make"
    if ext == ".py":
        return "python"
    if ext in (".sh", ".bash", ".zsh", ".ion"):
        return "sh"
    if ext == ".fish":
        return "hashline"
    if ext == ".toml":
        return "toml"
    if ext in (".yml", ".yaml"):
        return "yaml"
    if ext == ".nix":
        return "nix"
    if ext == ".rs":
        return "rust"
    if ext in (".c", ".h", ".cc", ".cpp", ".hpp", ".cxx"):
        return "c"
    if ext in (".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"):
        return "js"
    if ext == ".go":
        return "go"
    if ext in (".css", ".scss"):
        return "css"
    if base in HASH_SIMPLE or ext in (".cfg", ".conf", ".ini", ".service", ".ipxe", ".desktop") or "containerfile" in base.lower() or base.startswith("Dockerfile"):
        return "hashline"
    if head.startswith("#!"):
        first = head.split("\n", 1)[0]
        if "python" in first:
            return "python"
        if re.search(r"\b(ba|z|da|k)?sh\b", first):
            return "sh"
        return "hashline"
    return None


class Out:
    def __init__(self, path, lang, src):
        self.path, self.lang, self.src, self.items = path, lang, src, []
        self.starts = line_starts(src)
        self.cursor = 0

    def add(self, line, end, text, whole, lang=None):
        start = self.src.find(text, max(self.starts[line - 1], self.cursor))
        if start < 0 or pos_to_line(self.starts, start) != line:
            raise ValueError(f"{self.path}:{line}: cannot locate comment")
        self.cursor = start + len(text)
        self.items.append({"file": self.path, "line": line, "end_line": end,
                           "lang": lang or self.lang,
                           "kind": "whole" if whole else "trailing", "text": text,
                           "spans": [[start, self.cursor]]})


def line_starts(src):
    starts = [0]
    for i, ch in enumerate(src):
        if ch == "\n":
            starts.append(i + 1)
    return starts


def pos_to_line(starts, pos):
    lo, hi = 0, len(starts) - 1
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if starts[mid] <= pos:
            lo = mid
        else:
            hi = mid - 1
    return lo + 1


def whole_line(src, starts, start, end):
    line = pos_to_line(starts, start)
    before = src[starts[line - 1]:start]
    nl = src.find("\n", end)
    after = src[end:nl if nl >= 0 else len(src)]
    return before.strip() == "" and after.strip() == ""


def emit(out, src, starts, start, end, lang=None, offset=0):
    text = src[start:end]
    line = pos_to_line(starts, start) + offset
    endl = pos_to_line(starts, max(start, end - 1)) + offset
    out.add(line, endl, text, whole_line(src, starts, start, end), lang)


HEREDOC = re.compile(r"<<(-?)\s*(\\?)(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\3")


def scan_shell(src, out, offset=0, lang=None):
    starts = line_starts(src)
    i, n = 0, len(src)
    pending = []
    while i < n:
        ch = src[i]
        if ch == "\n":
            i += 1
            while pending:
                strip_tabs, word = pending.pop(0)
                while i < n:
                    nl = src.find("\n", i)
                    nl = n if nl < 0 else nl
                    body = src[i:nl]
                    i = nl + 1
                    if (body.lstrip("\t") if strip_tabs else body) == word:
                        break
            continue
        if ch == "\\":
            i += 2
            continue
        if ch == "'":
            j = src.find("'", i + 1)
            i = n if j < 0 else j + 1
            continue
        if ch == "$" and i + 1 < n and src[i + 1] == "'":
            j = i + 2
            while j < n and src[j] != "'":
                j += 2 if src[j] == "\\" else 1
            i = j + 1
            continue
        if ch == '"' or ch == "`":
            q = ch
            j = i + 1
            while j < n and src[j] != q:
                j += 2 if src[j] == "\\" else 1
            i = j + 1
            continue
        if ch == "<" and src.startswith("<<", i) and not src.startswith("<<<", i):
            m = HEREDOC.match(src, i)
            if m:
                pending.append((m.group(1) == "-", m.group(4)))
                i = m.end()
                continue
        if ch == "#":
            prev = src[i - 1] if i > 0 else "\n"
            if prev in " \t\n;&|()<>" and not (prev == "(" and i >= 2 and src[i - 2] == "$"):
                j = src.find("\n", i)
                j = n if j < 0 else j
                emit(out, src, starts, i, j, lang, offset)
                i = j
                continue
        i += 1


def scan_make(src, out):
    lines = src.split("\n")
    in_define = 0
    shell_buf, shell_start = [], None

    def flush():
        nonlocal shell_buf, shell_start
        if shell_buf:
            text = "\n".join(l[1:] if l.startswith("\t") else l for l in shell_buf)
            scan_shell(text, out, offset=shell_start - 1, lang="sh-in-make")
        shell_buf, shell_start = [], None

    cont = False
    for no, raw in enumerate(lines, 1):
        stripped = raw.strip()
        if in_define:
            if re.match(r"\s*endef\b", raw):
                in_define -= 1
            elif re.match(r"\s*define\b", raw):
                in_define += 1
            continue
        if raw.startswith("\t") and not cont:
            if shell_start is None:
                shell_start = no
            shell_buf.append(raw)
            cont = raw.endswith("\\")
            continue
        if shell_buf and cont:
            shell_buf.append(raw)
            cont = raw.endswith("\\")
            continue
        flush()
        cont = raw.endswith("\\")
        if re.match(r"\s*define\b", raw):
            in_define = 1
            continue
        j = 0
        while j < len(raw):
            if raw[j] == "\\":
                j += 2
                continue
            if raw[j] == "#":
                out.add(no, no, raw[j:], raw[:j].strip() == "")
                break
            j += 1
    flush()


def scan_toml(src, out):
    starts = line_starts(src)
    i, n = 0, len(src)
    while i < n:
        ch = src[i]
        if ch == "#":
            j = src.find("\n", i)
            j = n if j < 0 else j
            emit(out, src, starts, i, j)
            i = j
            continue
        if src.startswith('"""', i) or src.startswith("'''", i):
            q = src[i:i + 3]
            line_start = starts[pos_to_line(starts, i) - 1]
            key = re.match(r"\s*([A-Za-z0-9_.\-\"']+)\s*=", src[line_start:i])
            j = i + 3
            while j < n and not src.startswith(q, j):
                j += 2 if (q == '"""' and src[j] == "\\") else 1
            while src.startswith(q[0], j + 3):
                j += 1
            body_start = i + 3
            if src.startswith("\n", body_start):
                body_start += 1
            if key and key.group(1).strip("\"\'").split(".")[-1] == "script":
                body = src[body_start:j]
                scan_shell(body, out, offset=pos_to_line(starts, body_start) - 1, lang="sh-in-toml")
            i = j + 3
            continue
        if ch == '"' or ch == "'":
            j = i + 1
            while j < n and src[j] != ch and src[j] != "\n":
                j += 2 if (ch == '"' and src[j] == "\\") else 1
            i = j + 1
            continue
        i += 1


def scan_yaml(src, out):
    lines = src.split("\n")
    block_indent = None
    block_shell, block_start, block_lines = False, 0, []

    def flush():
        nonlocal block_lines
        if block_shell and block_lines:
            scan_shell("\n".join(block_lines), out, offset=block_start - 1, lang="sh-in-yaml")
        block_lines = []

    for no, raw in enumerate(lines, 1):
        indent = len(raw) - len(raw.lstrip(" "))
        if block_indent is not None:
            if raw.strip() == "" or indent > block_indent:
                block_lines.append(raw)
                continue
            flush()
            block_indent = None
        j, quote = 0, None
        while j < len(raw):
            c = raw[j]
            if quote:
                if c == quote:
                    quote = None
                elif c == "\\" and quote == '"':
                    j += 1
            elif c in "\"'" and (j == 0 or raw[j - 1] in " :[{,-"):
                quote = c
            elif c == "#" and (j == 0 or raw[j - 1] in " \t"):
                out.add(no, no, raw[j:], raw[:j].strip() == "")
                break
            j += 1
        code = raw.split(" #", 1)[0].rstrip()
        m = re.search(r"^(\s*)(-\s+)?([A-Za-z0-9_\-]+)?\s*:?\s*[|>][-+0-9]*$", code)
        if m and (code.endswith(("|", ">", "|-", ">-", "|+", ">+")) or re.search(r"[|>][-+]?\d$", code)):
            block_indent = indent + len(m.group(2) or "")
            block_shell = (m.group(3) or "") in ("run", "script", "command", "shell")
            block_start = no + 1
            block_lines = []
    flush()


REGEX_PREV = set("(,=:[!&|?{};+-*%<>~^")
REGEX_WORDS = {"return", "typeof", "case", "do", "else", "in", "of", "new", "delete", "void", "throw", "yield", "await"}


def scan_clike(src, out, lang):
    starts = line_starts(src)
    i, n = 0, len(src)
    nested = lang == "rust"
    tmpl = []
    last_sig = ""
    last_word = ""
    parens = []
    while i < n:
        ch = src[i]
        if ch == "/" and src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            emit(out, src, starts, i, j)
            i = j
            continue
        if ch == "/" and src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("*/", j):
                    depth -= 1
                    j += 2
                elif nested and src.startswith("/*", j):
                    depth += 1
                    j += 2
                else:
                    j += 1
            emit(out, src, starts, i, j)
            i = j
            continue
        if lang == "rust" and ch in "rb" and re.match(r"(br|r)#*\"", src[i:i + 40]) and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            m = re.match(r"(br|r)(#*)\"", src[i:])
            close = '"' + m.group(2)
            j = src.find(close, i + m.end())
            i = n if j < 0 else j + len(close)
            last_sig = '"'
            continue
        if ch == "'" and lang == "rust":
            m = re.match(r"'(\\(x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'", src[i:])
            if m:
                i += m.end()
                last_sig = "'"
                continue
            i += 1
            continue
        if ch == '"' or (ch == "'" and lang != "rust") or (ch == "`" and lang in ("js", "go")):
            q = ch
            j = i + 1
            while j < n:
                c = src[j]
                if c == "\\" and not (lang == "go" and q == "`"):
                    j += 2
                    continue
                if c == q:
                    break
                if q == "`" and lang == "js" and src.startswith("${", j):
                    break
                j += 1
            if q == "`" and lang == "js" and j < n and src.startswith("${", j):
                tmpl.append(0)
                i = j + 2
                last_sig = "{"
                continue
            i = j + 1
            last_sig = q
            continue
        if lang == "js" and tmpl:
            if ch == "{":
                tmpl[-1] += 1
            elif ch == "}":
                if tmpl[-1] == 0:
                    tmpl.pop()
                    j = i + 1
                    while j < n:
                        c = src[j]
                        if c == "\\":
                            j += 2
                            continue
                        if c == "`":
                            break
                        if src.startswith("${", j):
                            break
                        j += 1
                    if j < n and src.startswith("${", j):
                        tmpl.append(0)
                        i = j + 2
                        continue
                    i = j + 1
                    last_sig = "`"
                    continue
                tmpl[-1] -= 1
        if lang == "js" and ch == "(":
            parens.append(last_word in {"if", "while", "for", "with", "switch", "catch"})
        elif lang == "js" and ch == ")" and parens:
            if parens.pop():
                last_sig, last_word = "=", ""
                i += 1
                continue
        if lang == "js" and ch == "/" and (last_sig == "" or last_sig in REGEX_PREV or last_word in REGEX_WORDS):
            j, in_class = i + 1, False
            while j < n and src[j] != "\n":
                c = src[j]
                if c == "\\":
                    j += 2
                    continue
                if c == "[":
                    in_class = True
                elif c == "]":
                    in_class = False
                elif c == "/" and not in_class:
                    break
                j += 1
            if j < n and src[j] == "/":
                j += 1
                while j < n and src[j].isalpha():
                    j += 1
                i = j
                last_sig, last_word = "/", ""
                continue
        if ch.isalnum() or ch == "_" or ch == "$":
            j = i
            while j < n and (src[j].isalnum() or src[j] in "_$"):
                j += 1
            last_word = src[i:j]
            last_sig = "a"
            i = j
            continue
        if not ch.isspace():
            last_sig, last_word = ch, ""
        i += 1


def scan_nix(src, out):
    starts = line_starts(src)
    i, n = 0, len(src)
    while i < n:
        ch = src[i]
        if ch == "#":
            j = src.find("\n", i)
            j = n if j < 0 else j
            emit(out, src, starts, i, j)
            i = j
            continue
        if src.startswith("/*", i):
            j = src.find("*/", i + 2)
            j = n if j < 0 else j + 2
            emit(out, src, starts, i, j)
            i = j
            continue
        if src.startswith("''", i):
            j = i + 2
            while j < n:
                if src.startswith("'''", j) or src.startswith("''$", j) or src.startswith("''\\", j):
                    j += 3
                    continue
                if src.startswith("''", j):
                    break
                j += 1
            i = j + 2
            continue
        if ch == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            i = j + 1
            continue
        i += 1


def scan_hashline(src, out):
    for no, raw in enumerate(src.split("\n"), 1):
        s = raw.lstrip()
        if (s.startswith("#") or (out.path.endswith((".ini", ".cfg", ".conf", ".desktop")) and s.startswith(";"))):
            out.add(no, no, s, True)


def scan_python(src, out):
    try:
        toks = list(tokenize.generate_tokens(io.StringIO(src).readline))
    except (tokenize.TokenError, IndentationError, SyntaxError):
        raise ValueError(f"{out.path}: Python tokenization failed")
    lines = src.split("\n")
    for t in toks:
        if t.type == tokenize.COMMENT:
            (row, col), (_, ecol) = t.start, t.end
            out.add(row, row, t.string, lines[row - 1][:col].strip() == "")


def group(items):
    """Join consecutive whole-line comments at one indent into one record."""
    result = []
    for it in sorted(items, key=lambda r: (r["line"], r["kind"] != "whole")):
        prev = result[-1] if result else None
        if (prev and it["kind"] == "whole" and prev["kind"] == "whole" and prev["lang"] == it["lang"]
                and prev["end_line"] + 1 == it["line"] and prev.get("_indent") == it.get("_indent")
                and prev.get("exempt") == it.get("exempt")
                and "\n" not in it["text"] and "\n" not in prev["text"].split("\n")[-1]):
            prev["end_line"] = it["end_line"]
            prev["text"] += "\n" + it["text"]
            prev["spans"].extend(it["spans"])
        else:
            result.append(it)
    return result


def documented_line(rec, lines):
    """The code a comment documents: the line after a whole-line comment, or a trailing comment's own line."""
    if lines is None:
        return None
    number = rec["end_line"] + 1 if rec["kind"] == "whole" else rec["line"]
    return lines[number - 1] if 0 < number <= len(lines) else ""


def exempt(rec, config=None, lines=None):
    t = rec["text"]
    if rec["line"] == 1 and t.startswith("#!"):
        return "shebang"
    if DIRECTIVE.search(t):
        return "directive"
    if LICENSE.search(t) or ORIGIN.search(t):
        return "license/origin"
    for rule in (config or {}).get("required_patterns", []):
        if isinstance(rule, str):
            rule = {"text": rule}
        if not re.search(rule.get("path", ".*"), rec["file"]) or not re.search(rule.get("text", ""), t):
            continue
        if "code" in rule:
            code = documented_line(rec, lines)
            if code is None or not re.search(rule["code"], code):
                continue
        return rule.get("reason", "repository-required")
    return None


def scan_source(src, path, config=None):
    lang = lang_of(path, src[:200])
    if not lang:
        return []
    out = Out(path, lang, src)
    {"python": scan_python, "sh": scan_shell, "make": scan_make, "toml": scan_toml, "yaml": scan_yaml,
     "nix": scan_nix, "hashline": scan_hashline}.get(lang, lambda s, o: scan_clike(s, o, lang))(src, out)
    lines = src.split("\n")
    for it in out.items:
        ln = lines[it["line"] - 1]
        it["_indent"] = len(ln) - len(ln.lstrip())
        reason = exempt(it)
        it["exempt"] = None if reason == "license/origin" else reason
    recs = group(out.items)
    for r in recs:
        r.pop("_indent", None)
        r["exempt"] = exempt(r, config, lines)
        identity = json.dumps([path, r["line"], r["end_line"], r["spans"], r["text"]], ensure_ascii=False)
        r["id"] = hashlib.sha256(identity.encode()).hexdigest()[:20]
    return recs


def safe_path(root, path):
    base = Path(root).resolve()
    candidate = base / path
    if Path(path).is_absolute() or ".." in Path(path).parts:
        raise ValueError(f"unsafe repository path: {path}")
    current = base
    for part in Path(path).parts:
        current = current / part
        if current.is_symlink():
            raise ValueError(f"symlink is outside sweep scope: {path}")
    return candidate


def scan_file(root, path, config=None):
    full = safe_path(root, path)
    data = full.read_bytes()
    if b"\0" in data[:8192]:
        return []
    try:
        src = data.decode("utf-8")
    except UnicodeDecodeError:
        if lang_of(path, ""):
            raise ValueError(f"{path}: source is not UTF-8")
        return []
    return scan_source(src, path, config)


def read_config(path):
    config = json.loads(Path(path).read_text()) if path else {}
    for pattern in config.get("skip", []):
        re.compile(pattern)
    for pattern in config.get("required_patterns", []):
        if isinstance(pattern, str):
            re.compile(pattern)
        else:
            re.compile(pattern.get("path", ".*"))
            re.compile(pattern.get("text", ""))
            re.compile(pattern.get("code", ""))
            if "text" not in pattern and "code" not in pattern:
                raise ValueError("a required pattern needs text or code")
    return config


def inventory(root, config=None, exclude=()):
    config = config or {}
    files = subprocess.run(["git", "-C", str(root), "ls-files", "-z"], capture_output=True, check=True).stdout.decode().split("\0")
    skip = [re.compile(x) for x in list(exclude) + config.get("skip", []) +
            [r"\.patch$", r"\.(md|json|jsonl|lock|svg|png|jpg|txt|csv|diff)$", r"(^|/)Cargo\.lock$"]]
    recs, total_lines, scanned = [], 0, []
    for f in sorted(set(files)):
        if not f or any(s.search(f) for s in skip):
            continue
        full = Path(root) / f
        if full.is_symlink() or not full.is_file():
            continue
        data = full.read_bytes()
        if not lang_of(f, data[:200].decode("utf-8", errors="replace")):
            continue
        scanned.append(f)
        total_lines += len(data.splitlines())
        recs.extend(scan_file(root, f, config))
    return recs, total_lines, scanned


def summary(recs, total_lines):
    import collections
    whole = [r for r in recs if r["kind"] == "whole"]
    by_lang = collections.Counter()
    for r in whole:
        if not r["exempt"]:
            by_lang[r["lang"]] += r["end_line"] - r["line"] + 1
    return {"comments": len(recs), "whole": len(whole), "trailing": len(recs) - len(whole),
            "deletable_whole_lines": sum(by_lang.values()),
            "all_whole_lines": sum(r["end_line"] - r["line"] + 1 for r in whole),
            "lines_in_scanned_files": total_lines,
            "exempt": collections.Counter(r["exempt"] for r in recs if r["exempt"]),
            "lines_by_lang": by_lang.most_common()}


def main():
    import argparse
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("root")
    p.add_argument("--config")
    p.add_argument("--exclude", action="append", default=[], help="regex of paths to skip")
    p.add_argument("--summary", action="store_true")
    a = p.parse_args()
    try:
        recs, total_lines, _ = inventory(a.root, read_config(a.config), a.exclude)
        if a.summary:
            print(json.dumps(summary(recs, total_lines), indent=2))
        else:
            for r in recs:
                print(json.dumps(r, ensure_ascii=False))
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        p.exit(1, f"extract: {error}\n")


if __name__ == "__main__":
    main()
