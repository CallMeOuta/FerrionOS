import argparse
import json
import math
import os
import re
import shlex
import socket
import statistics
import tempfile
import time
from pathlib import Path

from desktop_fixtures import compare_probes, load_regions

ROLES = ("files", "editor", "terminal")
LAYOUTS = ("tiling", "traditional")
SESSION_POLL_LIMIT = 900
WINDOW_POLL_LIMIT = 150
STOP_POLL_LIMIT = 50
STOP_POLL_INTERVAL_S = 0.1
MAX_QMP_REPLIES = 256
MAX_JSON_SCAN_MATCHES = 128
INPUT_STEPS = 40
MIN_FLIP_FRAMES = 10
FRAME_DIFF_CHANNEL_TOL = 2
FRAME_DIFF_MAX_FRACTION = 0.005
VENUS_TRACE_EVENTS = ("virtio_gpu_cmd_ctx_submit", "virtio_gpu_fence_resp")
EXPECTED_WIDTH = 1280
EXPECTED_HEIGHT = 800


def summary(samples):
    ordered = sorted(samples)
    if not ordered:
        return None
    return {
        "p50": statistics.median(ordered),
        "p99": ordered[math.ceil(len(ordered) * 0.99) - 1],
        "max": ordered[-1],
        "count": len(ordered),
    }


def parse_json_array(output):
    decoder = json.JSONDecoder()
    for idx, match in enumerate(re.finditer(r"\[", output)):
        if idx >= MAX_JSON_SCAN_MATCHES:
            break
        try:
            value, _ = decoder.raw_decode(output[match.start():])
            if isinstance(value, list) and all(isinstance(w, dict) and "id" in w for w in value):
                return value
        except ValueError:
            pass
    raise RuntimeError("window query returned no JSON array: " + output[-500:])


def parse_wm_log(text, client_logs=""):
    renderer_match = re.search(r'FERRION_RENDERER\s+renderer="([^"]*)"', text)
    renderer = renderer_match.group(1) if renderer_match else None
    present_mode_match = re.search(r"FERRION_PRESENTER\s+mode=(\w+)", text)
    presenter_mode = present_mode_match.group(1) if present_mode_match else None
    ready_match = re.search(r"FERRION_READY\s+width=(\d+)\s+height=(\d+)\s+vt=(\d+)", text)
    ready_vt = int(ready_match.group(3)) if ready_match else None
    frames = []
    for line in text.splitlines():
        if "FERRION_FRAME " not in line:
            continue
        kv = dict(re.findall(r"(\w+)=([\w-]+)", line.split("FERRION_FRAME ", 1)[1]))
        if "frame" not in kv:
            continue
        entry = {"frame": int(kv["frame"])}
        for key in (
            "started_us",
            "completed_us",
            "composite_us",
            "sync_wait_us",
            "copy_us",
            "readback_us",
            "present_us",
        ):
            if key in kv:
                entry[key] = int(kv[key])
        if "mode" in kv:
            entry["mode"] = kv["mode"]
        frames.append(entry)
    flip_frames = [f for f in frames if f.get("mode") == "flip"]
    dirty_frames = [f for f in frames if f.get("mode") == "dirtyfb"]
    present_durations = [
        f["present_us"]
        if "present_us" in f
        else f["completed_us"] - f["started_us"]
        for f in frames
        if "present_us" in f or ("started_us" in f and "completed_us" in f)
    ]
    composite_us = [f["composite_us"] for f in frames if "composite_us" in f]
    sync_wait_us = [f["sync_wait_us"] for f in frames if "sync_wait_us" in f]
    copy_us = [f["copy_us"] for f in frames if "copy_us" in f]
    readback_us = [f["readback_us"] for f in frames if "readback_us" in f]
    cadence_us = [
        b["completed_us"] - a["completed_us"]
        for a, b in zip(frames, frames[1:])
        if "completed_us" in a and "completed_us" in b and b["completed_us"] >= a["completed_us"]
    ]
    combined = text + "\n" + client_logs
    dd2_frames = [
        {
            "role": role,
            "key": int(key),
            "presented_ns": int(presented),
        }
        for role, _seq, key, _commit, presented, _out in re.findall(
            r"DD2_PRESENT role=(\w+) frame=(\d+) key=(\d+) commit_ns=(\d+) presented_ns=(\d+) output_frame=(\d+)",
            combined,
        )
    ]
    key_events = [
        (role, int(key), int(received))
        for role, key, received in re.findall(
            r"DD2_KEY role=(\w+) key=(\d+) received_ns=(\d+)", combined
        )
    ]
    key_samples = []
    used = set()
    for read in re.findall(r"FERRION_KEY code=30 read_ns=(\d+)", combined):
        read_ns = int(read)
        candidates = sorted(
            (received, role, key)
            for role, key, received in key_events
            if received >= read_ns and (role, key) not in used
        )
        if not candidates:
            continue
        _received, role, key = candidates[0]
        used.add((role, key))
        presented = [f for f in dd2_frames if f["role"] == role and f["key"] == key]
        if presented:
            first = min(presented, key=lambda f: f["presented_ns"])
            key_samples.append((first["presented_ns"] - read_ns) / 1000.0)
    return {
        "renderer": renderer,
        "presenter_mode": presenter_mode,
        "ready_vt": ready_vt,
        "frame_count": len(frames),
        "flip_frame_count": len(flip_frames),
        "dirty_frame_count": len(dirty_frames),
        "frame_us": summary(cadence_us),
        "key_to_present_us": summary(key_samples),
        "composite_us": summary(composite_us),
        "sync_wait_us": summary(sync_wait_us),
        "copy_us": summary(copy_us),
        "readback_us": summary(readback_us),
        "present_us": summary(present_durations),
    }


