#!/usr/bin/env python3
# Check VM graphics capture and host capability verdicts without booting an image.
import concurrent.futures
import contextlib
import io
import importlib.util
import json
import socket
import struct
import sys
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("vm_test", Path(__file__).with_name("vm-test.py"))
vm_test = importlib.util.module_from_spec(spec)
spec.loader.exec_module(vm_test)


class GraphicsTests(unittest.TestCase):
    def test_rfb_rgb_rectangles_resize_and_repeated_capture(self):
        client, server = socket.socketpair()
        client.settimeout(5)
        server.settimeout(5)

        def serve():
            def read(n):
                data = b""
                while len(data) < n:
                    part = server.recv(n - len(data))
                    self.assertTrue(part)
                    data += part
                return data

            def send(data):
                for offset in range(0, len(data), 7):
                    server.sendall(data[offset:offset + 7])

            send(b"RFB 003.008\n")
            self.assertEqual(read(12), b"RFB 003.008\n")
            send(b"\x01\x01")
            self.assertEqual(read(1), b"\x01")
            send(b"\0" * 4)
            self.assertEqual(read(1), b"\x01")
            send(struct.pack("!HH", 1, 1) + bytes(16) + struct.pack("!I", 4) + b"QEMU")
            pixel_format = read(20)
            self.assertEqual(pixel_format[4:8], bytes([32, 24, 1, 1]))
            self.assertEqual(read(12), struct.pack("!BBHii", 2, 0, 2, 0, -223))
            self.assertEqual(read(10), struct.pack("!BBHHHH", 3, 0, 0, 0, 1, 1))
            send(struct.pack("!BBH", 0, 0, 1) + struct.pack("!HHHHi", 0, 0, 2, 2, -223))
            for _ in range(2):
                self.assertEqual(read(10), struct.pack("!BBHHHH", 3, 0, 0, 0, 2, 2))
                send(b"\x02" + b"\x03\0\0\0" + struct.pack("!I", 4) + b"text")
                send(struct.pack("!BBH", 0, 0, 2)
                     + struct.pack("!HHHHi", 0, 0, 2, 1, 0) + b"\0\xff\0\0\0\0\xff\0"
                     + struct.pack("!HHHHi", 0, 1, 2, 1, 0) + b"\0\0\0\xff\0\x12\x34\x56")

        try:
            with concurrent.futures.ThreadPoolExecutor() as executor:
                future = executor.submit(serve)
                rfb = vm_test.RFB(client)
                for _ in range(2):
                    frame = rfb.frame()
                    self.assertEqual((frame.width, frame.height), (2, 2))
                    self.assertEqual(frame.rgb, b"\xff\0\0\0\xff\0\0\0\xff\x12\x34\x56")
                future.result(timeout=5)
        finally:
            client.close()
            server.close()

    def test_rfb_closed_connection_is_an_error(self):
        client, server = socket.socketpair()
        server.close()
        with client, self.assertRaisesRegex(RuntimeError, "closed VNC"):
            vm_test.RFB(client)

    def test_blocked_hosts_write_result_and_exit_two_without_starting_vm(self):
        cases = [
            ("6.12.9", "none\negl-headless", "hostmem=\nblob=\nvenus=", "Linux 6.13"),
            ("6.13.0", "none\ngtk", "hostmem=\nblob=\nvenus=", "egl-headless"),
            ("7.3.4", "none\negl-headless", "hostmem=\nblob=", "venus"),
        ]
        for kernel, displays, properties, expected in cases:
            with self.subTest(expected=expected), tempfile.TemporaryDirectory() as out:
                def output(command):
                    return displays if "-display" in command else properties
                argv = ["vm-test.py", "boot", "--make", "gpu=venus", "--out", out]
                with patch.object(sys, "argv", argv), \
                     patch.object(vm_test.sys, "platform", "linux"), \
                     patch.object(vm_test.os, "uname", return_value=SimpleNamespace(release=kernel)), \
                     patch.object(vm_test, "command_output", side_effect=output), \
                     patch.object(vm_test, "qemu_settings", return_value=(["qemu"], "venus", "", "")), \
                     patch.object(vm_test, "host_versions", return_value={"kernel": kernel}), \
                     patch.object(vm_test, "boot_render_node", return_value=""), \
                     patch.object(vm_test, "VM") as launch, self.assertRaises(SystemExit) as exit_status:
                    vm_test.main()
                self.assertEqual(exit_status.exception.code, 2)
                launch.assert_not_called()
                result = json.loads((Path(out) / "boot-1/result.json").read_text())
                self.assertEqual(result["verdict"], "BLOCKED")
                self.assertIn(expected, result["note"])
                self.assertEqual(result["host"]["kernel"], kernel)

    def test_make_cannot_start_a_build_or_replace_snapshot_flags(self):
        for assignment in ("image", "QEMUFLAGS=-display gtk", "BUILD=somewhere"):
            with self.subTest(assignment=assignment), \
                 patch.object(sys, "argv", ["vm-test.py", "boot", "--make", assignment]), \
                 patch.object(vm_test, "qemu_settings") as settings, \
                 contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as exit_status:
                vm_test.main()
            self.assertEqual(exit_status.exception.code, 2)
            settings.assert_not_called()

    def test_2d_does_not_require_a_3d_host(self):
        with patch.object(vm_test, "command_output", side_effect=AssertionError("3D probe on 2D")):
            vm_test.require_3d_host(["qemu"], "virtio", "x86_64")

    def serve_rfb(self, server, width, height, answer):
        """A VNC server for one connection: handshake, then `answer(read, send)` for the frame requests."""
        def read(n):
            data = b""
            while len(data) < n:
                part = server.recv(n - len(data))
                if not part:
                    raise ConnectionError("client closed")
                data += part
            return data

        server.sendall(b"RFB 003.008\n")
        read(12)
        server.sendall(b"\x01\x01")
        read(1)
        server.sendall(b"\0" * 4)
        read(1)
        server.sendall(struct.pack("!HH", width, height) + bytes(16) + struct.pack("!I", 4) + b"QEMU")
        read(20 + 12)
        try:
            answer(read, server.sendall)
        except (ConnectionError, OSError):
            pass

    def capture(self, width, height, answer, timeout=5):
        client, server = socket.socketpair()
        client.settimeout(5)
        server.settimeout(5)
        try:
            with concurrent.futures.ThreadPoolExecutor() as executor:
                future = executor.submit(self.serve_rfb, server, width, height, answer)
                try:
                    return vm_test.RFB(client).frame(timeout=timeout)
                finally:
                    client.close()
                    future.result(timeout=5)
        finally:
            server.close()

    def test_rfb_frame_waits_for_every_pixel_across_updates(self):
        requests = []

        def answer(read, send):
            requests.append(read(10))
            send(struct.pack("!BBH", 0, 0, 1) + struct.pack("!HHHHi", 0, 0, 2, 1, 0) + b"\0\xff\0\0\0\0\xff\0")
            requests.append(read(10))
            send(struct.pack("!BBH", 0, 0, 1) + struct.pack("!HHHHi", 0, 1, 2, 1, 0) + b"\0\0\0\xff\0\x12\x34\x56")

        frame = self.capture(2, 2, answer)
        self.assertEqual(frame.rgb, b"\xff\0\0\0\xff\0\0\0\xff\x12\x34\x56")
        self.assertEqual(len(requests), 2)

    def test_rfb_empty_rectangle_is_not_a_frame(self):
        def answer(read, send):
            read(10)
            send(struct.pack("!BBH", 0, 0, 1) + struct.pack("!HHHHi", 0, 0, 0, 0, 0))
            read(10)
            send(struct.pack("!BBH", 0, 0, 1) + struct.pack("!HHHHi", 0, 0, 1, 1, 0) + b"\0\x0a\x0b\x0c")

        self.assertEqual(self.capture(1, 1, answer).rgb, b"\x0a\x0b\x0c")

    def test_rfb_frame_has_an_overall_deadline(self):
        def answer(read, send):
            read(10)
            while True:
                send(b"\x02")  # Bell, forever, each well inside the per-read timeout.
                time.sleep(0.02)

        with self.assertRaisesRegex(RuntimeError, "deadline"):
            self.capture(1, 1, answer, timeout=0.3)

    def test_qemu_settings_ignore_lines_make_prints_itself(self):
        stdout = ("sccache not found, building without it\n"
                  f"{vm_test.SETTING}qemu-system-x86_64 -name 'a b'\n{vm_test.SETTING}venus\n"
                  f"{vm_test.SETTING}\n{vm_test.SETTING}/dev/dri/renderD128\n")
        run = SimpleNamespace(returncode=0, stdout=stdout, stderr="")
        args = SimpleNamespace(arch="x86_64", config="desktop", make=[])
        with patch.object(vm_test.subprocess, "run", return_value=run) as call:
            settings = vm_test.qemu_settings(args)
        self.assertEqual(settings, (["qemu-system-x86_64", "-name", "a b"], "venus", "", "/dev/dri/renderD128"))
        self.assertIn(vm_test.SETTING, " ".join(call.call_args.args[0]))

    def require(self, gpu, properties, kernel="7.3.4", qemu_runs=True, intel=False, honor=True):
        """require_3d_host on a pretend host whose QEMU answers with these device properties."""
        def output(command):
            if not qemu_runs:
                return None
            if "--version" in command:
                return "QEMU emulator version 11.1.3"
            return "none\negl-headless" if "-display" in command else properties

        with patch.object(vm_test.sys, "platform", "linux"), \
             patch.object(vm_test.os, "uname", return_value=SimpleNamespace(release=kernel)), \
             patch.object(vm_test, "command_output", side_effect=output), \
             patch.object(vm_test, "intel_host", return_value=intel), \
             patch.object(vm_test, "kvm_available", return_value=True), \
             patch.object(vm_test, "qemu_accepts", return_value=honor):
            vm_test.require_3d_host(["qemu"], gpu, "x86_64")

    def test_virtio_gl_has_no_venus_kernel_floor(self):
        self.require("virtio-gl", "max_outputs=<uint32>", kernel="6.12.9")

    def test_a_qemu_that_does_not_run_is_a_failure_not_blocked(self):
        with self.assertRaises(RuntimeError) as error:
            self.require("venus", "", qemu_runs=False)
        self.assertNotIsInstance(error.exception, vm_test.Blocked)

    def test_venus_on_intel_needs_honor_guest_pat(self):
        venus = "hostmem=<size>\nblob=<bool>\nvenus=<bool>"
        with self.assertRaisesRegex(vm_test.Blocked, "honor-guest-pat"):
            self.require("venus", venus, intel=True, honor=False)
        self.require("venus", venus, intel=True, honor=True)
        self.require("venus", venus, intel=False, honor=False)

    def test_vnc_socket_is_retried_until_it_appears(self):
        with tempfile.TemporaryDirectory() as scratch:
            vm = vm_test.VM.__new__(vm_test.VM)
            vm.proc = SimpleNamespace(poll=lambda: None, returncode=None)
            vm.out = Path(scratch)
            vm.vnc_path = Path(scratch) / "vnc.sock"
            with self.assertRaisesRegex(RuntimeError, "never appeared"):
                vm._connect_vnc(timeout=0.2)
            listener = socket.socket(socket.AF_UNIX)

            def listen_later():
                time.sleep(0.3)
                listener.bind(str(vm.vnc_path))
                listener.listen(1)

            with concurrent.futures.ThreadPoolExecutor() as executor:
                executor.submit(listen_later)
                sock = vm._connect_vnc(timeout=5)
            sock.close()
            listener.close()


if __name__ == "__main__":
    unittest.main()
