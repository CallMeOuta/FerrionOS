#!/usr/bin/env python3
import argparse
import datetime
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORK_ROOT = ROOT / "build/firefox-source-verify"
ARCHIVE_BASE = "https://archive.mozilla.org/pub/firefox/releases"
PINNED_PRIMARY_FPR = "14F26682D0916CDD81E37B6D61B7B526D98F0353"
PINNED_KEY_SOURCES = (
    "https://archive.mozilla.org/pub/firefox/releases/153.7.0esr/KEY",
    "https://blog.mozilla.org/security/2026/08/10/updated-gpg-key-for-signing-firefox-and-thunderbird-releases/",
)

USER_AGENT = "ferrionos-firefox-source-verify"
URL_TIMEOUT_S = 60
TARBALL_TIMEOUT_S = 600
GPG_TIMEOUT_S = 60
MAX_RETRIES = 2
KEY_MAX_BYTES = 1 * 1024 * 1024
SUMS_MAX_BYTES = 4 * 1024 * 1024
ASC_MAX_BYTES = 64 * 1024
TARBALL_MAX_BYTES = 2 * 1024 * 1024 * 1024
CHUNK_BYTES = 1024 * 1024

VERSION_RE = re.compile(r"^\d+\.\d+(\.\d+)?(esr)?$")
SHA512_RE = re.compile(r"^[0-9a-fA-F]{128}$")
BLAKE3_RE = re.compile(r"^[0-9a-f]{64}$")

EXIT_OK = 0
EXIT_ERROR = 1
EXIT_BAD_SIG = 3
EXIT_BAD_HASH = 4


class VerifyError(Exception):
    def __init__(self, message, exit_code=EXIT_ERROR):
        super().__init__(message)
        self.exit_code = exit_code


def download_bounded(url, dest_path, max_bytes, timeout_s):
    assert max_bytes > 0 and timeout_s > 0
    last_err = None
    for attempt in range(MAX_RETRIES + 1):
        req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
        try:
            with urllib.request.urlopen(req, timeout=timeout_s) as resp:
                status = getattr(resp, "status", 200)
                total = 0
                h_sha256 = hashlib.sha256()
                with dest_path.open("wb") as out_f:
                    while True:
                        chunk = resp.read(CHUNK_BYTES)
                        if not chunk:
                            break
                        total += len(chunk)
                        if total > max_bytes:
                            raise VerifyError(f"{url} exceeded {max_bytes} bytes")
                        out_f.write(chunk)
                        h_sha256.update(chunk)
                return {
                    "url": url,
                    "status": status,
                    "bytes": total,
                    "sha256": h_sha256.hexdigest(),
                    "attempts": attempt + 1,
                }
        except VerifyError:
            dest_path.unlink(missing_ok=True)
            raise
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            dest_path.unlink(missing_ok=True)
            last_err = f"{type(exc).__name__}: {exc} for {url}"
        if attempt < MAX_RETRIES:
            time.sleep(0.5 * (attempt + 1))
    raise VerifyError(last_err or f"failed to download {url}")