def diff_frames(frame_a, frame_b):
    if (frame_a.width, frame_a.height) != (frame_b.width, frame_b.height):
        return {
            "comparable": False,
            "reason": f"size mismatch {frame_a.width}x{frame_a.height} vs {frame_b.width}x{frame_b.height}",
        }
    total = frame_a.width * frame_a.height
    assert len(frame_a.rgb) == total * 3
    assert len(frame_b.rgb) == total * 3
    differing = 0
    max_channel_diff = 0
    a_bytes = frame_a.rgb
    b_bytes = frame_b.rgb
    for offset in range(0, len(a_bytes), 3):
        dr = abs(a_bytes[offset] - b_bytes[offset])
        dg = abs(a_bytes[offset + 1] - b_bytes[offset + 1])
        db = abs(a_bytes[offset + 2] - b_bytes[offset + 2])
        d = max(dr, dg, db)
        if d > max_channel_diff:
            max_channel_diff = d
        if d > FRAME_DIFF_CHANNEL_TOL:
            differing += 1
    return {
        "comparable": True,
        "total_pixels": total,
        "differing_pixels": differing,
        "diff_fraction": differing / total if total else 1.0,
        "max_channel_diff": max_channel_diff,
        "channel_tolerance": FRAME_DIFF_CHANNEL_TOL,
    }


def strip_gpu_make(make_args):
    return [item for item in make_args if not item.startswith("gpu=")]


def wait_for_session(vm, run_status, timeout, out):
    deadline = time.monotonic() + timeout
    for _ in range(SESSION_POLL_LIMIT):
        st, _ = run_status(vm, "test -s /tmp/ferrion-wm-session", timeout)
        if st == 0:
            return True
        if time.monotonic() >= deadline:
            break
        time.sleep(0.2)
    _, diag = run_status(vm, "cat /tmp/ferrion-wm.log 2>/dev/null || true; ps", 20)
    (out / "startup-diagnostics.txt").write_text(diag)
    return False


