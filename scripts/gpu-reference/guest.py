#!/usr/bin/env python3
# PID 1 of the offline Linux reference guest. Every subprocess is bounded.

import hashlib
import json
import os
import re
import subprocess
import sys
import termios
import traceback
from pathlib import Path

EXPECTED_HASH = "47972063c7a47bfb"
ICDS = Path("/usr/share/vulkan/icd.d")
BASE_ENV = {"PATH": "/usr/bin:/bin", "HOME": "/tmp", "XDG_RUNTIME_DIR": "/run",
            "LC_ALL": "C", "MESA_SHADER_CACHE_DISABLE": "true", "LP_NUM_THREADS": "2"}


def run(command, env=None, timeout=35):
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=timeout,
                                env=BASE_ENV | (env or {}))
        return {"command": command, "exit_code": result.returncode,
                "stdout": result.stdout, "stderr": result.stderr}
    except subprocess.TimeoutExpired as error:
        return {"command": command, "exit_code": None, "error": "timeout",
                "stdout": (error.stdout or b"").decode(errors="replace"),
                "stderr": (error.stderr or b"").decode(errors="replace")}


def required(command):
    result = run(command, timeout=60)
    if result["exit_code"] != 0:
        raise RuntimeError(json.dumps(result))
    return result


def gl_test(name, env):
    versions = run(["/tmp/probe", "gl"], env)
    scene = run(["/tmp/scene"], env)
    result = {"name": name, "environment": env, "versions": versions, "scene_run": scene}
    result["apis"] = {}
    for line in versions["stdout"].splitlines():
        fields = line.split("\t")
        if len(fields) == 3 and fields[0] in ("gl", "gles"):
            result["apis"][fields[0]] = {"version": fields[1], "renderer": fields[2]}
    match = re.search(r'GLTRI renderer="(.*?)" hash=(\w+) tri=(\d+) mismatches=(\d+) centers_on_edge=(\d+)', scene["stdout"])
    if match:
        result["scene"] = {"renderer": match[1], "hash": match[2], "triangle_pixels": int(match[3]),
                           "differing_pixels": int(match[4]), "centers_on_edge": int(match[5])}
    passed = (versions["exit_code"] == 0 and len(result["apis"]) == 2 and scene["exit_code"] == 0 and match
              and match[2] == EXPECTED_HASH and tuple(map(int, match.group(3, 4, 5))) == (17540, 0, 0))
    # An accelerated test must never silently fall back to the CPU renderer.
    if name != "llvmpipe" and match and any(s in match[1].lower() for s in ("llvmpipe", "softpipe", "lavapipe")):
        passed = False
    if name == "virgl" and match and "virgl" not in match[1].lower():
        passed = False
    if name == "zink-venus" and match and "zink" not in match[1].lower():
        passed = False
    if name == "llvmpipe" and match and "llvmpipe" not in match[1].lower():
        passed = False
    result["status"] = "PASS" if passed else "FAIL"
    return result


def vulkan_test(name, filename):
    output = run(["/tmp/probe", "vulkan"], {"VK_DRIVER_FILES": str(ICDS / filename)})
    devices = []
    for line in output["stdout"].splitlines():
        fields = line.split("\t")
        if fields[0] == "device" and len(fields) == 10:
            devices.append(dict(zip(("name", "api_version", "conformance_version", "extension_count",
                                     "vendor_id", "device_id", "driver_name", "driver_info", "driver_version"), fields[1:])))
            devices[-1]["extensions"] = []
        elif fields[0] == "extension" and devices:
            devices[-1]["extensions"].append(fields[1])
    for device in devices:
        device["extension_count"] = int(device["extension_count"])
    return {"name": name, "run": output, "devices": devices}