def verify_key_and_signature(key_path, sums_path, asc_path, gnupg_dir):
    gpg_bin = "/usr/bin/gpg"
    gpgv_bin = "/usr/bin/gpgv"
    if not Path(gpg_bin).is_file() or not Path(gpgv_bin).is_file():
        raise VerifyError("host /usr/bin/gpg and /usr/bin/gpgv are required")
    gnupg_dir.mkdir(parents=True, exist_ok=True)
    os.chmod(gnupg_dir, 0o700)
    env = dict(os.environ)
    env["GNUPGHOME"] = str(gnupg_dir)

    imp = subprocess.run(
        [gpg_bin, "--batch", "--no-tty", "--import", str(key_path)],
        env=env, capture_output=True, text=True, timeout=GPG_TIMEOUT_S,
    )
    if imp.returncode != 0:
        raise VerifyError(f"gpg import failed: {imp.stderr.strip()}", EXIT_BAD_SIG)

    listed = subprocess.run(
        [gpg_bin, "--batch", "--no-tty", "--list-keys", "--with-colons", "--with-fingerprint"],
        env=env, capture_output=True, text=True, timeout=GPG_TIMEOUT_S,
    )
    if listed.returncode != 0:
        raise VerifyError(f"gpg list-keys failed: {listed.stderr.strip()}", EXIT_BAD_SIG)

    primary_fprs = []
    expect_primary_fpr = False
    for line in listed.stdout.splitlines():
        fields = line.split(":")
        if not fields:
            continue
        if fields[0] == "pub":
            expect_primary_fpr = True
        elif fields[0] == "sub":
            expect_primary_fpr = False
        elif fields[0] == "fpr" and len(fields) > 9 and expect_primary_fpr:
            primary_fprs.append(fields[9].upper())
            expect_primary_fpr = False

    if primary_fprs != [PINNED_PRIMARY_FPR]:
        raise VerifyError(
            f"key primary fingerprints {primary_fprs} do not match pinned {PINNED_PRIMARY_FPR}",
            EXIT_BAD_SIG,
        )

    keyring_path = gnupg_dir / "trusted.gpg"
    exp = subprocess.run(
        [gpg_bin, "--batch", "--no-tty", "--export", "--output", str(keyring_path), PINNED_PRIMARY_FPR],
        env=env, capture_output=True, text=True, timeout=GPG_TIMEOUT_S,
    )
    if exp.returncode != 0 or not keyring_path.is_file():
        raise VerifyError(f"gpg export failed: {exp.stderr.strip()}", EXIT_BAD_SIG)

    ver = subprocess.run(
        [gpgv_bin, "--keyring", str(keyring_path), "--status-fd", "1", str(asc_path), str(sums_path)],
        env=env, capture_output=True, text=True, timeout=GPG_TIMEOUT_S,
    )
    goodsig = False
    validsig_primary = None
    validsig_signer = None
    for line in ver.stdout.splitlines():
        if line.startswith("[GNUPG:] GOODSIG "):
            goodsig = True
        elif line.startswith("[GNUPG:] VALIDSIG "):
            parts = line.split()
            if len(parts) >= 12:
                validsig_signer = parts[2].upper()
                validsig_primary = parts[11].upper()
    if ver.returncode != 0 or not goodsig or validsig_primary != PINNED_PRIMARY_FPR:
        raise VerifyError(
            f"gpgv rejected signature (rc={ver.returncode}, goodsig={goodsig}, "
            f"primary={validsig_primary}, signer={validsig_signer}): {ver.stderr.strip()}",
            EXIT_BAD_SIG,
        )
    return {
        "primary_fingerprint": validsig_primary,
        "signing_subkey_fingerprint": validsig_signer,
        "gpgv_exit": ver.returncode,
    }


def extract_expected_sha512(sums_path, rel_tarball):
    matches = []
    for raw_line in sums_path.read_text(encoding="utf-8", errors="replace").splitlines():
        parts = raw_line.strip().split()
        if len(parts) == 2 and parts[1] == rel_tarball:
            digest = parts[0].lower()
            if not SHA512_RE.match(digest):
                raise VerifyError(f"invalid SHA-512 digest for {rel_tarball}: {digest!r}")
            matches.append(digest)
    if len(matches) != 1:
        raise VerifyError(f"expected 1 SHA512SUMS entry for {rel_tarball}, found {len(matches)}")
    return matches[0]


def sha512_file(path):
    h = hashlib.sha512()
    total = 0
    with path.open("rb") as f:
        while True:
            chunk = f.read(CHUNK_BYTES)
            if not chunk:
                break
            total += len(chunk)
            if total > TARBALL_MAX_BYTES:
                raise VerifyError(f"{path} exceeded {TARBALL_MAX_BYTES} bytes")
            h.update(chunk)
    return h.hexdigest(), total


