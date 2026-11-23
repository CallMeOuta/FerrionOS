#!/usr/bin/env python3
import argparse
import datetime
import hashlib
import json
import re
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_RECIPE = ROOT / "recipes/wip/web/firefox/recipe.toml"
EVIDENCE_DIR = ROOT / "build/browser-pin"

PRODUCT_DETAILS_VERSIONS_URL = "https://product-details.mozilla.org/1.0/firefox_versions.json"
PRODUCT_DETAILS_HISTORY_URL = "https://product-details.mozilla.org/1.0/firefox_history_stability_releases.json"
MOZILLA_GIT_REPO = "https://github.com/mozilla-firefox/firefox.git"
MOZILLA_RAW_BASE = "https://raw.githubusercontent.com/mozilla-firefox/firefox"

USER_AGENT = "ferrionos-browser-pin"
MAX_URLS = 8
URL_TIMEOUT_S = 20
MAX_RESPONSE_BYTES = 1024 * 1024
MAX_RETRIES = 2
MAX_ESR_BRANCHES = 16
MAX_HEADS_SCANNED = 512

VERSION_RE = re.compile(r"^\d+\.\d+(\.\d+)?(esr)?$")
TAR_VERSION_RE = re.compile(r"^firefox-(\d+\.\d+(?:\.\d+)?(?:esr)?)\.source\.tar\.(?:xz|bz2|gz)$")
ESR_KEY_RE = re.compile(r"^FIREFOX_ESR(\d+|_NEXT)?$")
ESR_BRANCH_RE = re.compile(r"^refs/heads/esr(\d+)$")

EXIT_GREEN = 0
EXIT_RED = 1
EXIT_UNKNOWN = 2


class NetworkError(Exception):
    pass


class CheckFailure(Exception):
    pass


def parse_version_tuple(version):
    assert VERSION_RE.match(version), f"invalid version {version!r}"
    is_esr = version.endswith("esr")
    core = version[:-3] if is_esr else version
    parts = [int(p) for p in core.split(".")]
    while len(parts) < 3:
        parts.append(0)
    return (parts[0], parts[1], parts[2], is_esr)


class BoundedFetcher:
    def __init__(self):
        self.records = []

    def fetch(self, url):
        if len(self.records) >= MAX_URLS:
            raise CheckFailure(f"URL budget exceeded (max {MAX_URLS})")
        last_err = None
        for attempt in range(MAX_RETRIES + 1):
            req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            try:
                with urllib.request.urlopen(req, timeout=URL_TIMEOUT_S) as resp:
                    status = getattr(resp, "status", 200)
                    raw = resp.read(MAX_RESPONSE_BYTES + 1)
                    if len(raw) > MAX_RESPONSE_BYTES:
                        raise CheckFailure(f"response from {url} exceeded {MAX_RESPONSE_BYTES} bytes")
                    sha256 = hashlib.sha256(raw).hexdigest()
                    self.records.append({
                        "url": url,
                        "status": status,
                        "bytes": len(raw),
                        "sha256": sha256,
                        "attempts": attempt + 1,
                    })
                    return status, raw
            except urllib.error.HTTPError as exc:
                last_err = f"HTTP {exc.code} for {url}"
                if exc.code in (400, 404, 410):
                    self.records.append({
                        "url": url,
                        "status": exc.code,
                        "bytes": 0,
                        "sha256": None,
                        "attempts": attempt + 1,
                        "error": last_err,
                    })
                    raise CheckFailure(last_err)
            except (urllib.error.URLError, TimeoutError, OSError) as exc:
                last_err = f"{type(exc).__name__}: {exc} for {url}"
            if attempt < MAX_RETRIES:
                time.sleep(0.5 * (attempt + 1))
        self.records.append({
            "url": url,
            "status": None,
            "bytes": 0,
            "sha256": None,
            "attempts": MAX_RETRIES + 1,
            "error": last_err,
        })
        raise NetworkError(last_err or f"failed to fetch {url}")


