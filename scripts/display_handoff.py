import json
import os
import re
import shlex
import socket
import struct
import tempfile
import threading
import time
from pathlib import Path

HANDOFF_COUNT = 60
HANDOFF_INTERVAL_S = 0.04
POINTER_INTERVAL_S = 0.005
MAX_POINTER_EVENTS = 4000
MAX_ANIM_ITERATIONS = 400
POST_HANDOFF_MOVES = 10
MIN_POST_HANDOFF_DISTINCT = 5
MAX_RECV_CHUNKS = 1024
MAX_QMP_REPLIES = 256
MAX_JSON_SCAN_MATCHES = 128
SESSION_POLL_LIMIT = 900
WINDOW_POLL_LIMIT = 150
RESIZE_WIDTH = 1024
RESIZE_HEIGHT = 768
BASE_WIDTH = 1280
BASE_HEIGHT = 800
QMP_COORD_MAX = 32767
RFB_MSG_SET_DESKTOP_SIZE = 251
RFB_ENC_EXTENDED_DESKTOP_SIZE = -308
RFB_ENC_RAW = 0


def parse_anim_log(text):
    in_window = False
    saw_start = False
    saw_end = False
    total_ok = 0
    total_fail = 0
    during_ok = 0
    during_fail = 0
    first_error = None
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if line == "HANDOFF_START":
            in_window = True
            saw_start = True
        elif line == "HANDOFF_END":
            in_window = False
            saw_end = True
        elif line.startswith("RESIZE_OK "):
            total_ok += 1
            if in_window:
                during_ok += 1
        elif line.startswith("RESIZE_FAIL "):
            total_fail += 1
            if in_window:
                during_fail += 1
            if first_error is None:
                first_error = line
    return {
        "saw_start": saw_start,
        "saw_end": saw_end,
        "total_ok": total_ok,
        "total_fail": total_fail,
        "during_ok": during_ok,
        "during_fail": during_fail,
        "first_error": first_error,
    }


def recv_exact(sock, count):
    assert count >= 0
    buf = bytearray()
    for _ in range(MAX_RECV_CHUNKS):
        if len(buf) >= count:
            return bytes(buf)
        chunk = sock.recv(count - len(buf))
        if not chunk:
            raise EOFError(f"short read from VNC socket ({len(buf)}/{count})")
        buf.extend(chunk)
    raise RuntimeError(f"VNC socket read exceeded {MAX_RECV_CHUNKS} chunks ({len(buf)}/{count})")


