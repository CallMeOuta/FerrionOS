import json
import os
import re
import shlex
import tempfile
import time
from pathlib import Path

from desktop_gpu import wait_for_session
from display_modes import set_head_size
from wm_display_heads import output_current_size, parse_json_object

FIXTURE_NAMES = (
    "refresh-4k-60-120-multi.edid",
    "displayid7-8k120.edid",
    "malformed-oversized-9ext.edid",
    "malformed-bad-checksum.edid",
)
MAX_WRITE_BATCH_CHARS = 180
OUTPUT_POLL_LIMIT = 40
OUTPUT_POLL_INTERVAL_S = 0.5
REBOOT_PROMPT_DELAY_S = 0.5
HOTPLUG_BOOT_SIZES = ((1280, 800), (2560, 1600))
HOTPLUG_ON_SIZE = (1920, 1080)
MODE_LINE_RE = re.compile(
    r"(?m)^MODE conn=(\d+) idx=(\d+) (\d+)x(\d+) clock=(\d+) "
    r"htotal=(\d+) vtotal=(\d+) vrefresh=(\d+) type=0x([0-9a-fA-F]+) "
    r"flags=0x([0-9a-fA-F]+) name=(\S+)$"
)
CONN_LINE_RE = re.compile(
    r"(?m)^CONN id=(\d+) .*? name=(\S+) status=(\d+) mm=(\d+)x(\d+) modes=(\d+)$"
)


def write_binary_rle(vm, run_status, dest_path, data):
    """Writes a small binary file into the guest using bounded RLE bash commands."""
    runs = []
    idx = 0
    total = len(data)
    while idx < total:
        if data[idx] == 0:
            end = idx
            while end < total and data[end] == 0:
                end += 1
            runs.append(("Z", end - idx))
            idx = end
        else:
            end = idx
            while end < total and data[end] != 0:
                end += 1
            runs.append(("B", data[idx:end]))
            idx = end
    cmds = [f": > {dest_path}"]
    for kind, val in runs:
        if kind == "Z":
            cmds.append(f"printf '%0*d' {val} 0 | tr 0 '\\000' >> {dest_path}")
        else:
            esc = "".join(f"\\x{b:02x}" for b in val)
            cmds.append(f"printf '{esc}' >> {dest_path}")
    batch = ""
    for cmd in cmds:
        nxt = f"{batch}; {cmd}" if batch else cmd
        if len(nxt) > MAX_WRITE_BATCH_CHARS and batch:
            status, out = run_status(vm, f'/bin/bash -c "{batch}"', 90)
            if status != 0:
                raise RuntimeError(f"write {dest_path} failed ({status}): {out[-400:]}")
            batch = cmd
        else:
            batch = nxt
    if batch:
        status, out = run_status(vm, f'/bin/bash -c "{batch}"', 90)
        if status != 0:
            raise RuntimeError(f"write {dest_path} failed ({status}): {out[-400:]}")
    status, out = run_status(vm, f"/bin/bash -c 'test $(wc -c < {dest_path}) -eq {total}'", 60)
    if status != 0:
        raise RuntimeError(f"size verify {dest_path} != {total}: {out[-400:]}")


def stage_fixtures(vm, run_status, root):
    """Copies the required EDID fixtures into /var/lib/edid-stage so they persist across reboots."""
    status, out = run_status(vm, "mkdir -p /var/lib/edid-stage /usr/lib/firmware/edid", 60)
    if status != 0:
        raise RuntimeError(f"mkdir failed ({status}): {out[-400:]}")
    corpus = Path(root) / "scripts" / "edid-corpus"
    for name in FIXTURE_NAMES:
        write_binary_rle(vm, run_status, f"/var/lib/edid-stage/{name}", (corpus / name).read_bytes())
    status, out = run_status(
        vm,
        "cp /var/lib/edid-stage/refresh-4k-60-120-multi.edid /usr/lib/firmware/outside.edid "
        "&& chmod 0644 /usr/lib/firmware/outside.edid",
        60,
    )
    if status != 0:
        raise RuntimeError(f"stage outside.edid failed ({status}): {out[-400:]}")