def read_wm_pid(vm, run_status, timeout):
    st, raw = run_status(vm, "cat /tmp/ferrion-wm.pid 2>/dev/null || true", timeout)
    if st != 0:
        return None
    match = re.search(r"(?m)^\s*(\d+)\s*$", raw)
    return int(match.group(1)) if match else None


def run_phase(
    vm,
    qmp_send,
    run_status,
    args,
    phase_out,
    phase_name,
    gpu_mode,
    check_layout=True,
):
    phase_out = Path(phase_out)
    phase_out.mkdir(parents=True, exist_ok=True)
    record = {
        "phase": phase_name,
        "gpu": gpu_mode,
        "load_before": [round(x, 2) for x in os.getloadavg()],
        "layouts": {},
        "problems": [],
    }
    captured = {}

    if not wait_for_session(vm, run_status, args.timeout, phase_out):
        record["problems"].append(f"{phase_name}: no /tmp/ferrion-wm-session")
        return record, captured

    record["wm_pid"] = read_wm_pid(vm, run_status, args.timeout)

    def guest_run(command):
        status, output = run_status(
            vm,
            "bash -c " + shlex.quote("source /tmp/ferrion-wm-session; " + command),
            args.timeout,
        )
        if status != 0:
            raise RuntimeError(f"guest command failed ({status}): {command}: {output[-500:]}")
        return output

    run_status(vm, "rm -f /tmp/dg-files.log /tmp/dg-editor.log /tmp/dg-terminal.log", 20)
    for role in ROLES:
        guest_run(
            f"desktop-layout-probe {role}"
            + (" --slow" if role == "terminal" else "")
            + f" > /tmp/dg-{role}.log 2>&1 &"
        )
        time.sleep(0.4)

    ids = {}
    win_deadline = time.monotonic() + args.timeout
    for _ in range(WINDOW_POLL_LIMIT):
        current = parse_json_array(guest_run("ferrion-wm msg --json windows"))
        ids = {
            w["app_id"].removeprefix("ferrion-desktop-"): w["id"]
            for w in current
            if (w.get("app_id") or "").startswith("ferrion-desktop-")
        }
        if set(ids) == set(ROLES) or time.monotonic() >= win_deadline:
            break
        time.sleep(0.2)
    if set(ids) != set(ROLES):
        record["problems"].append(f"{phase_name}: expected three probe windows, got {sorted(ids)}")
        return record, captured

    guest_run("ferrion-wm msg action load-config-file --path /etc/ferrion-wm/config.kdl")

    layouts = LAYOUTS if check_layout else ("tiling",)
    for layout in layouts:
        regions = load_regions(f"desktop-{layout}.svg")
        if check_layout:
            for role in ROLES:
                ident = ids[role]
                x, y, width, height = map(int, regions[f"region-window-{role}"]["rect"])
                if layout == "traditional":
                    guest_run(f"ferrion-wm msg action move-window-to-floating --id {ident}")
                    guest_run(f"ferrion-wm msg action set-window-width --id {ident} {width - 2}")
                    guest_run(f"ferrion-wm msg action set-window-height --id {ident} {height - 2}")
                    guest_run(f"ferrion-wm msg action move-floating-window --id {ident} --x {x} --y {y}")
                else:
                    guest_run(f"ferrion-wm msg action move-window-to-tiling --id {ident}")
                    guest_run(f"ferrion-wm msg action set-window-width --id {ident} {width - 2}")
            if layout == "tiling":
                for index, role in enumerate(ROLES, 1):
                    guest_run(f"ferrion-wm msg action focus-window --id {ids[role]}")
                    guest_run(f"ferrion-wm msg action move-column-to-index {index}")
                guest_run(f"ferrion-wm msg action focus-window --id {ids['files']}")
        guest_run(f"ferrion-wm msg action focus-window --id {ids['editor']}")
        time.sleep(1.0)

        frame = vm.screenshot()
        frame.save_png(phase_out / f"{layout}.png")
        captured[layout] = frame
        layout_rec = {
            "width": frame.width,
            "height": frame.height,
            "capture_source": vm.last_capture_source,
            "non_black_bytes": sum(1 for byte in frame.rgb if byte != 0),
            "regions": {},
        }
        if check_layout:
            if (frame.width, frame.height) != (EXPECTED_WIDTH, EXPECTED_HEIGHT):
                record["problems"].append(
                    f"{phase_name} {layout}: display is {frame.width}x{frame.height}"
                )
            for role in ROLES:
                points = regions[f"region-window-{role}"]["probes"]
                if layout == "tiling" and role == "files":
                    points = [p for p in points if p[:2] != (417, 66)]
                differences = compare_probes(frame, points)
                layout_rec["regions"][role] = {
                    "checked": len(points),
                    "matched": len(points) - len(differences),
                    "differences": differences,
                }
        elif frame.width == 0 or frame.height == 0 or layout_rec["non_black_bytes"] == 0:
            record["problems"].append(
                f"{phase_name} {layout}: blank frame {frame.width}x{frame.height}"
            )
        record["layouts"][layout] = layout_rec

        for step in range(INPUT_STEPS):
            qmp_send(
                "input-send-event",
                {
                    "events": [
                        {"type": "abs", "data": {"axis": "x", "value": 1000 + step * 200}},
                        {"type": "abs", "data": {"axis": "y", "value": 1000 + step * 200}},
                    ]
                },
            )
            if step % 4 == 0:
                vm.command("sendkey a")
            time.sleep(0.04)
        time.sleep(0.5)

    _, wm_log = run_status(
        vm, "cat /tmp/ferrion-wm.log 2>/dev/null || true", max(args.timeout, 60)
    )
    (phase_out / "ferrion-wm.log").write_text(wm_log)
    _, client_logs = run_status(
        vm,
        "grep -hE '^(DD2_KEY |DD2_PRESENT .* key=[1-9])' "
        "/tmp/dg-files.log /tmp/dg-editor.log /tmp/dg-terminal.log 2>/dev/null || true",
        max(args.timeout, 60),
    )
    (phase_out / "clients.log").write_text(client_logs)
    record["wm_log"] = parse_wm_log(wm_log, client_logs)
    record["capture_sources"] = sorted(vm.capture_sources)
    record["last_capture_source"] = vm.last_capture_source
    record["load_after"] = [round(x, 2) for x in os.getloadavg()]
    return record, captured


