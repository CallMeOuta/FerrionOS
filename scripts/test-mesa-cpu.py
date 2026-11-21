#!/usr/bin/env python3
"""Check Mesa CPU probe rejection paths without a guest or cross compiler."""

import ast
from pathlib import Path
import re
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock

ROOT = Path(__file__).resolve().parent.parent


class MesaCPUChecks(unittest.TestCase):
    def test_platform_failures_and_version(self):
        patch = (ROOT / "recipes/libs/mesa/ferrion-orbital.patch").read_text()
        source = "\n".join(line[1:] for line in patch.splitlines()
                           if line.startswith("+") and not line.startswith("+++"))
        start = source.index("static _EGLSurface *\ndri2_redox_create_window_surface(")
        end = source.index("static void\nredox_swrast_get_drawable_info(", start)
        with tempfile.TemporaryDirectory(prefix="ferrion-mesa-test-") as tmp:
            tmp = Path(tmp)
            (tmp / "orbital-functions.h").write_text(source[start:end])
            binary = tmp / "platform-test"
            subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror",
                            "-Wno-unused-parameter", "-I", str(ROOT), "-I", str(tmp),
                            str(ROOT / "scripts/mesa-cpu-test.c"), "-o", str(binary)], check=True)
            subprocess.run([str(binary)], check=True)

    def test_vm_missing_completion_marker(self):
        tree = ast.parse((ROOT / "scripts/vm-test.py").read_text())
        function = next(node for node in tree.body
                        if isinstance(node, ast.FunctionDef) and node.name == "test_mesa_cpu")
        namespace = {"root_shell": lambda vm, timeout: True}
        exec(compile(ast.Module(body=[function], type_ignores=[]), "vm-test.py", "exec"), namespace)
        output = "\n".join(f"GPU_PIXELS api={api} triangle=17540 mismatches=0 hash=47972063c7a47bfb"
                           for api in ("gl", "vulkan")) + "\nMESA-CPU-EXIT-0\n"
        with tempfile.TemporaryDirectory(prefix="ferrion-mesa-vm-test-") as tmp:
            vm = Mock(out=Path(tmp))
            vm.wait_serial.return_value = re.search(r"EXIT-(\d+)", output)
            vm.text.return_value = output
            passed, note = namespace["test_mesa_cpu"](vm, SimpleNamespace(timeout=1))
            self.assertFalse(passed)
            self.assertIn("missing GPU_MESA_CPU_OK", note)
            vm.text.return_value += "GPU_MESA_CPU_OK\n"
            passed, note = namespace["test_mesa_cpu"](vm, SimpleNamespace(timeout=1))
            self.assertTrue(passed, note)


if __name__ == "__main__":
    unittest.main()