def main():
    for kind, target in (("devtmpfs", "/dev"), ("proc", "/proc"), ("sysfs", "/sys")):
        required(["/bin/busybox", "mount", "-t", kind, kind, target])
    for module in json.loads(Path("/probe/modules.json").read_text()):
        required(["/bin/busybox", "insmod", module])
    log = required(["/bin/busybox", "dmesg"])["stdout"]
    for source, output, libraries in (("scene.c", "scene", ["-lEGL", "-lGLESv2"]),
                                      ("probe.c", "probe", ["-lEGL", "-lGLESv2", "-lvulkan"])):
        required(["/usr/bin/gcc", "-O2", "-Wall", "-Wextra", "-Werror", "-I/usr/include/libdrm",
                  "/probe/" + source, "-o", "/tmp/" + output, *libraries])
    capsets = []
    for match in re.finditer(r"cap set \d+: id (\d+), max-version (\d+), max-size (\d+)", log):
        capsets.append(dict(zip(("id", "max_version", "max_size"), map(int, match.groups()))))
    venus = next((c for c in capsets if c["id"] == 4), None)
    caps = required(["/tmp/probe", "caps", *([str(venus["max_size"])] if venus else [])])
    result = {"status": "FAIL", "kernel": os.uname().release, "glibc": os.confstr("CS_GNU_LIBC_VERSION"),
              "kernel_gpu_log": [line for line in log.splitlines() if "[drm]" in line or "virtio_gpu" in line],
              "capsets": capsets, "params": {}, "gl": [], "vulkan": []}
    for line in caps["stdout"].splitlines():
        fields = line.split("\t")
        if fields[0] == "param":
            result["params"][fields[1]] = int(fields[2])
        elif fields[0] == "venus_capset":
            data = bytes.fromhex(fields[1])
            result["venus_capset"] = {"id": 4, "version": 0, "size": len(data), "bytes_hex": data.hex(),
                                       "sha256": hashlib.sha256(data).hexdigest()}
    result["gl"].append(gl_test("virgl", {"GALLIUM_DRIVER": "virgl"}))
    result["gl"].append(gl_test("llvmpipe", {"LIBGL_ALWAYS_SOFTWARE": "1", "GALLIUM_DRIVER": "llvmpipe"}))
    result["vulkan"].append(vulkan_test("venus", "virtio_icd.json"))
    if venus:
        result["gl"].append(gl_test("zink-venus", {"MESA_LOADER_DRIVER_OVERRIDE": "zink", "GALLIUM_DRIVER": "zink",
                                                  "VK_DRIVER_FILES": str(ICDS / "virtio_icd.json")}))
    if "gpu_reference=native" in Path("/proc/cmdline").read_text():
        native = vulkan_test("intel-native", "intel_icd.json")
        result["vulkan"].append(native)
        if native["devices"]:
            result["native_context"] = {"status": "AVAILABLE"}
            result["gl"].append(gl_test("intel-native", {"MESA_LOADER_DRIVER_OVERRIDE": "iris", "GALLIUM_DRIVER": "iris"}))
            result["gl"].append(gl_test("zink-native", {"MESA_LOADER_DRIVER_OVERRIDE": "zink", "GALLIUM_DRIVER": "zink",
                                                       "VK_DRIVER_FILES": str(ICDS / "intel_icd.json")}))
        else:
            result["native_context"] = {"status": "UNSUPPORTED",
                "reason": "No Intel native Vulkan device. The pinned kernel is below QEMU's 6.14 guest floor; Intel also needs Mesa 26.1 or newer."}
    good = all(g["status"] == "PASS" for g in result["gl"])
    devices = result["vulkan"][0]["devices"]
    mode = re.search(r"gpu_reference=(\w+)", Path("/proc/cmdline").read_text())[1]
    expected = {1, 2} if mode == "virgl" else {1, 2, 4}
    good = good and expected.issubset({c["id"] for c in capsets})
    if mode != "virgl":
        good = good and result["vulkan"][0]["run"]["exit_code"] == 0 and bool(devices)
        good = good and all(d["driver_name"] == "venus" and len(d["extensions"]) == d["extension_count"] for d in devices)
    else:
        good = good and not devices
    result["status"] = "PASS" if good else "FAIL"
    return result


try:
    result = main()
except Exception as error:
    result = {"status": "FAIL", "error": str(error), "traceback": traceback.format_exc()}
# Kernel shutdown messages otherwise interleave with a long serial record.
# Keep the earlier kernel log in the record, then drain the UART before S5.
Path("/proc/sys/kernel/printk").write_text("0\n")
print("GPU_REFERENCE_JSON=" + json.dumps(result, separators=(",", ":")), flush=True)
termios.tcdrain(sys.stdout.fileno())
subprocess.run(["/bin/busybox", "poweroff", "-f"])
while True:
    import time
    time.sleep(1)