def github_raw_url(git_url, rev, rel_path):
    parsed = urllib.parse.urlparse(git_url)
    if parsed.scheme != "https" or parsed.netloc != "github.com":
        raise CheckFailure(f"unparseable: unsupported git host in {git_url!r}")
    parts = [p for p in parsed.path.strip("/").split("/") if p]
    if len(parts) != 2:
        raise CheckFailure(f"unparseable: unexpected git path in {git_url!r}")
    owner, repo = parts[0], parts[1].removesuffix(".git")
    return f"https://raw.githubusercontent.com/{owner}/{repo}/{rev}/{rel_path}"


def read_pinned_version(recipe_path, fetcher):
    try:
        data = tomllib.loads(recipe_path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise CheckFailure(f"unparseable: cannot read {recipe_path}: {exc}")
    source = data.get("source")
    if not isinstance(source, dict):
        raise CheckFailure("unparseable: missing [source] table")
    if "git" in source:
        git_url = source.get("git")
        rev = source.get("rev")
        if not isinstance(git_url, str) or not isinstance(rev, str) or not re.fullmatch(r"[0-9a-fA-F]{7,40}", rev):
            raise CheckFailure("unparseable: git source requires a hex rev")
        raw_url = github_raw_url(git_url, rev, "browser/config/version.txt")
        _, body = fetcher.fetch(raw_url)
        version = body.decode("utf-8", errors="replace").strip()
        if not VERSION_RE.match(version):
            raise CheckFailure(f"unparseable: version {version!r} from {raw_url}")
        return {
            "source_kind": "git",
            "git": git_url,
            "branch": source.get("branch"),
            "rev": rev,
            "version": version,
        }
    if "tar" in source:
        tar_url = source.get("tar")
        blake3 = source.get("blake3")
        if not isinstance(tar_url, str) or not isinstance(blake3, str) or not blake3.strip():
            raise CheckFailure("unparseable: tar source requires tar URL and blake3")
        filename = Path(urllib.parse.urlparse(tar_url).path).name
        match = TAR_VERSION_RE.match(filename)
        if not match:
            raise CheckFailure(f"unparseable: tar filename {filename!r}")
        version = match.group(1)
        if not VERSION_RE.match(version):
            raise CheckFailure(f"unparseable: tar version {version!r}")
        return {
            "source_kind": "tar",
            "tar": tar_url,
            "blake3": blake3,
            "version": version,
        }
    raise CheckFailure("unparseable: [source] has neither git nor tar")


def read_product_details(fetcher):
    _, raw_versions = fetcher.fetch(PRODUCT_DETAILS_VERSIONS_URL)
    try:
        versions = json.loads(raw_versions.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise CheckFailure(f"unknown product-details JSON: {exc}")
    if not isinstance(versions, dict):
        raise CheckFailure("unknown product-details shape: root is not an object")
    latest_release = versions.get("LATEST_FIREFOX_VERSION")
    if not isinstance(latest_release, str) or not VERSION_RE.match(latest_release) or latest_release.endswith("esr"):
        raise CheckFailure(f"unknown product-details shape: LATEST_FIREFOX_VERSION={latest_release!r}")
    esr_lines = {}
    for key, value in sorted(versions.items()):
        if not key.startswith("FIREFOX_ESR"):
            continue
        if not ESR_KEY_RE.match(key):
            raise CheckFailure(f"unknown product-details ESR key {key!r}")
        if not isinstance(value, str):
            raise CheckFailure(f"unknown product-details value for {key!r}: {value!r}")
        if not value:
            continue
        if not VERSION_RE.match(value) or not value.endswith("esr"):
            raise CheckFailure(f"unknown product-details ESR version {key}={value!r}")
        major = parse_version_tuple(value)[0]
        line = f"esr{major}"
        if line in esr_lines and esr_lines[line] != value:
            raise CheckFailure(f"sources disagree inside product-details for {line}: {esr_lines[line]} vs {value}")
        esr_lines[line] = value
    if not esr_lines:
        raise CheckFailure("unknown product-details shape: no active FIREFOX_ESR version")
    _, raw_history = fetcher.fetch(PRODUCT_DETAILS_HISTORY_URL)
    try:
        history = json.loads(raw_history.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise CheckFailure(f"unknown history JSON: {exc}")
    if not isinstance(history, dict):
        raise CheckFailure("unknown history shape: root is not an object")
    return latest_release, esr_lines, history


def cross_check_mozilla_git(latest_release, esr_lines, fetcher):
    cmd = [
        "git", "ls-remote", "--heads", MOZILLA_GIT_REPO,
        "refs/heads/release", "refs/heads/esr*",
    ]
    last_err = None
    output = None
    for attempt in range(MAX_RETRIES + 1):
        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, timeout=URL_TIMEOUT_S)
            if proc.returncode == 0:
                output = proc.stdout
                break
            last_err = proc.stderr.strip() or f"git ls-remote exited {proc.returncode}"
        except (OSError, subprocess.SubprocessError) as exc:
            last_err = str(exc)
        if attempt < MAX_RETRIES:
            time.sleep(0.5 * (attempt + 1))
    if output is None:
        raise NetworkError(f"git ls-remote failed: {last_err}")

    esr_branches = []
    has_release_branch = False
    scanned = 0
    for line in output.splitlines():
        scanned += 1
        if scanned > MAX_HEADS_SCANNED:
            break
        parts = line.split("\t")
        if len(parts) != 2:
            continue
        ref = parts[1].strip()
        if ref == "refs/heads/release":
            has_release_branch = True
            continue
        m_br = ESR_BRANCH_RE.match(ref)
        if m_br:
            if len(esr_branches) < MAX_ESR_BRANCHES:
                esr_branches.append(f"esr{m_br.group(1)}")

    if not has_release_branch:
        raise CheckFailure("sources disagree: Mozilla git missing refs/heads/release")
    rel_branch_url = f"{MOZILLA_RAW_BASE}/refs/heads/release/browser/config/version.txt"
    _, rel_body = fetcher.fetch(rel_branch_url)
    rel_vtxt = rel_body.decode("utf-8", errors="replace").strip()
    if not VERSION_RE.match(rel_vtxt) or rel_vtxt.endswith("esr"):
        raise CheckFailure(f"sources disagree: unparseable release branch version.txt={rel_vtxt!r}")
    rel_pd_tuple = parse_version_tuple(latest_release)[:3]
    rel_br_tuple = parse_version_tuple(rel_vtxt)[:3]
    if rel_pd_tuple > rel_br_tuple:
        raise CheckFailure(
            f"sources disagree: product-details release={latest_release} exceeds branch version.txt={rel_vtxt}"
        )
    if rel_br_tuple[0] - rel_pd_tuple[0] > 1:
        raise CheckFailure(
            f"sources disagree: product-details release={latest_release} far behind branch version.txt={rel_vtxt}"
        )

    git_verified = {
        "release_branch": has_release_branch,
        "release_branch_version_txt": rel_vtxt,
        "esr_branches": sorted(esr_branches, key=lambda s: int(s[3:])),
        "esr_branch_versions": {},
    }
    for line_name, pd_ver in sorted(esr_lines.items(), key=lambda kv: int(kv[0][3:]), reverse=True):
        if line_name not in esr_branches:
            raise CheckFailure(f"sources disagree: product-details lists {line_name} ({pd_ver}) missing from git heads")
        esr_url = f"{MOZILLA_RAW_BASE}/refs/heads/{line_name}/browser/config/version.txt"
        _, esr_body = fetcher.fetch(esr_url)
        esr_vtxt = esr_body.decode("utf-8", errors="replace").strip()
        if not VERSION_RE.match(esr_vtxt):
            raise CheckFailure(f"sources disagree: unparseable {line_name} branch version.txt={esr_vtxt!r}")
        pd_tuple = parse_version_tuple(pd_ver)[:3]
        br_tuple = parse_version_tuple(esr_vtxt)[:3]
        if br_tuple[0] != pd_tuple[0]:
            raise CheckFailure(
                f"sources disagree: product-details {line_name}={pd_ver} major != branch version.txt={esr_vtxt}"
            )
        if pd_tuple > br_tuple:
            raise CheckFailure(
                f"sources disagree: product-details {line_name}={pd_ver} exceeds branch version.txt={esr_vtxt}"
            )
        git_verified["esr_branch_versions"][line_name] = esr_vtxt
    return git_verified


def release_age(version, history, today):
    date_str = history.get(version) or history.get(version.removesuffix("esr"))
    if not isinstance(date_str, str):
        return None, "date unknown"
    try:
        rel_date = datetime.date.fromisoformat(date_str)
    except ValueError:
        return None, "date unknown"
    days = (today - rel_date).days
    return date_str, f"{days} days ago ({date_str})"


def write_evidence(payload):
    EVIDENCE_DIR.mkdir(parents=True, exist_ok=True)
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out_path = EVIDENCE_DIR / f"{stamp}.json"
    out_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return out_path


def main():
    parser = argparse.ArgumentParser(description="Check Firefox recipe pin against Mozilla upstream releases.")
    parser.add_argument("--recipe", type=Path, default=DEFAULT_RECIPE)
    args = parser.parse_args()

    fetcher = BoundedFetcher()
    today = datetime.date.today()
    payload = {
        "timestamp_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "recipe": str(args.recipe),
        "bounds": {
            "max_urls": MAX_URLS,
            "url_timeout_s": URL_TIMEOUT_S,
            "max_response_bytes": MAX_RESPONSE_BYTES,
            "max_retries": MAX_RETRIES,
            "max_esr_branches": MAX_ESR_BRANCHES,
            "user_agent": USER_AGENT,
        },
        "fetches": fetcher.records,
    }
    try:
        pin_info = read_pinned_version(args.recipe, fetcher)
        payload["pin"] = pin_info
        latest_release, esr_lines, history = read_product_details(fetcher)
        git_verified = cross_check_mozilla_git(latest_release, esr_lines, fetcher)
        payload["upstream"] = {
            "latest_release": latest_release,
            "supported_esr_lines": esr_lines,
            "git_cross_check": git_verified,
        }
    except NetworkError as exc:
        payload["verdict"] = "unknown"
        payload["reason"] = str(exc)
        ev = write_evidence(payload)
        print(f"UNKNOWN: {exc} (evidence: {ev})")
        return EXIT_UNKNOWN
    except CheckFailure as exc:
        payload["verdict"] = "red"
        payload["reason"] = str(exc)
        ev = write_evidence(payload)
        print(f"RED: {exc} (evidence: {ev})")
        return EXIT_RED

    pin_ver = pin_info["version"]
    pin_major, _, _, pin_is_esr = parse_version_tuple(pin_ver)
    rel_major = parse_version_tuple(latest_release)[0]
    newest_esr_line = max(esr_lines.keys(), key=lambda s: int(s[3:]))
    newest_esr_ver = esr_lines[newest_esr_line]

    if pin_is_esr:
        pin_line = f"esr{pin_major}"
        line_updated = pin_line in esr_lines
        newest_on_line = esr_lines.get(pin_line)
    else:
        pin_line = f"release ({pin_major})"
        line_updated = pin_major == rel_major
        newest_on_line = latest_release if line_updated else None

    rel_date, rel_age = release_age(latest_release, history, today)
    esr_date, esr_age = release_age(newest_esr_ver, history, today)
    line_date, line_age = (release_age(newest_on_line, history, today) if newest_on_line else (None, "date unknown"))

    is_green = bool(line_updated and newest_on_line == pin_ver)
    verdict = "green" if is_green else "red"
    payload["verdict"] = verdict
    payload["result"] = {
        "pin_version": pin_ver,
        "pin_line": pin_line,
        "line_still_updated": line_updated,
        "newest_on_pin_line": newest_on_line,
        "newest_on_pin_line_date": line_date,
        "newest_on_pin_line_age": line_age,
        "newest_release": latest_release,
        "newest_release_date": rel_date,
        "newest_release_age": rel_age,
        "newest_esr_line": newest_esr_line,
        "newest_esr": newest_esr_ver,
        "newest_esr_date": esr_date,
        "newest_esr_age": esr_age,
    }
    ev = write_evidence(payload)
    line_newest_text = f"{newest_on_line} ({line_age})" if newest_on_line else "none (line EOL)"
    summary = (
        f"pin {pin_ver} on line {pin_line}, line updated: {'yes' if line_updated else 'no'}, "
        f"newest on pin line: {line_newest_text}, "
        f"release at {latest_release} ({rel_age}), "
        f"newest ESR {newest_esr_ver} ({esr_age})"
    )
    print(f"{verdict.upper()}: {summary} (evidence: {ev})")
    return EXIT_GREEN if is_green else EXIT_RED


if __name__ == "__main__":
    sys.exit(main())