def build_blake3_helper():
    helper_bin = WORK_ROOT / "b3sum-helper"
    if helper_bin.is_file() and os.access(helper_bin, os.X_OK):
        return helper_bin
    deps_dir = ROOT / "target/release/deps"
    rlibs = sorted(deps_dir.glob("libblake3-*.rlib")) if deps_dir.is_dir() else []
    if not rlibs:
        return None
    WORK_ROOT.mkdir(parents=True, exist_ok=True)
    src = WORK_ROOT / "b3sum_helper.rs"
    src.write_text(
        "use std::io::Read;\n"
        "fn main() {\n"
        "    let path = std::env::args().nth(1).expect(\"path\");\n"
        "    let mut f = std::fs::File::open(&path).expect(\"open\");\n"
        "    let mut h = blake3::Hasher::new();\n"
        "    let mut buf = vec![0u8; 1024 * 1024];\n"
        "    loop {\n"
        "        let n = f.read(&mut buf).expect(\"read\");\n"
        "        if n == 0 { break; }\n"
        "        h.update(&buf[..n]);\n"
        "    }\n"
        "    println!(\"{}\", h.finalize().to_hex());\n"
        "}\n",
        encoding="utf-8",
    )
    proc = subprocess.run(
        [
            "rustc", "--edition=2021", "-O", str(src),
            "-L", f"dependency={deps_dir}",
            "--extern", f"blake3={rlibs[-1]}",
            "-o", str(helper_bin),
        ],
        capture_output=True, text=True, timeout=60,
    )
    src.unlink(missing_ok=True)
    if proc.returncode != 0:
        helper_bin.unlink(missing_ok=True)
        return None
    return helper_bin


def blake3_file(path):
    b3sum = shutil.which("b3sum")
    if b3sum:
        out = subprocess.check_output([b3sum, "--no-names", str(path)], text=True, timeout=TARBALL_TIMEOUT_S).strip()
        assert BLAKE3_RE.match(out), f"unexpected b3sum output {out!r}"
        return out
    try:
        import blake3 as blake3_mod
        h = blake3_mod.blake3()
        with path.open("rb") as f:
            while True:
                chunk = f.read(CHUNK_BYTES)
                if not chunk:
                    break
                h.update(chunk)
        return h.hexdigest()
    except ImportError:
        pass
    helper = build_blake3_helper()
    if helper:
        out = subprocess.check_output([str(helper), str(path)], text=True, timeout=TARBALL_TIMEOUT_S).strip()
        assert BLAKE3_RE.match(out), f"unexpected helper blake3 output {out!r}"
        return out
    raise VerifyError("no blake3 implementation available (need b3sum, python blake3, or target/release/deps/libblake3)")


