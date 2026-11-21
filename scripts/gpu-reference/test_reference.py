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
GUEST_SPEC = importlib.util.spec_from_file_location("guest", Path(__file__).resolve().with_name("guest.py"))
guest = importlib.util.module_from_spec(GUEST_SPEC)
GUEST_SPEC.loader.exec_module(guest)


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

    def passing_run(self):
        scene = {"hash": reference.SCENE_HASH, "differing_pixels": 0, "triangle_pixels": 17540}
        return {"device": "venus", "status": "PASS",
                "guest": {"venus_capset": {"bytes_hex": "00", "sha256": "hash"},
                          "gl": [{"name": "zink-venus", "status": "PASS", "scene": scene}]}}

    def test_repeatability_checks_capset_and_pixels(self):
        import copy
        one = self.passing_run()
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

    def test_identical_failures_do_not_count_as_repeatable(self):
        failed = {"device": "venus", "status": "FAIL", "guest": {"gl": [{"name": "virgl", "status": "FAIL"}]}}
        self.assertEqual(reference.repeatability([failed, failed])["venus"]["status"], "FAIL")
        wrong = self.passing_run()
        wrong["guest"]["gl"][0]["scene"]["hash"] = "0" * 16
        self.assertEqual(reference.repeatability([wrong, wrong])["venus"]["status"], "FAIL")

    def test_blocked_never_hides_a_failure(self):
        failed = {"device": "venus", "status": "FAIL", "error": "crash"}
        blocked = {"device": "venus", "status": "BLOCKED", "error": "eglInitialize failed"}
        self.assertEqual(reference.repeatability([failed, blocked])["venus"]["status"], "FAIL")
        self.assertEqual(reference.repeatability([blocked, blocked])["venus"]["status"], "BLOCKED")

    def test_unavailable_scenes_are_compared_but_not_required(self):
        import copy
        one = self.passing_run()
        one["guest"]["gl"].append({"name": "intel-native", "status": "UNAVAILABLE"})
        self.assertEqual(reference.repeatability([one, copy.deepcopy(one)])["venus"]["status"], "PASS")

    def test_assembled_cache_manifest_must_name_exactly_the_boot_files(self):
        with tempfile.TemporaryDirectory(prefix="ferrion-gpu-reference-") as folder:
            build = Path(folder)
            names = ("vmlinuz", "rootfs.cpio.gz")
            for name in names:
                (build / name).write_bytes(name.encode())
            good = {name: reference.sha256(build / name) for name in names}
            reference.check_assembled(build, good, names)
            for manifest in ({}, {"vmlinuz": good["vmlinuz"]}, {**good, "../escape": "0"}, []):
                with self.subTest(manifest=manifest), self.assertRaisesRegex(RuntimeError, "unexpected manifest"):
                    reference.check_assembled(build, manifest, names)
            (build / "rootfs.cpio.gz").write_bytes(b"changed")
            with self.assertRaisesRegex(RuntimeError, "fails SHA-256"):
                reference.check_assembled(build, good, names)

    def test_gl_test_checks_every_renderer_the_probes_saw(self):
        def outputs(versions):
            scene = (f'GLTRI renderer="zink Vulkan 1.4(Virtio-GPU Venus)" hash={reference.SCENE_HASH} '
                     'tri=17540 mismatches=0 centers_on_edge=0')
            def run(command, env=None, timeout=35):
                stdout = versions if command[-1] == "gl" else scene
                return {"command": command, "exit_code": 0, "stdout": stdout, "stderr": ""}
            return run

        cases = [("gl\t4.6\tzink Vulkan 1.4\ngles\tOpenGL ES 3.2\tzink Vulkan 1.4\n", "PASS"),
                 ("gl\t4.5\tllvmpipe (LLVM 22)\ngles\tOpenGL ES 3.2\tzink Vulkan 1.4\n", "FAIL"),
                 ("gl\t(null)\t(null)\ngles\tOpenGL ES 3.2\tzink Vulkan 1.4\n", "FAIL")]
        for versions, expected in cases:
            with self.subTest(versions=versions), patch.object(guest, "run", side_effect=outputs(versions)):
                self.assertEqual(guest.gl_test("zink-venus", {})["status"], expected)

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