def open_vm_and_shell(args, out, gpu_mode, trace_events, vm_cls, root_shell):
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    run_args = argparse.Namespace(
        **{
            **vars(args),
            "make": strip_gpu_make(args.make),
            "trace": [*args.trace, *trace_events],
        }
    )
    directory = tempfile.TemporaryDirectory(prefix=f"ferrion-gpu-{gpu_mode}-")
    qmp_path = Path(directory.name) / "qmp.sock"
    vm = vm_cls(
        run_args,
        out,
        qemu_flags=f"-qmp unix:{qmp_path},server=on,wait=off",
        make=(f"gpu={gpu_mode}",),
    )
    qmp_sock = socket.socket(socket.AF_UNIX)
    qmp_sock.settimeout(args.timeout)
    qmp_sock.connect(str(qmp_path))
    qmp_stream = qmp_sock.makefile("rwb")
    greeting = json.loads(qmp_stream.readline())
    if "QMP" not in greeting:
        raise RuntimeError("QEMU sent no QMP greeting")

    def qmp_send(command, arguments=None):
        payload = json.dumps({"execute": command, "arguments": arguments or {}}).encode() + b"\n"
        qmp_stream.write(payload)
        qmp_stream.flush()
        for _ in range(MAX_QMP_REPLIES):
            reply = json.loads(qmp_stream.readline())
            if "error" in reply:
                raise RuntimeError(f"QMP {command}: {reply['error']}")
            if "return" in reply:
                return reply["return"]
        raise RuntimeError(f"QMP {command}: exceeded {MAX_QMP_REPLIES} replies")

    qmp_send("qmp_capabilities")
    if not root_shell(vm, args.timeout):
        raise RuntimeError("serial root shell unavailable")
    mark = vm.mark()
    vm.type("bash --norc --noprofile --noediting\n")
    if not vm.wait_serial(r"bash-[\d.]+# ", args.timeout, mark):
        raise RuntimeError("serial Bash prompt unavailable")
    return directory, vm, qmp_sock, qmp_stream, qmp_send


