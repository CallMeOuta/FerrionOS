import json
import math
import re
import shlex
import statistics
import struct
import tempfile
import time
from pathlib import Path

from desktop_gpu import parse_json_array, wait_for_session
from display_handoff import RFB_MSG_SET_DESKTOP_SIZE, VncResizeClient
from display_modes import (
    HEAD_SIZES,
    HOTPLUG_SIZE,
    desktop_drawn,
    framebuffer_errors,
    pixel,
    set_head_size,
    shot,
)

OUTPUT_POLL_LIMIT = 40
OUTPUT_POLL_INTERVAL_S = 0.5
WINDOW_POLL_LIMIT = 40
WINDOW_POLL_INTERVAL_S = 0.5
FRAME_POLL_LIMIT = 40
FRAME_POLL_INTERVAL_S = 0.5
MAX_JSON_SCAN_MATCHES = 128
DIFF_MIN_POINTS = 32
BACK_TO_BACK_SIZE = (1600, 900)


def summary(samples):
    ordered = sorted(samples)
    if not ordered:
        return None
    return {
        "p50": round(statistics.median(ordered), 1),
        "p99": round(ordered[math.ceil(len(ordered) * 0.99) - 1], 1),
        "max": round(ordered[-1], 1),
        "count": len(ordered),
    }


def parse_json_object(output):
    decoder = json.JSONDecoder()
    for idx, match in enumerate(re.finditer(r"\{", output)):
        if idx >= MAX_JSON_SCAN_MATCHES:
            break
        try:
            value, _ = decoder.raw_decode(output[match.start():])
            if isinstance(value, dict):
                return value
        except ValueError:
            pass
    raise RuntimeError("IPC query returned no JSON object: " + output[-500:])


def parse_json_list(output):
    decoder = json.JSONDecoder()
    for idx, match in enumerate(re.finditer(r"\[", output)):
        if idx >= MAX_JSON_SCAN_MATCHES:
            break
        try:
            value, _ = decoder.raw_decode(output[match.start():])
            if isinstance(value, list):
                return value
        except ValueError:
            pass
    raise RuntimeError("IPC query returned no JSON list: " + output[-500:])