class VncResizeClient:
    def __init__(self, sock_path, timeout=15.0):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(str(sock_path))
        version = recv_exact(self.sock, 12)
        if not version.startswith(b"RFB "):
            raise RuntimeError(f"unexpected RFB banner: {version!r}")
        self.sock.sendall(b"RFB 003.008\n")
        sec_count = recv_exact(self.sock, 1)[0]
        if sec_count == 0:
            reason_len = struct.unpack("!I", recv_exact(self.sock, 4))[0]
            reason = recv_exact(self.sock, reason_len).decode("utf-8", "replace")
            raise RuntimeError(f"VNC security negotiation failed: {reason}")
        sec_types = recv_exact(self.sock, sec_count)
        if 1 not in sec_types:
            raise RuntimeError(f"VNC None auth not offered: {list(sec_types)}")
        self.sock.sendall(b"\x01")
        sec_result = struct.unpack("!I", recv_exact(self.sock, 4))[0]
        if sec_result != 0:
            raise RuntimeError(f"VNC security result {sec_result}")
        self.sock.sendall(b"\x01")
        init_hdr = recv_exact(self.sock, 24)
        self.width, self.height = struct.unpack("!HH", init_hdr[:4])
        name_len = struct.unpack("!I", init_hdr[20:24])[0]
        self.name = recv_exact(self.sock, name_len).decode("utf-8", "replace")
        self.sock.sendall(
            struct.pack("!BBHii", 2, 0, 2, RFB_ENC_EXTENDED_DESKTOP_SIZE, RFB_ENC_RAW)
        )

    def set_desktop_size(self, width, height):
        assert width > 0 and height > 0
        msg = struct.pack(
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
        self.sock.sendall(msg)
        self.width = width
        self.height = height

    def close(self):
        self.sock.close()


def parse_json_value(output, expect_type):
    decoder = json.JSONDecoder()
    marker = "[" if expect_type is list else "{"
    for idx, match in enumerate(re.finditer(re.escape(marker), output)):
        if idx >= MAX_JSON_SCAN_MATCHES:
            break
        try:
            value, _ = decoder.raw_decode(output[match.start():])
            if isinstance(value, expect_type):
                return value
        except ValueError:
            pass
    raise RuntimeError(f"no JSON {expect_type.__name__} in output: {output[-500:]}")


def run_display_handoff(args, out, vm_cls, root_shell, run_status):
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    results = {
        "load_before": list(os.getloadavg()),
        "handoffs_requested": HANDOFF_COUNT,
    }
    problems = []

    def guest_cmd(vm, cmd, timeout=60, check=True):
        status, output = run_status(
            vm,
            "bash -c " + shlex.quote("source /tmp/ferrion-wm-session; " + cmd),
            timeout,
        )
        if check and status != 0:
            raise RuntimeError(f"guest command failed ({status}): {cmd}: {output[-500:]}")
        return status, output

    with tempfile.TemporaryDirectory(prefix="ferrion-handoff-") as directory:
        vnc_path = Path(directory) / "vnc.sock"
        qmp_path = Path(directory) / "qmp.sock"
        vm = vm_cls(
            args,
            out,
            qemu_flags=f"-vnc unix:{vnc_path} -qmp unix:{qmp_path},server=on,wait=off",
            make=("gpu=virtio",),
        )
        qmp_sock = None
        qmp_stream = None
        try:
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
                return False, "serial root shell unavailable"
            mark = vm.mark()
            vm.type("bash --norc --noprofile --noediting\n")
            if not vm.wait_serial(r"bash-[\d.]+# ", args.timeout, mark):
                return False, "serial Bash prompt unavailable"

            deadline = time.monotonic() + args.timeout
            session_ready = False
            for _ in range(SESSION_POLL_LIMIT):
                st_sess, _ = run_status(vm, "test -s /tmp/ferrion-wm-session", args.timeout)
                if st_sess == 0:
                    session_ready = True
                    break
                if time.monotonic() >= deadline:
                    break
                time.sleep(0.2)
            if not session_ready:
                return False, "no /tmp/ferrion-wm-session"

            guest_cmd(vm, "desktop-layout-probe files > /tmp/handoff-files.log 2>&1 &")
            wid = None
            win_deadline = time.monotonic() + 30
            for _ in range(WINDOW_POLL_LIMIT):
                _, raw_wins = guest_cmd(vm, "ferrion-wm msg --json windows")
                wins = parse_json_value(raw_wins, list)
                if wins:
                    wid = wins[0]["id"]
                    break
                if time.monotonic() >= win_deadline:
                    break
                time.sleep(0.2)
            if wid is None:
                return False, "desktop-layout-probe window did not appear"

            guest_cmd(vm, "ferrion-wm msg action set-workspace-mode free")
            time.sleep(0.4)

            vnc = VncResizeClient(vnc_path, timeout=15.0)
            try:
                vnc.set_desktop_size(RESIZE_WIDTH, RESIZE_HEIGHT)
                time.sleep(1.5)
                st_out, raw_out = guest_cmd(vm, "ferrion-wm msg --json outputs", check=False)
                mode_ok = False
                if st_out == 0:
                    outputs = parse_json_value(raw_out, dict)
                    first_out = next(iter(outputs.values()))
                    mode0 = first_out["modes"][first_out["current_mode"]]
                    results["reconfigure_mode"] = mode0
                    results["reconfigure_logical"] = first_out.get("logical")
                    mode_ok = mode0["width"] == RESIZE_WIDTH and mode0["height"] == RESIZE_HEIGHT
                if not mode_ok:
                    problems.append(f"reconfigure did not report {RESIZE_WIDTH}x{RESIZE_HEIGHT}")

                guest_cmd(
                    vm,
                    "ferrion-wm msg action load-config-file --path /etc/ferrion-wm/rounded.kdl",
                    check=False,
                )
                guest_cmd(vm, "desktop-layout-probe editor > /tmp/handoff-editor.log 2>&1 &", check=False)
                guest_cmd(vm, "desktop-layout-probe terminal > /tmp/handoff-terminal.log 2>&1 &", check=False)
                time.sleep(0.8)
                guest_cmd(vm, "rm -f /tmp/handoff-anim.log /tmp/handoff-anim.stop", check=False)
                guest_cmd(
                    vm,
                    f"bash -c 'i=0; while [ $i -lt {MAX_ANIM_ITERATIONS} ] && [ ! -f /tmp/handoff-anim.stop ]; do "
                    f"i=$((i + 1)); ts=$(date +%s 2>/dev/null || echo 0); "
                    f"out=$(ferrion-wm msg action set-window-width --id {wid} $((400 + (i % 5) * 40)) 2>&1); rc=$?; "
                    f"if [ $rc -eq 0 ]; then printf \"RESIZE_OK %s %d\\n\" \"$ts\" \"$i\" >> /tmp/handoff-anim.log; "
                    f"else printf \"RESIZE_FAIL %s %d rc=%d %s\\n\" \"$ts\" \"$i\" \"$rc\" "
                    f"\"$(printf \"%s\" \"$out\" | tr \"\\r\\n\" \"  \")\" >> /tmp/handoff-anim.log; fi; "
                    f"sleep 0.01; done' > /tmp/handoff-anim-sh.log 2>&1 &",
                    check=False,
                )
                guest_cmd(vm, "echo HANDOFF_START >> /tmp/handoff-anim.log", check=False)

                ptr_stop = threading.Event()
                ptr_state = {"sent": 0, "error": None}

                def pump_pointer():
                    try:
                        for idx in range(MAX_POINTER_EVENTS):
                            if ptr_stop.is_set():
                                break
                            x_val = 2000 + ((idx * 317) % (QMP_COORD_MAX - 4000))
                            y_val = 2000 + ((idx * 193) % (QMP_COORD_MAX - 4000))
                            qmp_send(
                                "input-send-event",
                                {
                                    "events": [
                                        {"type": "abs", "data": {"axis": "x", "value": x_val}},
                                        {"type": "abs", "data": {"axis": "y", "value": y_val}},
                                    ]
                                },
                            )
                            ptr_state["sent"] = idx + 1
                            time.sleep(POINTER_INTERVAL_S)
                    except Exception as exc:
                        ptr_state["error"] = str(exc)

                ptr_thread = threading.Thread(target=pump_pointer, daemon=True)
                ptr_thread.start()
                handoffs_sent = 0
                try:
                    for step in range(HANDOFF_COUNT):
                        width, height = (
                            (BASE_WIDTH, BASE_HEIGHT)
                            if step % 2 == 0
                            else (RESIZE_WIDTH, RESIZE_HEIGHT)
                        )
                        vnc.set_desktop_size(width, height)
                        time.sleep(HANDOFF_INTERVAL_S)
                        handoffs_sent += 1
                finally:
                    ptr_stop.set()
                    ptr_thread.join(timeout=5.0)

                guest_cmd(
                    vm,
                    "echo HANDOFF_END >> /tmp/handoff-anim.log; touch /tmp/handoff-anim.stop",
                    check=False,
                )
                ptr_thread_alive = ptr_thread.is_alive()
                time.sleep(1.0)
                st_alive, _ = guest_cmd(vm, "ferrion-wm msg --json outputs", check=False)

                _, anim_log = run_status(vm, "cat /tmp/handoff-anim.log 2>/dev/null || true", 20)
                (out / "handoff-anim.log").write_text(anim_log)
                anim_stats = parse_anim_log(anim_log)

                post_cursor_events = 0
                post_cursor_distinct = 0
                if st_alive == 0 and not ptr_thread_alive:
                    _, lc_raw = run_status(vm, "wc -l < /tmp/ferrion-wm.log", 20)
                    nums = re.findall(r"\d+", lc_raw)
                    log_start = int(nums[-1]) if nums else 0
                    for step in range(POST_HANDOFF_MOVES):
                        px = 4000 + step * 2000
                        py = 4000 + step * 1800
                        qmp_send(
                            "input-send-event",
                            {
                                "events": [
                                    {"type": "abs", "data": {"axis": "x", "value": px}},
                                    {"type": "abs", "data": {"axis": "y", "value": py}},
                                ]
                            },
                        )
                        time.sleep(0.05)
                    time.sleep(0.4)
                    _, post_log = run_status(
                        vm, f"tail -n +{log_start + 1} /tmp/ferrion-wm.log", 20
                    )
                    coords = [
                        (int(x), int(y))
                        for x, y in re.findall(r"FERRION_CURSOR\b.*?x=(-?\d+)\s+y=(-?\d+)", post_log)
                    ]
                    post_cursor_events = len(coords)
                    post_cursor_distinct = len(set(coords))

                _, wm_log = run_status(vm, "cat /tmp/ferrion-wm.log", 20)
                (out / "ferrion-wm.log").write_text(wm_log)

                had_frame_fail = "frame presentation failed" in wm_log
                had_reader_fail = "input reader failed" in wm_log
                had_handoff_fail = "display handoff failed" in wm_log
                if ptr_thread_alive:
                    problems.append("pointer motion thread did not stop before post-handoff check")
                if st_alive != 0:
                    problems.append("ferrion-wm IPC unavailable after handoffs")
                if had_frame_fail:
                    problems.append("frame presentation failed logged in ferrion-wm.log")
                if had_reader_fail:
                    problems.append("input reader failed logged in ferrion-wm.log")
                if had_handoff_fail:
                    problems.append("display handoff failed logged in ferrion-wm.log")
                if ptr_state["error"] is not None:
                    problems.append(f"pointer motion error: {ptr_state['error']}")
                if ptr_state["sent"] == 0:
                    problems.append("no pointer events sent during handoffs")
                if not anim_stats["saw_start"] or not anim_stats["saw_end"]:
                    problems.append("missing HANDOFF_START/HANDOFF_END markers in handoff-anim.log")
                if anim_stats["during_ok"] == 0:
                    problems.append("no window resizes completed during handoff window")
                if post_cursor_distinct < MIN_POST_HANDOFF_DISTINCT:
                    problems.append(
                        f"post-handoff cursor moves {post_cursor_distinct} < {MIN_POST_HANDOFF_DISTINCT}"
                    )

                results.update(
                    {
                        "handoffs_sent": handoffs_sent,
                        "pointer_events_sent": ptr_state["sent"],
                        "pointer_error": ptr_state["error"],
                        "pointer_thread_alive": ptr_thread_alive,
                        "anim_resizes_during_handoff": anim_stats["during_ok"],
                        "anim_resizes_total": anim_stats["total_ok"],
                        "anim_failures_during_handoff": anim_stats["during_fail"],
                        "anim_failures_total": anim_stats["total_fail"],
                        "first_resize_error": anim_stats["first_error"],
                        "alive_after": st_alive == 0,
                        "had_frame_fail": had_frame_fail,
                        "had_reader_fail": had_reader_fail,
                        "had_handoff_fail": had_handoff_fail,
                        "post_handoff_cursor_events": post_cursor_events,
                        "post_handoff_cursor_distinct": post_cursor_distinct,
                        "problems": problems,
                    }
                )
            finally:
                vnc.close()
        finally:
            if qmp_stream is not None:
                qmp_stream.close()
            if qmp_sock is not None:
                qmp_sock.close()
            vm.close()

    results["load_after"] = list(os.getloadavg())
    (out / "display-handoff.json").write_text(json.dumps(results, indent=2) + "\n")
    if problems:
        return False, "; ".join(problems) + f"; see {out / 'display-handoff.json'}"
    return (
        True,
        f"{results['handoffs_sent']}/{HANDOFF_COUNT} handoffs with "
        f"{results['pointer_events_sent']} pointer events and "
        f"{results['anim_resizes_during_handoff']} in-window resizes survived, "
        f"0 presentation failures, {results['post_handoff_cursor_distinct']} distinct "
        f"post-handoff cursor moves; see {out / 'display-handoff.json'}",
    )