def run_single_boot(
    args,
    out,
    gpu_mode,
    trace_events,
    vm_cls,
    root_shell,
    run_status,
    check_layout=True,
):
    out = Path(out)
    directory = vm = qmp_sock = qmp_stream = None
    try:
        directory, vm, qmp_sock, qmp_stream, qmp_send = open_vm_and_shell(
            args, out, gpu_mode, trace_events, vm_cls, root_shell
        )
        record, captured = run_phase(
            vm, qmp_send, run_status, args, out, gpu_mode, gpu_mode, check_layout=check_layout
        )
    finally:
        if qmp_stream is not None:
            qmp_stream.close()
        if qmp_sock is not None:
            qmp_sock.close()
        if vm is not None:
            vm.close()
            submits = vm.trace("virtio_gpu_cmd_ctx_submit")
            fences = vm.trace("virtio_gpu_fence_resp")
            record["trace"] = {
                "ctx_submit_count": len(submits),
                "fence_resp_count": len(fences),
            }
        if directory is not None:
            directory.cleanup()
    return record, captured


def run_venus_dual_boot(args, out, vm_cls, root_shell, run_status):
    out = Path(out)
    directory = vm = qmp_sock = qmp_stream = None
    default_rec = {"phase": "venus-default", "gpu": "venus", "layouts": {}, "problems": []}
    gpu_rec = {"phase": "venus-gpu-restart", "gpu": "venus", "layouts": {}, "problems": []}
    default_frames = {}
    gpu_frames = {}
    default_submits_before_restart = 0
    try:
        directory, vm, qmp_sock, qmp_stream, qmp_send = open_vm_and_shell(
            args, out, "venus", VENUS_TRACE_EVENTS, vm_cls, root_shell
        )
        default_rec, default_frames = run_phase(
            vm, qmp_send, run_status, args, out / "venus-default", "venus-default", "venus", True
        )
        default_submits_before_restart = len(vm.trace("virtio_gpu_cmd_ctx_submit"))
        default_rec["trace"] = {"ctx_submit_count": default_submits_before_restart}

        old_pid = default_rec.get("wm_pid")
        if old_pid is None:
            gpu_rec["problems"].append("venus-gpu-restart: missing /tmp/ferrion-wm.pid from initial session")
            return default_rec, default_frames, gpu_rec, gpu_frames

        run_status(
            vm,
            "bash -c "
            + shlex.quote("source /tmp/ferrion-wm-session; ferrion-wm msg action quit --skip-confirmation"),
            args.timeout,
        )
        st_alive, _ = run_status(vm, f"kill -0 {old_pid} 2>/dev/null", args.timeout)
        if st_alive == 0:
            run_status(vm, f"kill {old_pid} 2>/dev/null || true", args.timeout)

        exited = False
        for _ in range(STOP_POLL_LIMIT):
            st_alive, _ = run_status(vm, f"kill -0 {old_pid} 2>/dev/null", args.timeout)
            if st_alive != 0:
                exited = True
                break
            time.sleep(STOP_POLL_INTERVAL_S)
        if not exited:
            gpu_rec["problems"].append(
                f"venus-gpu-restart: initial ferrion-wm PID {old_pid} did not exit within {STOP_POLL_LIMIT} polls"
            )
            return default_rec, default_frames, gpu_rec, gpu_frames

        run_status(
            vm,
            "rm -f /tmp/ferrion-wm-session /tmp/ferrion-wm.log /tmp/ferrion-wm.pid",
            args.timeout,
        )
        run_status(
            vm,
            "(FERRION_WM_GPU=1 /bin/bash /usr/lib/ferrion-wm-test-session </dev/null >/dev/null 2>&1 &)",
            args.timeout,
        )

        gpu_rec, gpu_frames = run_phase(
            vm,
            qmp_send,
            run_status,
            args,
            out / "venus-gpu-restart",
            "venus-gpu-restart",
            "venus",
            True,
        )
        new_pid = gpu_rec.get("wm_pid")
        if new_pid is None or new_pid == old_pid:
            gpu_rec["problems"].append(
                f"venus-gpu-restart: expected fresh PID != {old_pid}, got {new_pid}"
            )
        old_vt = (default_rec.get("wm_log") or {}).get("ready_vt")
        new_vt = (gpu_rec.get("wm_log") or {}).get("ready_vt")
        if old_vt is None or new_vt is None or new_vt == old_vt:
            gpu_rec["problems"].append(
                f"venus-gpu-restart: expected FERRION_READY on new VT != {old_vt}, got {new_vt}"
            )
    finally:
        if qmp_stream is not None:
            qmp_stream.close()
        if qmp_sock is not None:
            qmp_sock.close()
        if vm is not None:
            vm.close()
            submits = vm.trace("virtio_gpu_cmd_ctx_submit")
            fences = vm.trace("virtio_gpu_fence_resp")
            (out / "qemu-trace-submits.txt").write_text(
                "\n".join(submits) + ("\n" if submits else "")
            )
            gpu_rec["trace"] = {
                "ctx_submit_count": len(submits) - default_submits_before_restart,
                "total_ctx_submit_count": len(submits),
                "fence_resp_count": len(fences),
            }
        if directory is not None:
            directory.cleanup()
    return default_rec, default_frames, gpu_rec, gpu_frames