def reboot_to_root(vm, login_fn, timeout):
    """Reboots the guest in place and logs back into the root serial shell."""
    since = vm.mark()
    vm.type("shutdown -r\n")
    if not vm.wait_serial(r"login: ", max(timeout, 120), since):
        raise RuntimeError("no login prompt after shutdown -r")
    time.sleep(REBOOT_PROMPT_DELAY_S)
    ok, err = login_fn(vm, "root", "password", r"root:.*# ", since, timeout=max(timeout, 60))
    if not ok:
        raise RuntimeError(f"root login failed after reboot: {err}")
    return since


def parse_drm_probe(output):
    """Parses display-drm-probe CONN and MODE lines into structured records."""
    connectors = {}
    for match in CONN_LINE_RE.finditer(output):
        cid = int(match[1])
        connectors[cid] = {
            "id": cid,
            "name": match[2],
            "status": int(match[3]),
            "mm": f"{match[4]}x{match[5]}",
            "mode_count": int(match[6]),
            "modes": [],
        }
    for match in MODE_LINE_RE.finditer(output):
        cid = int(match[1])
        entry = {
            "idx": int(match[2]),
            "width": int(match[3]),
            "height": int(match[4]),
            "clock": int(match[5]),
            "htotal": int(match[6]),
            "vtotal": int(match[7]),
            "vrefresh": int(match[8]),
            "type": int(match[9], 16),
            "flags": int(match[10], 16),
            "name": match[11],
        }
        if cid in connectors:
            connectors[cid]["modes"].append(entry)
    return {c["name"]: c for c in connectors.values()}


def query_wm_outputs(vm, run_status, timeout, out):
    """Waits for ferrion-wm and queries ferrion-wm msg --json outputs plus alive check."""
    if not wait_for_session(vm, run_status, timeout, out):
        return {}, False
    status, raw = run_status(
        vm,
        "bash -c " + shlex.quote("source /tmp/ferrion-wm-session; ferrion-wm msg --json outputs"),
        timeout,
    )
    if status != 0:
        return {}, False
    outputs = parse_json_object(raw)
    pid_status, _ = run_status(
        vm,
        "bash -c 'test -f /tmp/ferrion-wm.pid && kill -0 $(cat /tmp/ferrion-wm.pid)'",
        30,
    )
    return outputs, pid_status == 0


def edid_log_lines(serial_text):
    """Extracts driver-graphics and virtio-gpud log lines mentioning EDID overrides."""
    return [
        line.strip()
        for line in serial_text.splitlines()
        if ("@driver_graphics" in line or "@virtio_gpud" in line)
        and "edid override" in line.lower()
    ]


def has_wm_mode(output_obj, width, height, min_mhz, max_mhz, require_preferred=False):
    """Checks whether a ferrion-wm output lists a mode matching size, mHz range, and preferred flag."""
    for mode in output_obj.get("modes", []):
        if (
            mode.get("width") == width
            and mode.get("height") == height
            and min_mhz <= mode.get("refresh_rate", 0) <= max_mhz
            and (not require_preferred or bool(mode.get("is_preferred")))
        ):
            return True
    return False


def poll_wm_outputs(vm, run_status, timeout, out, pred):
    """Polls ferrion-wm outputs until pred(outputs) is satisfied or the bound is reached."""
    last = {}
    alive = False
    for _ in range(OUTPUT_POLL_LIMIT):
        last, alive = query_wm_outputs(vm, run_status, timeout, out)
        if alive and pred(last):
            return last, True
        time.sleep(OUTPUT_POLL_INTERVAL_S)
    return last, False