def frame_diff_points(frame, reference):
    if (frame.width, frame.height) != (reference.width, reference.height):
        return 0
    points = [
        ((col * 2 + 1) * frame.width // 128, (row * 2 + 1) * frame.height // 72)
        for col in range(64)
        for row in range(36)
    ]
    return sum(pixel(frame, x, y) != pixel(reference, x, y) for x, y in points)


def send_head_sizes_back_to_back(path, sizes, gap_s=0.0):
    client = VncResizeClient(path)
    try:
        for idx, (width, height) in enumerate(sizes):
            if idx > 0 and gap_s > 0.0:
                time.sleep(gap_s)
            client.sock.sendall(
                struct.pack(
                    "!BBHHBBIHHHHI",
                    RFB_MSG_SET_DESKTOP_SIZE,
                    0,
                    width,
                    height,
                    1,
                    0,
                    0,
                    0,
                    0,
                    width,
                    height,
                    0,
                )
            )
        time.sleep(0.2)
    finally:
        client.close()


def wait_size_and_pred(vm, head, size, pred):
    last = None
    for _ in range(FRAME_POLL_LIMIT):
        last = shot(vm, head=head)
        if (last.width, last.height) == size and pred(last):
            return last, True
        time.sleep(FRAME_POLL_INTERVAL_S)
    return last, False


def parse_per_output_frames(log_text):
    by_output = {}
    for line in log_text.splitlines():
        if "FERRION_FRAME " not in line:
            continue
        kv = dict(re.findall(r"(\w+)=([\w.-]+)", line.split("FERRION_FRAME ", 1)[1]))
        if "frame" not in kv:
            continue
        out_name = kv.get("output", "default")
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
        by_output.setdefault(out_name, []).append(entry)

    result = {}
    for out_name, frames in sorted(by_output.items()):
        cadence_us = [
            b["completed_us"] - a["completed_us"]
            for a, b in zip(frames, frames[1:])
            if "completed_us" in a and "completed_us" in b and b["completed_us"] >= a["completed_us"]
        ]
        result[out_name] = {
            "frame_count": len(frames),
            "flip_frame_count": sum(1 for f in frames if f.get("mode") == "flip"),
            "dirty_frame_count": sum(1 for f in frames if f.get("mode") == "dirtyfb"),
            "frame_us": summary(cadence_us),
            "composite_us": summary([f["composite_us"] for f in frames if "composite_us" in f]),
            "copy_us": summary([f["copy_us"] for f in frames if "copy_us" in f]),
            "present_us": summary([f["present_us"] for f in frames if "present_us" in f]),
        }
    return result


def output_current_size(entry):
    idx = entry.get("current_mode")
    modes = entry.get("modes") or []
    if idx is None or not (0 <= idx < len(modes)):
        return None
    mode = modes[idx]
    return (int(mode["width"]), int(mode["height"]))


def run_wm_display_heads(args, out, vm_cls, root_shell, run_status):
    """Multi-head ferrion-wm check on wm-columns (gpu=multi): boots Virtual-1 (3840x2160) and Virtual-2 (2560x1600), moves a window across heads, hotplugs Virtual-2 off and back on at 1920x1080, and records per-head frame_us."""
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    problems = []
    notes = []
    record = {"problems": problems, "phases": {}}

    with tempfile.TemporaryDirectory(prefix="ferrion-wm-display-heads-") as directory:
        sockets = [Path(directory) / f"head{head}.sock" for head in range(2)]
        flags = " ".join(
            f"-vnc unix:{path},display=gpu0,head={head}" for head, path in enumerate(sockets)
        )
        vm = vm_cls(args, out, qemu_flags=flags, make=("gpu=multi",))
        try:
            for path, size in zip(sockets, HEAD_SIZES):
                set_head_size(path, *size)

            if not root_shell(vm, args.timeout):
                return False, "no root serial login"
            if not wait_for_session(vm, run_status, args.timeout, out):
                return False, f"no /tmp/ferrion-wm-session; see {out / 'startup-diagnostics.txt'}"

            def guest_run(command, check=True):
                status, output = run_status(
                    vm,
                    "bash -c " + shlex.quote("source /tmp/ferrion-wm-session; " + command),
                    args.timeout,
                )
                if check and status != 0:
                    raise RuntimeError(f"guest command failed ({status}): {command}: {output[-500:]}")
                return status, output

            def query_outputs():
                _, raw = guest_run("ferrion-wm msg --json outputs")
                return parse_json_object(raw)

            def query_windows():
                _, raw = guest_run("ferrion-wm msg --json windows")
                return parse_json_array(raw)

            def query_workspaces():
                _, raw = guest_run("ferrion-wm msg --json workspaces")
                return parse_json_list(raw)

            def poll_outputs(pred):
                last = {}
                for _ in range(OUTPUT_POLL_LIMIT):
                    last = query_outputs()
                    if pred(last):
                        return last, True
                    time.sleep(OUTPUT_POLL_INTERVAL_S)
                return last, False

            outputs, boot_ipc_ok = poll_outputs(
                lambda o: output_current_size(o.get("Virtual-1", {})) == HEAD_SIZES[0]
                and output_current_size(o.get("Virtual-2", {})) == HEAD_SIZES[1]
            )
            record["phases"]["boot_outputs"] = {
                k: {
                    "size": output_current_size(v),
                    "logical": v.get("logical"),
                }
                for k, v in sorted(outputs.items())
            }
            if not boot_ipc_ok:
                problems.append(
                    f"boot outputs mismatch: got {record['phases']['boot_outputs']}, "
                    f"expected Virtual-1={HEAD_SIZES[0]} and Virtual-2={HEAD_SIZES[1]}"
                )
                _, wm_log = run_status(
                    vm, "bash -c 'cat /tmp/ferrion-wm.log 2>/dev/null || true'", 30
                )
                (out / "ferrion-wm.log").write_text(wm_log)
                record["per_output_frames"] = parse_per_output_frames(wm_log)
                (out / "wm-display-heads.json").write_text(json.dumps(record, indent=2) + "\n")
                return False, "; ".join(problems)

            boot_h0, h0_ok = wait_size_and_pred(vm, 0, HEAD_SIZES[0], desktop_drawn)
            boot_h1, h1_ok = wait_size_and_pred(vm, 1, HEAD_SIZES[1], desktop_drawn)
            boot_h0.save_png(out / "boot-head0.png")
            boot_h1.save_png(out / "boot-head1.png")
            if not h0_ok:
                problems.append(f"boot head 0 not drawn at {HEAD_SIZES[0]} (got {boot_h0.width}x{boot_h0.height})")
            if not h1_ok:
                problems.append(f"boot head 1 not drawn at {HEAD_SIZES[1]} (got {boot_h1.width}x{boot_h1.height})")
            notes.append(
                f"boot: Virtual-1 {boot_h0.width}x{boot_h0.height} ok={h0_ok}, "
                f"Virtual-2 {boot_h1.width}x{boot_h1.height} ok={h1_ok}"
            )

            guest_run(
                "desktop-layout-probe files >/tmp/wm-heads-probe.log 2>&1 </dev/null &"
            )
            wid = None
            for _ in range(WINDOW_POLL_LIMIT):
                wins = query_windows()
                if wins:
                    wid = wins[0]["id"]
                    break
                time.sleep(WINDOW_POLL_INTERVAL_S)
            if wid is None:
                problems.append("probe window did not open")
            else:
                win_h0, win_h0_ok = wait_size_and_pred(
                    vm, 0, HEAD_SIZES[0], lambda f: frame_diff_points(f, boot_h0) >= DIFF_MIN_POINTS
                )
                win_h0.save_png(out / "window-head0.png")
                if not win_h0_ok:
                    problems.append("probe window not visible on head 0")
                guest_run(f"ferrion-wm msg action focus-window --id {wid}")
                guest_run("ferrion-wm msg action move-window-to-monitor-right")

                moved_ipc = False
                for _ in range(WINDOW_POLL_LIMIT):
                    wins = query_windows()
                    ws_by_id = {w["id"]: w.get("output") for w in query_workspaces()}
                    target = next((w for w in wins if w["id"] == wid), None)
                    if target is not None and ws_by_id.get(target.get("workspace_id")) == "Virtual-2":
                        moved_ipc = True
                        break
                    time.sleep(WINDOW_POLL_INTERVAL_S)
                win_h1, win_h1_ok = wait_size_and_pred(
                    vm, 1, HEAD_SIZES[1], lambda f: frame_diff_points(f, boot_h1) >= DIFF_MIN_POINTS
                )
                win_h1.save_png(out / "window-head1.png")
                if not moved_ipc:
                    problems.append("window did not move to Virtual-2 in IPC")
                if not win_h1_ok:
                    problems.append("moved window not visible on head 1 screendump")
                notes.append(f"move: ipc={moved_ipc} painted={win_h1_ok}")

            since_off = vm.mark()
            set_head_size(sockets[1], 0, 0)
            off_logged = vm.wait_serial(r"virtio-gpu: display 1 \(0x0px\)", 30, since_off)
            off_outputs, off_ipc_ok = poll_outputs(
                lambda o: "Virtual-2" not in o
                and output_current_size(o.get("Virtual-1", {})) == HEAD_SIZES[0]
            )
            record["phases"]["off_outputs"] = sorted(off_outputs.keys())
            off_h0, off_h0_ok = wait_size_and_pred(vm, 0, HEAD_SIZES[0], lambda _: True)
            off_h0.save_png(out / "off-head0.png")
            rescued = False
            if wid is not None:
                wins = query_windows()
                ws_by_id = {w["id"]: w.get("output") for w in query_workspaces()}
                target = next((w for w in wins if w["id"] == wid), None)
                rescued = target is not None and ws_by_id.get(target.get("workspace_id")) == "Virtual-1"
            if not off_logged:
                problems.append("guest did not log display 1 0x0px")
            if not off_ipc_ok:
                problems.append(f"Virtual-2 still present after hotplug off: {sorted(off_outputs.keys())}")
            if not off_h0_ok:
                problems.append(f"head 0 size drifted after hotplug off: {off_h0.width}x{off_h0.height}")
            if wid is not None and not rescued:
                problems.append("window not rescued to Virtual-1 after head 1 off")
            notes.append(f"off: logged={bool(off_logged)} ipc={off_ipc_ok} rescued={rescued}")

            since_on = vm.mark()
            set_head_size(sockets[1], *HOTPLUG_SIZE)
            on_logged = vm.wait_serial(
                rf"virtio-gpu: display 1 \({HOTPLUG_SIZE[0]}x{HOTPLUG_SIZE[1]}px\)", 30, since_on
            )
            on_outputs, on_ipc_ok = poll_outputs(
                lambda o: output_current_size(o.get("Virtual-1", {})) == HEAD_SIZES[0]
                and output_current_size(o.get("Virtual-2", {})) == HOTPLUG_SIZE
            )
            record["phases"]["on_outputs"] = {
                k: output_current_size(v) for k, v in sorted(on_outputs.items())
            }
            on_h1, on_h1_ok = wait_size_and_pred(vm, 1, HOTPLUG_SIZE, desktop_drawn)
            on_h0, on_h0_ok = wait_size_and_pred(vm, 0, HEAD_SIZES[0], lambda _: True)
            on_h1.save_png(out / "on-head1.png")
            on_h0.save_png(out / "on-head0.png")
            if not on_logged:
                problems.append(f"guest did not log display 1 {HOTPLUG_SIZE[0]}x{HOTPLUG_SIZE[1]}px")
            if not on_ipc_ok:
                problems.append(f"Virtual-2 did not return at {HOTPLUG_SIZE}: {record['phases']['on_outputs']}")
            if not on_h1_ok:
                problems.append(f"head 1 not drawn at {HOTPLUG_SIZE} after hotplug on (got {on_h1.width}x{on_h1.height})")
            if not on_h0_ok:
                problems.append(f"head 0 drifted after hotplug on (got {on_h0.width}x{on_h0.height})")
            notes.append(
                f"on: logged={bool(on_logged)} ipc={on_ipc_ok} "
                f"Virtual-2={on_h1.width}x{on_h1.height} ok={on_h1_ok}"
            )

            if wid is not None:
                guest_run(f"ferrion-wm msg action focus-window --id {wid}")
                guest_run("ferrion-wm msg action move-window-to-monitor-right")
                for _ in range(WINDOW_POLL_LIMIT):
                    wins = query_windows()
                    ws_by_id = {w["id"]: w.get("output") for w in query_workspaces()}
                    target = next((w for w in wins if w["id"] == wid), None)
                    if target is not None and ws_by_id.get(target.get("workspace_id")) == "Virtual-2":
                        break
                    time.sleep(WINDOW_POLL_INTERVAL_S)

            since_b2b = vm.mark()
            for gap_s in (0.0, 0.002, 0.005, 0.010):
                send_head_sizes_back_to_back(sockets[1], ((0, 0), BACK_TO_BACK_SIZE), gap_s=gap_s)
            b2b_logged = vm.wait_serial(
                rf"virtio-gpu: display 1 \({BACK_TO_BACK_SIZE[0]}x{BACK_TO_BACK_SIZE[1]}px\)",
                30,
                since_b2b,
            )
            b2b_outputs, b2b_ipc_ok = poll_outputs(
                lambda o: output_current_size(o.get("Virtual-1", {})) == HEAD_SIZES[0]
                and output_current_size(o.get("Virtual-2", {})) == BACK_TO_BACK_SIZE
            )
            record["phases"]["back_to_back_outputs"] = {
                k: output_current_size(v) for k, v in sorted(b2b_outputs.items())
            }
            b2b_h1, b2b_h1_ok = wait_size_and_pred(vm, 1, BACK_TO_BACK_SIZE, desktop_drawn)
            b2b_h1.save_png(out / "b2b-head1.png")
            if not b2b_logged:
                problems.append(
                    f"guest did not log display 1 {BACK_TO_BACK_SIZE[0]}x{BACK_TO_BACK_SIZE[1]}px after back-to-back hotplug"
                )
            if not b2b_ipc_ok:
                problems.append(
                    f"Virtual-2 mismatch after back-to-back off+on (expected {BACK_TO_BACK_SIZE}): "
                    f"{record['phases']['back_to_back_outputs']}"
                )
            if not b2b_h1_ok:
                problems.append(
                    f"head 1 not drawn at {BACK_TO_BACK_SIZE} after back-to-back off+on "
                    f"(got {b2b_h1.width}x{b2b_h1.height})"
                )
            notes.append(
                f"b2b: logged={bool(b2b_logged)} ipc={b2b_ipc_ok} "
                f"Virtual-2={b2b_h1.width}x{b2b_h1.height} ok={b2b_h1_ok}"
            )

            _, wm_log = run_status(
                vm, "bash -c 'cat /tmp/ferrion-wm.log 2>/dev/null || true'", 30
            )
            (out / "ferrion-wm.log").write_text(wm_log)
            _, probe_log = run_status(
                vm, "bash -c 'cat /tmp/wm-heads-probe.log 2>/dev/null || true'", 30
            )
            (out / "wm-heads-probe.log").write_text(probe_log)
            per_output = parse_per_output_frames(wm_log)
            record["per_output_frames"] = per_output

            errors = framebuffer_errors(vm)
            record["errors"] = errors
            if errors:
                problems.append(f"framebuffer errors: {errors}")

            (out / "wm-display-heads.json").write_text(json.dumps(record, indent=2) + "\n")
            timing_bits = ", ".join(
                f"{name}: n={stats['frame_count']} p50={stats['frame_us']['p50']}us p99={stats['frame_us']['p99']}us"
                for name, stats in sorted(per_output.items())
                if stats.get("frame_us")
            )
            if timing_bits:
                notes.append(f"frame_us({timing_bits})")
            if problems:
                return False, "; ".join(problems) + " | " + "; ".join(notes)
            return True, "; ".join(notes)
        finally:
            if not (out / "ferrion-wm.log").exists():
                try:
                    _, wm_log = run_status(
                        vm, "bash -c 'cat /tmp/ferrion-wm.log 2>/dev/null || true'", 15
                    )
                    (out / "ferrion-wm.log").write_text(wm_log)
                except Exception:
                    pass
            vm.close()