def check_regions_and_diff(
    label_a, rec_a, frames_a, label_b, rec_b, frames_b, problems
):
    comparisons = {}
    for layout in LAYOUTS:
        layout_a = (rec_a.get("layouts") or {}).get(layout)
        layout_b = (rec_b.get("layouts") or {}).get(layout)
        if not layout_a or not layout_b:
            problems.append(f"{label_b} vs {label_a} {layout}: missing layout capture")
            continue
        for role in ROLES:
            reg_a = layout_a["regions"][role]
            reg_b = layout_b["regions"][role]
            if reg_b["matched"] < reg_a["matched"]:
                problems.append(
                    f"{layout} window-{role}: {label_b} matched {reg_b['matched']}/{reg_b['checked']} "
                    f"< {label_a} {reg_a['matched']}/{reg_a['checked']}"
                )
        if layout in frames_a and layout in frames_b:
            diff = diff_frames(frames_a[layout], frames_b[layout])
            comparisons[layout] = diff
            if not diff.get("comparable"):
                problems.append(
                    f"{label_b} vs {label_a} {layout}: frame diff not comparable: {diff.get('reason')}"
                )
            elif diff["diff_fraction"] > FRAME_DIFF_MAX_FRACTION:
                problems.append(
                    f"{label_b} vs {label_a} {layout}: whole-frame diff "
                    f"{diff['differing_pixels']}/{diff['total_pixels']} "
                    f"({diff['diff_fraction']:.4%}) > {FRAME_DIFF_MAX_FRACTION:.2%} "
                    f"(max channel diff {diff['max_channel_diff']})"
                )
    return comparisons