def run_display_edid_override(args, out, vm_cls, root_shell, run_status, login_fn, root):
    """Verifies /usr/lib/firmware/edid/<connector>.edid overrides across valid 4K120, valid 8K120 (max_hostmem=1G), oversized, bad checksum, symlink escape, absent baseline, and Virtual-2 hotplug off/on."""
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    problems = []
    record = {
        "load_before": list(os.getloadavg()),
        "cases": {},
        "hotplug": {},
        "problems": problems,
    }

    single_cases = (
        (
            "valid_4k120",
            "cp /var/lib/edid-stage/refresh-4k-60-120-multi.edid /usr/lib/firmware/edid/Virtual-1.edid "
            "&& chmod 0644 /usr/lib/firmware/edid/Virtual-1.edid",
        ),
        (
            "valid_8k120",
            "cp /var/lib/edid-stage/displayid7-8k120.edid /usr/lib/firmware/edid/Virtual-1.edid "
            "&& chmod 0644 /usr/lib/firmware/edid/Virtual-1.edid",
        ),
        (
            "oversized_1280",
            "cp /var/lib/edid-stage/malformed-oversized-9ext.edid /usr/lib/firmware/edid/Virtual-1.edid "
            "&& chmod 0644 /usr/lib/firmware/edid/Virtual-1.edid",
        ),
        (
            "bad_checksum",
            "cp /var/lib/edid-stage/malformed-bad-checksum.edid /usr/lib/firmware/edid/Virtual-1.edid "
            "&& chmod 0644 /usr/lib/firmware/edid/Virtual-1.edid",
        ),
        (
            "symlink_escape",
            "ln -sf /usr/lib/firmware/outside.edid /usr/lib/firmware/edid/Virtual-1.edid",
        ),
        (
            "absent_monitor_edid",
            "rm -f /usr/lib/firmware/edid/Virtual-1.edid",
        ),
    )

    single_out = out / "single"
    single_out.mkdir(parents=True, exist_ok=True)
    vm = vm_cls(
        args,
        single_out,
        qemu_flags="-device virtio-gpu-pci,edid=on,xres=1280,yres=800,max_hostmem=1G",
        make=("gpu=none",),
    )
    try:
        if not root_shell(vm, args.timeout):
            return False, "single-head boot 1: no root serial login"
        stage_fixtures(vm, run_status, root)

        for case_name, setup_cmd in single_cases:
            status, txt = run_status(
                vm,
                f"/bin/bash -c 'rm -f /usr/lib/firmware/edid/Virtual-1.edid && {setup_cmd}'",
                60,
            )
            if status != 0:
                problems.append(f"{case_name}: setup failed ({status})")
                continue
            since = reboot_to_root(vm, login_fn, args.timeout)
            drm_status, drm_raw = run_status(vm, "display-drm-probe", 60)
            drm = parse_drm_probe(drm_raw) if drm_status == 0 else {}
            wm_outputs, wm_alive = query_wm_outputs(vm, run_status, args.timeout, out / "single")
            logs = edid_log_lines(vm.text(since))
            v1_drm = drm.get("Virtual-1", {})
            v1_wm = wm_outputs.get("Virtual-1", {})
            record["cases"][case_name] = {
                "drm_status": drm_status,
                "drm_modes": v1_drm.get("modes", []),
                "wm_alive": wm_alive,
                "wm_current": output_current_size(v1_wm),
                "wm_modes": v1_wm.get("modes", []),
                "edid_logs": logs,
            }

            if drm_status != 0 or not v1_drm.get("modes"):
                problems.append(f"{case_name}: display-drm-probe failed (status={drm_status})")
                continue
            if not wm_alive:
                problems.append(f"{case_name}: ferrion-wm session did not stay alive")
                continue

            mode0 = v1_drm["modes"][0]
            if case_name == "valid_4k120":
                if not (
                    mode0["width"] == 3840
                    and mode0["height"] == 2160
                    and mode0["vrefresh"] == 120
                    and mode0["htotal"] > 3840
                    and mode0["vtotal"] > 2160
                    and (mode0["type"] & 0x08) != 0
                ):
                    problems.append(f"valid_4k120: unexpected DRM preferred mode {mode0}")
                if not has_wm_mode(v1_wm, 3840, 2160, 119500, 120500, require_preferred=True):
                    problems.append(f"valid_4k120: ferrion-wm missing preferred 3840x2160@120: {v1_wm.get('modes')}")
                if not any("applying EDID override /usr/lib/firmware/edid/Virtual-1.edid" in l for l in logs):
                    problems.append(f"valid_4k120: missing boot override log line: {logs}")
            elif case_name == "valid_8k120":
                if not (
                    mode0["width"] == 7680
                    and mode0["height"] == 4320
                    and mode0["vrefresh"] == 120
                    and mode0["clock"] >= 4_000_000
                    and mode0["htotal"] > 7680
                    and mode0["vtotal"] > 4320
                    and (mode0["type"] & 0x08) != 0
                ):
                    problems.append(f"valid_8k120: unexpected DRM preferred mode {mode0}")
                if not has_wm_mode(v1_wm, 7680, 4320, 119500, 120500, require_preferred=True):
                    problems.append(f"valid_8k120: ferrion-wm missing preferred 7680x4320@120: {v1_wm.get('modes')}")
                if not any("applying EDID override /usr/lib/firmware/edid/Virtual-1.edid" in l for l in logs):
                    problems.append(f"valid_8k120: missing boot override log line: {logs}")
            elif case_name in ("oversized_1280", "bad_checksum", "symlink_escape"):
                if not (mode0["width"] == 1280 and mode0["height"] == 800):
                    problems.append(f"{case_name}: did not fall back to 1280x800, got {mode0}")
                if output_current_size(v1_wm) != (1280, 800):
                    problems.append(f"{case_name}: ferrion-wm did not fall back to 1280x800: {output_current_size(v1_wm)}")
                if not any("ignoring" in l and "EDID override" in l for l in logs):
                    problems.append(f"{case_name}: missing warning log line: {logs}")
            elif case_name == "absent_monitor_edid":
                if not (mode0["width"] == 1280 and mode0["height"] == 800):
                    problems.append(f"absent_monitor_edid: unexpected mode {mode0}")
                if logs:
                    problems.append(f"absent_monitor_edid: unexpected override log lines: {logs}")
    finally:
        vm.close()

    with tempfile.TemporaryDirectory(prefix="ferrion-edid-hotplug-") as directory:
        sockets = [Path(directory) / f"head{head}.sock" for head in range(2)]
        flags = " ".join(
            f"-vnc unix:{path},display=gpu0,head={head}" for head, path in enumerate(sockets)
        )
        hotplug_out = out / "hotplug"
        hotplug_out.mkdir(parents=True, exist_ok=True)
        vm2 = vm_cls(args, hotplug_out, qemu_flags=flags, make=("gpu=multi",))
        try:
            for path, size in zip(sockets, HOTPLUG_BOOT_SIZES):
                set_head_size(path, *size)
            if not root_shell(vm2, args.timeout):
                problems.append("hotplug boot 1: no root serial login")
            else:
                stage_fixtures(vm2, run_status, root)
                status, _ = run_status(
                    vm2,
                    "cp /var/lib/edid-stage/refresh-4k-60-120-multi.edid "
                    "/usr/lib/firmware/edid/Virtual-2.edid "
                    "&& chmod 0644 /usr/lib/firmware/edid/Virtual-2.edid",
                    60,
                )
                if status != 0:
                    problems.append(f"hotplug setup failed ({status})")
                else:
                    since_boot = reboot_to_root(vm2, login_fn, args.timeout)
                    boot_outputs, boot_ok = poll_wm_outputs(
                        vm2,
                        run_status,
                        args.timeout,
                        out / "hotplug",
                        lambda o: has_wm_mode(
                            o.get("Virtual-2", {}), 3840, 2160, 119500, 120500, require_preferred=True
                        ),
                    )
                    boot_drm_st, boot_drm_raw = run_status(vm2, "display-drm-probe", 60)
                    boot_drm = parse_drm_probe(boot_drm_raw) if boot_drm_st == 0 else {}
                    boot_logs = edid_log_lines(vm2.text(since_boot))
                    record["hotplug"]["boot"] = {
                        "wm_ok": boot_ok,
                        "v2_current": output_current_size(boot_outputs.get("Virtual-2", {})),
                        "v2_drm_modes": boot_drm.get("Virtual-2", {}).get("modes", []),
                        "logs": boot_logs,
                    }
                    if not boot_ok:
                        problems.append(f"hotplug boot: Virtual-2 missing 3840x2160@120: {boot_outputs.get('Virtual-2')}")
                    if len([l for l in boot_logs if "Virtual-2" in l and "applying EDID override" in l]) < 1:
                        problems.append(f"hotplug boot: missing Virtual-2 apply log: {boot_logs}")

                    since_off = vm2.mark()
                    set_head_size(sockets[1], 0, 0)
                    off_logged = vm2.wait_serial(r"virtio-gpu: display 1 \(0x0px\)", 30, since_off) is not None
                    off_outputs, off_ok = poll_wm_outputs(
                        vm2,
                        run_status,
                        args.timeout,
                        out / "hotplug",
                        lambda o: "Virtual-2" not in o,
                    )
                    record["hotplug"]["off"] = {
                        "logged": off_logged,
                        "wm_ok": off_ok,
                        "outputs": sorted(off_outputs.keys()),
                    }
                    if not off_logged or not off_ok:
                        problems.append(f"hotplug off failed: logged={off_logged} wm_ok={off_ok}")

                    since_on = vm2.mark()
                    set_head_size(sockets[1], *HOTPLUG_ON_SIZE)
                    on_logged = (
                        vm2.wait_serial(
                            rf"virtio-gpu: display 1 \({HOTPLUG_ON_SIZE[0]}x{HOTPLUG_ON_SIZE[1]}px\)",
                            30,
                            since_on,
                        )
                        is not None
                    )
                    on_outputs, on_ok = poll_wm_outputs(
                        vm2,
                        run_status,
                        args.timeout,
                        out / "hotplug",
                        lambda o: has_wm_mode(
                            o.get("Virtual-2", {}), 3840, 2160, 119500, 120500, require_preferred=True
                        )
                        and output_current_size(o.get("Virtual-2", {})) == (3840, 2160),
                    )
                    on_drm_st, on_drm_raw = run_status(vm2, "display-drm-probe", 60)
                    on_drm = parse_drm_probe(on_drm_raw) if on_drm_st == 0 else {}
                    on_logs = edid_log_lines(vm2.text(since_on))
                    record["hotplug"]["on"] = {
                        "logged": on_logged,
                        "wm_ok": on_ok,
                        "v2_current": output_current_size(on_outputs.get("Virtual-2", {})),
                        "v2_drm_modes": on_drm.get("Virtual-2", {}).get("modes", []),
                        "logs": on_logs,
                    }
                    if not on_logged or not on_ok:
                        problems.append(
                            f"hotplug on failed: logged={on_logged} wm_ok={on_ok} "
                            f"v2={on_outputs.get('Virtual-2')}"
                        )
                    if len([l for l in on_logs if "Virtual-2" in l and "applying EDID override" in l]) < 1:
                        problems.append(f"hotplug on: missing second Virtual-2 apply log: {on_logs}")
        finally:
            vm2.close()

    record["load_after"] = list(os.getloadavg())
    report_path = out / "display-edid-override.json"
    report_path.write_text(json.dumps(record, indent=2) + "\n")
    if problems:
        return False, "; ".join(problems) + f"; see {report_path}"
    m4k = record["cases"]["valid_4k120"]["drm_modes"][0]
    m8k = record["cases"]["valid_8k120"]["drm_modes"][0]
    return (
        True,
        f"6/6 single-head + Virtual-2 hotplug PASS; "
        f"4k120={m4k['width']}x{m4k['height']}@{m4k['vrefresh']}(clk={m4k['clock']},ht={m4k['htotal']},vt={m4k['vtotal']}), "
        f"8k120={m8k['width']}x{m8k['height']}@{m8k['vrefresh']}(clk={m8k['clock']},ht={m8k['htotal']},vt={m8k['vtotal']}), "
        f"hotplug_v2={record['hotplug']['on']['v2_current']}; see {report_path}",
    )
