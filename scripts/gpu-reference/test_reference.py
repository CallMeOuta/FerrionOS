#!/usr/bin/env python3
# Host-only checks for the reference harness; no download, GPU, or guest needed.

import importlib.util
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("reference", Path(__file__).resolve().parents[1] / "gpu-reference-guest.py")
reference = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reference)


class ReferenceTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("bsdtar"), "bsdtar is needed to read RPM payloads")
    def test_cpio_hardlink_data_on_last_member_is_preserved(self):
        import stat
        with tempfile.TemporaryDirectory(prefix="ferrion-gpu-reference-") as folder:
            package = Path(folder) / "payload.cpio"
            with package.open("wb") as stream:
                for index, name in enumerate(("compiler", "compiler-link", "compiler-last")):
                    reference.cpio_entry(stream, 1, name, stat.S_IFREG | 0o755,
                                         b"payload" if index == 2 else b"", nlink=3)
                reference.cpio_entry(stream, 2, "TRAILER!!!", 0)
            with reference.rpm_tar(package) as (archive, read):
                self.assertEqual({member.name: read(member) for member in archive},
                                 {name: b"payload" for name in ("compiler", "compiler-link", "compiler-last")})

    def test_corrupted_cache_is_refused_without_network(self):
        with tempfile.TemporaryDirectory(prefix="ferrion-gpu-reference-") as folder:
            cache = Path(folder)
            (cache / "fixture.deb").write_bytes(b"corrupt")
            with patch.object(reference.urllib.request, "urlopen") as network:
                with self.assertRaisesRegex(RuntimeError, "Cached file fails SHA-256"):
                    reference.fetch(cache, "0" * 64, "pool/fixture.deb")
                network.assert_not_called()

    def test_wrong_download_hash_is_never_promoted(self):
        import io
        with tempfile.TemporaryDirectory(prefix="ferrion-gpu-reference-") as folder:
            cache = Path(folder)
            with patch.object(reference.urllib.request, "urlopen", return_value=io.BytesIO(b"corrupt")):
                with self.assertRaisesRegex(RuntimeError, "Download fails pinned SHA-256"):
                    reference.fetch(cache, "0" * 64, "pool/fixture.deb")
            self.assertEqual(list(cache.iterdir()), [])

    def test_host_floors_are_blocked(self):
        host = {"kernel": "6.12.99", "architecture": "x86_64",
                "device_help": {"stdout": "venus=<bool>", "stderr": ""},
                "display_help": {"stdout": "egl-headless"}}
        with self.assertRaisesRegex(reference.Blocked, "6.13"):
            reference.preflight(SimpleNamespace(), host)
        host["kernel"] = "6.13.0"
        host["device_help"]["stdout"] = "blob=<bool>"
        with self.assertRaisesRegex(reference.Blocked, "venus property"):
            reference.preflight(SimpleNamespace(), host)
        host["device_help"]["stdout"] = "venus=<bool>"
        host["display_help"]["stdout"] = "none\n"
        with self.assertRaisesRegex(reference.Blocked, "OpenGL"):
            reference.preflight(SimpleNamespace(), host)

    def test_repeatability_checks_capset_and_pixels(self):
        import copy
        one = {"device": "venus", "guest": {"venus_capset": {"bytes_hex": "00", "sha256": "hash"},
               "gl": [{"name": "zink-venus", "scene": {"hash": reference.SCENE_HASH, "differing_pixels": 0}}]}}
        two = copy.deepcopy(one)
        self.assertEqual(reference.repeatability([one, two])["venus"]["status"], "PASS")
        two["guest"]["venus_capset"]["bytes_hex"] = "01"
        self.assertEqual(reference.repeatability([one, two])["venus"]["status"], "FAIL")
        two = copy.deepcopy(one)
        two["guest"]["gl"][0]["scene"]["differing_pixels"] = 1
        self.assertEqual(reference.repeatability([one, two])["venus"]["status"], "FAIL")

    def test_missing_results_do_not_count_as_repeatable(self):
        result = {"device": "venus", "error": "no complete guest record"}
        self.assertEqual(reference.repeatability([result, result])["venus"]["status"], "FAIL")

    def test_blocked_host_writes_record_and_exits_two_without_downloads(self):
        import contextlib
        import io
        import json
        with tempfile.TemporaryDirectory(prefix="ferrion-gpu-reference-") as folder:
            args = ["gpu-reference-guest.py", "--variant", "all", "--vk-driver", "fixture.json", "--out", folder, "--cache", folder]
            with patch.object(sys, "argv", args), patch.object(reference, "host_info", return_value={"kernel": "6.12.0"}), \
                 patch.object(reference, "fetch") as download, contextlib.redirect_stdout(io.StringIO()), \
                 contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(reference.main(), 2)
                download.assert_not_called()
            record = json.loads(next(Path(folder).glob("run-*/result.json")).read_text())
            self.assertEqual(record["status"], "BLOCKED")
            self.assertEqual(len(record["variants"]), 2)
            self.assertTrue(all(v["status"] == "BLOCKED" for v in record["variants"]))


if __name__ == "__main__":
    unittest.main()