def main():
    parser = argparse.ArgumentParser(description="Verify Mozilla Firefox release tarball GPG + SHA-512 chain and compute blake3.")
    parser.add_argument("version", help="Firefox version, e.g. 153.7.0esr")
    parser.add_argument("--verify-only", action="store_true", help="Verify KEY and SHA512SUMS signature without downloading the tarball")
    parser.add_argument("--tarball", type=Path, default=None, help="Verify an already-downloaded tarball instead of downloading it")
    parser.add_argument("--keep-tarball", action="store_true", help="Keep downloaded tarball under build/firefox-source-verify/")
    args = parser.parse_args()

    if not VERSION_RE.match(args.version):
        print(f"ERROR: invalid Firefox version {args.version!r}", file=sys.stderr)
        return EXIT_ERROR

    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_dir = WORK_ROOT / f"run-{args.version}-{stamp}-{os.getpid()}"
    gnupg_dir = run_dir / "gnupg"
    run_dir.mkdir(parents=True, exist_ok=True)

    rel_tarball = f"source/firefox-{args.version}.source.tar.xz"
    base_url = f"{ARCHIVE_BASE}/{args.version}"
    tarball_url = f"{base_url}/{rel_tarball}"
    report = {
        "timestamp_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "version": args.version,
        "pinned_primary_fingerprint": PINNED_PRIMARY_FPR,
        "pinned_key_sources": list(PINNED_KEY_SOURCES),
        "tarball_url": tarball_url,
        "bounds": {
            "url_timeout_s": URL_TIMEOUT_S,
            "tarball_timeout_s": TARBALL_TIMEOUT_S,
            "key_max_bytes": KEY_MAX_BYTES,
            "sums_max_bytes": SUMS_MAX_BYTES,
            "asc_max_bytes": ASC_MAX_BYTES,
            "tarball_max_bytes": TARBALL_MAX_BYTES,
            "max_retries": MAX_RETRIES,
        },
        "fetches": [],
    }
    try:
        key_path = run_dir / "KEY"
        sums_path = run_dir / "SHA512SUMS"
        asc_path = run_dir / "SHA512SUMS.asc"
        report["fetches"].append(download_bounded(f"{base_url}/KEY", key_path, KEY_MAX_BYTES, URL_TIMEOUT_S))
        report["fetches"].append(download_bounded(f"{base_url}/SHA512SUMS", sums_path, SUMS_MAX_BYTES, URL_TIMEOUT_S))
        report["fetches"].append(download_bounded(f"{base_url}/SHA512SUMS.asc", asc_path, ASC_MAX_BYTES, URL_TIMEOUT_S))

        sig_info = verify_key_and_signature(key_path, sums_path, asc_path, gnupg_dir)
        report["signature"] = sig_info
        expected_sha512 = extract_expected_sha512(sums_path, rel_tarball)
        report["expected_sha512"] = expected_sha512

        if args.verify_only:
            report["verdict"] = "verified_sums_only"
            ev_path = WORK_ROOT / f"{args.version}-{stamp}.json"
            ev_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
            print(
                f"OK (sums only): {args.version} primary={sig_info['primary_fingerprint']} "
                f"subkey={sig_info['signing_subkey_fingerprint']} sha512={expected_sha512} "
                f"(evidence: {ev_path})"
            )
            return EXIT_OK

        if args.tarball is not None:
            tarball_path = args.tarball
            if not tarball_path.is_file():
                raise VerifyError(f"tarball not found: {tarball_path}")
        else:
            tarball_path = WORK_ROOT / f"firefox-{args.version}.source.tar.xz"
            report["fetches"].append(download_bounded(tarball_url, tarball_path, TARBALL_MAX_BYTES, TARBALL_TIMEOUT_S))

        actual_sha512, tarball_bytes = sha512_file(tarball_path)
        report["actual_sha512"] = actual_sha512
        report["tarball_bytes"] = tarball_bytes
        if actual_sha512 != expected_sha512:
            if args.tarball is None:
                tarball_path.unlink(missing_ok=True)
            raise VerifyError(
                f"SHA-512 mismatch for {rel_tarball}: expected {expected_sha512}, got {actual_sha512}",
                EXIT_BAD_HASH,
            )

        b3 = blake3_file(tarball_path)
        report["blake3"] = b3
        report["verdict"] = "verified"
        if args.tarball is None and not args.keep_tarball:
            tarball_path.unlink(missing_ok=True)
        ev_path = WORK_ROOT / f"{args.version}-{stamp}.json"
        ev_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(
            f"OK: {args.version} bytes={tarball_bytes} primary={sig_info['primary_fingerprint']} "
            f"subkey={sig_info['signing_subkey_fingerprint']} sha512={actual_sha512} "
            f"blake3={b3} (evidence: {ev_path})"
        )
        return EXIT_OK
    except VerifyError as exc:
        report["verdict"] = "bad_signature" if exc.exit_code == EXIT_BAD_SIG else "error"
        report["error"] = str(exc)
        ev_path = WORK_ROOT / f"{args.version}-{stamp}.json"
        WORK_ROOT.mkdir(parents=True, exist_ok=True)
        ev_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(f"FAIL (exit {exc.exit_code}): {exc} (evidence: {ev_path})", file=sys.stderr)
        return exc.exit_code
    finally:
        shutil.rmtree(run_dir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