def run_desktop_gpu(args, out, vm_cls, root_shell, run_status):
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    cpu_rec, cpu_frames = run_single_boot(
        args, out / "cpu", "virtio", (), vm_cls, root_shell, run_status
    )
    vga_rec, _ = run_single_boot(
        args, out / "vga", "vga", (), vm_cls, root_shell, run_status, check_layout=False
    )
    venus_default_rec, venus_default_frames, venus_gpu_rec, venus_gpu_frames = run_venus_dual_boot(
        args, out / "venus", vm_cls, root_shell, run_status
    )

    problems = [
        *cpu_rec["problems"],
        *vga_rec["problems"],
        *venus_default_rec["problems"],
        *venus_gpu_rec["problems"],
    ]
    cpu_wm = cpu_rec.get("wm_log") or {}
    vga_wm = vga_rec.get("wm_log") or {}
    vdef_wm = venus_default_rec.get("wm_log") or {}
    vgpu_wm = venus_gpu_rec.get("wm_log") or {}

    cpu_renderer = (cpu_wm.get("renderer") or "").lower()
    if not cpu_renderer:
        problems.append("cpu(gpu=virtio): missing FERRION_RENDERER log line")
    elif "llvmpipe" not in cpu_renderer or "zink" in cpu_renderer:
        problems.append(f"cpu(gpu=virtio): expected llvmpipe renderer, got {cpu_wm.get('renderer')!r}")
    cpu_mode = cpu_wm.get("presenter_mode")
    cpu_flips = cpu_wm.get("flip_frame_count", 0)
    if cpu_mode != "flip":
        problems.append(f"cpu(gpu=virtio): expected presenter mode=flip, got {cpu_mode!r}")
    if cpu_flips < MIN_FLIP_FRAMES:
        problems.append(
            f"cpu(gpu=virtio): only {cpu_flips} flip-presented frames (expected >= {MIN_FLIP_FRAMES})"
        )

    vga_renderer = (vga_wm.get("renderer") or "").lower()
    if not vga_renderer:
        problems.append("vga(gpu=vga): missing FERRION_RENDERER log line")
    elif "llvmpipe" not in vga_renderer or "zink" in vga_renderer:
        problems.append(f"vga(gpu=vga): expected llvmpipe renderer, got {vga_wm.get('renderer')!r}")
    vga_mode = vga_wm.get("presenter_mode")
    vga_dirty = vga_wm.get("dirty_frame_count", 0)
    vga_flips = vga_wm.get("flip_frame_count", 0)
    if vga_mode != "dirtyfb":
        problems.append(f"vga(gpu=vga): expected presenter mode=dirtyfb, got {vga_mode!r}")
    if vga_dirty < MIN_FLIP_FRAMES:
        problems.append(
            f"vga(gpu=vga): only {vga_dirty} dirtyfb-presented frames (expected >= {MIN_FLIP_FRAMES})"
        )
    if vga_flips != 0:
        problems.append(f"vga(gpu=vga): unexpected {vga_flips} flip-presented frames on vesad")

    vdef_renderer = (vdef_wm.get("renderer") or "").lower()
    if not vdef_renderer:
        problems.append("venus-default: missing FERRION_RENDERER log line")
    elif "llvmpipe" not in vdef_renderer or "zink" in vdef_renderer:
        problems.append(
            f"venus-default: expected llvmpipe default renderer, got {vdef_wm.get('renderer')!r}"
        )
    vdef_mode = vdef_wm.get("presenter_mode")
    vdef_flips = vdef_wm.get("flip_frame_count", 0)
    if vdef_mode != "flip":
        problems.append(f"venus-default: expected presenter mode=flip, got {vdef_mode!r}")
    if vdef_flips < MIN_FLIP_FRAMES:
        problems.append(
            f"venus-default: only {vdef_flips} flip-presented frames (expected >= {MIN_FLIP_FRAMES})"
        )
    vdef_submits = (venus_default_rec.get("trace") or {}).get("ctx_submit_count", 0)
    if vdef_submits != 0:
        problems.append(
            f"venus-default: expected 0 ctx_submit events under llvmpipe default, got {vdef_submits}"
        )

    vgpu_renderer = (vgpu_wm.get("renderer") or "").lower()
    if not vgpu_renderer:
        problems.append("venus-gpu-restart: missing FERRION_RENDERER log line")
    elif "zink" not in vgpu_renderer or "venus" not in vgpu_renderer or "llvmpipe" in vgpu_renderer:
        problems.append(
            f"venus-gpu-restart: expected Zink+Venus renderer, got {vgpu_wm.get('renderer')!r}"
        )
    vgpu_mode = vgpu_wm.get("presenter_mode")
    if vgpu_mode != "flip":
        problems.append(f"venus-gpu-restart: expected presenter mode=flip, got {vgpu_mode!r}")
    submit_count = (venus_gpu_rec.get("trace") or {}).get("ctx_submit_count", 0)
    if submit_count == 0:
        problems.append("venus-gpu-restart: QEMU trace has 0 virtio_gpu_cmd_ctx_submit events")
    flip_count = vgpu_wm.get("flip_frame_count", 0)
    if flip_count < MIN_FLIP_FRAMES:
        problems.append(
            f"venus-gpu-restart: only {flip_count} flip-presented frames (expected >= {MIN_FLIP_FRAMES})"
        )
    if venus_gpu_rec.get("last_capture_source") != "vnc":
        problems.append(
            f"venus-gpu-restart: expected vnc capture source, got {venus_gpu_rec.get('last_capture_source')!r}"
        )

    for label, rec in (
        ("cpu", cpu_rec),
        ("venus-default", venus_default_rec),
        ("venus-gpu-restart", venus_gpu_rec),
    ):
        for layout in LAYOUTS:
            layout_rec = (rec.get("layouts") or {}).get(layout)
            if not layout_rec:
                continue
            for role in ROLES:
                reg = layout_rec["regions"][role]
                if reg["differences"]:
                    problems.append(
                        f"{label} {layout} window-{role}: " + "; ".join(reg["differences"])
                    )

    cmp_vs_cpu = check_regions_and_diff(
        "cpu", cpu_rec, cpu_frames, "venus-gpu-restart", venus_gpu_rec, venus_gpu_frames, problems
    )
    cmp_vs_vdef = check_regions_and_diff(
        "venus-default",
        venus_default_rec,
        venus_default_frames,
        "venus-gpu-restart",
        venus_gpu_rec,
        venus_gpu_frames,
        problems,
    )

    report = {
        "cpu": cpu_rec,
        "vga": vga_rec,
        "venus-default": venus_default_rec,
        "venus-gpu-restart": venus_gpu_rec,
        "comparisons": {
            "venus_gpu_vs_cpu": cmp_vs_cpu,
            "venus_gpu_vs_venus_default": cmp_vs_vdef,
        },
        "problems": problems,
    }
    (out / "desktop-gpu.json").write_text(json.dumps(report, indent=2) + "\n")
    if problems:
        return False, "; ".join(problems) + f"; see {out / 'desktop-gpu.json'}"
    max_diff_cpu = max(c["max_channel_diff"] for c in cmp_vs_cpu.values())
    max_diff_vdef = max(c["max_channel_diff"] for c in cmp_vs_vdef.values())
    return (
        True,
        f"cpu=llvmpipe({cpu_mode}:{cpu_flips}) "
        f"vga=llvmpipe({vga_mode}:{vga_dirty}) "
        f"venus-default=llvmpipe({vdef_mode}:{vdef_flips},vt={vdef_wm.get('ready_vt')}) "
        f"venus-gpu-restart=zink-venus({vgpu_mode}:{flip_count},vt={vgpu_wm.get('ready_vt')},submits={submit_count}) "
        f"max_diff_cpu={max_diff_cpu} max_diff_vdef={max_diff_vdef}; "
        f"see {out / 'desktop-gpu.json'}",
    )
