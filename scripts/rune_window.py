"""RU3 check: live Rune CPU window, input, O3 scan, and GPU surface error under ferrion-wm as user."""
import base64
import hashlib
import io
import json
import re
import shlex
import time

LOGIN_TIMEOUT = 90
WM_START_TIMEOUT = 60
WINDOW_MAP_TIMEOUT = 45
SETTLE_TIMEOUT = 20
SETTLE_STILL_S = 0.5
INPUT_TIMEOUT = 20
GPU_ERROR_TIMEOUT = 30
FETCH_TIMEOUT = 120
POLL_STEP_S = 0.25
MAX_DIFF_SAMPLES = 16

HELLO_W = 320
HELLO_H = 200
HELLO_X = 100
HELLO_Y = 100
IDLE_POINTER_PX = (10, 10)

RU2_HELLO_PNG_SHA256 = "f9a74ca448e728c88b78413617e73caeab0d4e0a37da38086653d78a1e3f67f1"
RU2_HELLO_TREE_SHA256 = "53559263d4876e61e90dc3ffa0c867f7d967d3e4fd9a9ec0ba82513c0dfe1b35"


def context_owners(vm, run_status, pattern):
    _, output = run_status(vm, f"grep '{pattern}' /scheme/sys/context")
    return [int(m) for m in re.findall(rf"(?m)^\s*\d+\s+(\d+)\s.*{re.escape(pattern)}\b", output)]


def qmp_move_px(qmp, x, y, width, height):
    ax = max(0, min(32767, int((x + 0.5) * 32767 / max(1, width))))
    ay = max(0, min(32767, int((y + 0.5) * 32767 / max(1, height))))
    qmp("input-send-event", {"events": [
        {"type": "abs", "data": {"axis": "x", "value": ax}},
        {"type": "abs", "data": {"axis": "y", "value": ay}},
    ]})


def qmp_click_px(qmp, x, y, width, height):
    qmp_move_px(qmp, x, y, width, height)
    time.sleep(0.2)
    qmp("input-send-event", {"events": [
        {"type": "btn", "data": {"button": "left", "down": True}},
    ]})
    time.sleep(0.1)
    qmp("input-send-event", {"events": [
        {"type": "btn", "data": {"button": "left", "down": False}},
    ]})


def capture_settled(vm, timeout=SETTLE_TIMEOUT, still=SETTLE_STILL_S):
    deadline = time.monotonic() + timeout
    prev = vm.screenshot()
    while time.monotonic() < deadline:
        time.sleep(still)
        curr = vm.screenshot()
        if curr.width == prev.width and curr.height == prev.height and curr.rgb == prev.rgb:
            return curr
        prev = curr
    return None


def run_bash(vm, run_status, script, timeout=45):
    st, raw = run_status(vm, script, timeout)
    return st, raw.replace("\r\n", "\n")


def fetch_guest_bytes(vm, remote_path, tag):
    since = vm.mark()
    vm.type(
        f"echo RUNEWIN-\"\"{tag}-BEGIN; base64 -w 0 {shlex.quote(remote_path)} 2>/dev/null; "
        f"echo; echo RUNEWIN-\"\"{tag}-END\n"
    )
    found = vm.wait_serial(
        rf"RUNEWIN-{tag}-BEGIN\r?\n([A-Za-z0-9+/=\s]*?)RUNEWIN-{tag}-END",
        FETCH_TIMEOUT,
        since,
    )
    if not found:
        return None
    raw = "".join(found.group(1).split())
    if not raw:
        return None
    try:
        return base64.b64decode(raw, validate=True)
    except ValueError:
        return None


def crop_frame_rgb(frame, x, y, w, h):
    out = bytearray(w * h * 3)
    stride = frame.width * 3
    row_bytes = w * 3
    for row in range(h):
        src_off = (y + row) * stride + x * 3
        dst_off = row * row_bytes
        out[dst_off:dst_off + row_bytes] = frame.rgb[src_off:src_off + row_bytes]
    return bytes(out)


def compare_rgb_exact(w, h, live_rgb, ref_rgb):
    differing = 0
    max_diff = 0
    samples = []
    for idx in range(w * h):
        off = idx * 3
        d = max(
            abs(live_rgb[off] - ref_rgb[off]),
            abs(live_rgb[off + 1] - ref_rgb[off + 1]),
            abs(live_rgb[off + 2] - ref_rgb[off + 2]),
        )
        if d > 0:
            differing += 1
            if d > max_diff:
                max_diff = d
            if len(samples) < MAX_DIFF_SAMPLES:
                samples.append({
                    "x": idx % w,
                    "y": idx // w,
                    "live": list(live_rgb[off:off + 3]),
                    "ref": list(ref_rgb[off:off + 3]),
                })
    return {
        "size": [w, h],
        "total_pixels": w * h,
        "differing_pixels": differing,
        "max_channel_diff": max_diff,
        "samples": samples,
    }


def save_rgb_png(path, w, h, rgb):
    from PIL import Image
    Image.frombytes("RGB", (w, h), rgb).save(path)


def window_content_rect(win, screen_w, screen_h):
    layout = win.get("layout") or {}
    ws = layout.get("window_size") or [HELLO_W, HELLO_H]
    cw, ch = int(ws[0]), int(ws[1])
    pos = layout.get("tile_pos_in_workspace_view")
    off = layout.get("window_offset_in_tile") or [0, 0]
    if isinstance(pos, list) and len(pos) == 2:
        cx = int(round(pos[0] + off[0]))
        cy = int(round(pos[1] + off[1]))
    else:
        cx = HELLO_X + 1
        cy = HELLO_Y + 1
    cx = max(0, min(screen_w - cw, cx))
    cy = max(0, min(screen_h - ch, cy))
    return cx, cy, cw, ch


def run_rune_window(vm, args, helpers):
    try:
        from PIL import Image
    except ImportError as error:
        raise helpers["Blocked"](f"Pillow is required for rune-window comparison: {error}") from error

    root_shell = helpers["root_shell"]
    run_status = helpers["run_status"]
    sendkeys = helpers["sendkeys"]
    settle = helpers["settle"]
    qmp = helpers["qmp"]

    problems = []
    record = {
        "user_uid": None,
        "o3_scan": None,
        "hello": {},
        "input": {},
        "gpu_probe": {},
        "problems": problems,
    }

    def save_record():
        (vm.out / "rune-window.json").write_text(json.dumps(record, indent=2) + "\n")

    deadline = time.monotonic() + LOGIN_TIMEOUT
    login_frame = None
    while time.monotonic() < deadline:
        frame = vm.screenshot()
        if frame.kind() == "text":
            login_frame = settle(vm, 10, still=1.0)
            break
        time.sleep(0.5)
    if login_frame is None:
        vm.screenshot().save_png(vm.out / "no-text-login.png")
        return False, f"no text login on VT 3 within {LOGIN_TIMEOUT}s; see {vm.out}"

    if not root_shell(vm, LOGIN_TIMEOUT):
        return False, f"no root serial shell within {LOGIN_TIMEOUT}s; see {vm.out}"

    bash_mark = vm.mark()
    vm.type('exec env PS1="RUNEBASH# " bash --norc --noprofile\n')
    if not vm.wait_serial(r"RUNEBASH# ", 30, bash_mark):
        return False, f"could not switch root serial shell to bash; see {vm.out}"

    _, passwd_line = run_bash(vm, run_status, "grep '^user;' /etc/passwd")
    uid_match = re.search(r"user;(\d+);(\d+);[^;]*;([^;\r\n]+);", passwd_line)
    if not uid_match:
        return False, f"could not read user uid from /etc/passwd: {passwd_line!r}"
    user_uid = int(uid_match.group(1))
    runtime_dir = f"/var/run/user/{user_uid}"
    record["user_uid"] = user_uid

    sendkeys(vm.command, "user\n")

    wm_deadline = time.monotonic() + WM_START_TIMEOUT
    owners = []
    while time.monotonic() < wm_deadline:
        owners = context_owners(vm, run_status, "ferrion-wm")
        if user_uid in owners:
            break
        time.sleep(0.5)
    if user_uid not in owners:
        vm.screenshot().save_png(vm.out / "no-wm.png")
        return False, f"ferrion-wm not running as euid {user_uid} within {WM_START_TIMEOUT}s (saw {owners})"
    record["ferrion_wm_euids"] = owners

    wm_sock = None
    sock_deadline = time.monotonic() + WM_START_TIMEOUT
    while time.monotonic() < sock_deadline:
        st, out = run_bash(vm, run_status, f"ls -1 {runtime_dir}/*.sock 2>/dev/null | head -n 1")
        cand = out.strip().splitlines()[-1].strip() if out.strip() else ""
        if st == 0 and cand.endswith(".sock"):
            wm_sock = cand
            break
        time.sleep(0.5)
    if not wm_sock:
        _, rt_ls = run_bash(vm, run_status, f"ls -la {runtime_dir}")
        return False, f"no ferrion-wm IPC socket in {runtime_dir} within {WM_START_TIMEOUT}s ({rt_ls!r})"
    record["wm_socket"] = wm_sock

    screen_deadline = time.monotonic() + WM_START_TIMEOUT
    while time.monotonic() < screen_deadline:
        frame = vm.screenshot()
        if frame.has_ground_row() and frame.rgb != login_frame.rgb:
            break
        time.sleep(0.5)
    session_frame = capture_settled(vm, timeout=SETTLE_TIMEOUT, still=SETTLE_STILL_S)
    if session_frame is None:
        return False, f"session frame never settled within {SETTLE_TIMEOUT}s"
    screen_w, screen_h = session_frame.width, session_frame.height
    record["screen_size"] = [screen_w, screen_h]

    def wm_msg(subcmd, timeout=45):
        return run_bash(vm, run_status, f"FERRION_WM_SOCKET={shlex.quote(wm_sock)} ferrion-wm msg {subcmd}", timeout)

    def wm_spawn_sh(script):
        cmd = (
            f"FERRION_WM_SOCKET={shlex.quote(wm_sock)} "
            f"ferrion-wm msg action spawn -- /bin/bash -c {shlex.quote(script)}"
        )
        return run_bash(vm, run_status, cmd, 45)

    def wm_windows():
        st, out = wm_msg("--json windows")
        if st != 0:
            return []
        for match in re.finditer(r"\[", out):
            try:
                val, _ = json.JSONDecoder().raw_decode(out[match.start():])
                if isinstance(val, list) and all(isinstance(w, dict) and "id" in w for w in val):
                    return val
            except ValueError:
                pass
        return []

    def wait_for_file(path, marker, timeout):
        dl = time.monotonic() + timeout
        last = ""
        while time.monotonic() < dl:
            st, out = run_bash(vm, run_status, f"cat {shlex.quote(path)} 2>/dev/null")
            last = out
            if st == 0 and marker in out:
                return True, out
            time.sleep(POLL_STEP_S)
        return False, last

    def wait_for_window(title_match, timeout):
        dl = time.monotonic() + timeout
        while time.monotonic() < dl:
            for win in wm_windows():
                if (win.get("title") or "") == title_match:
                    return win
            time.sleep(POLL_STEP_S)
        return None

    o3_out_path = f"{runtime_dir}/o3-scan.txt"
    o3_done_path = f"{runtime_dir}/o3-scan.done"
    run_bash(vm, run_status, f"rm -f {o3_out_path} {o3_done_path}")
    wm_spawn_sh(f"wayland-deny-probe --scan > {o3_out_path} 2>&1; echo O3_DONE > {o3_done_path}")
    o3_ok, _ = wait_for_file(o3_done_path, "O3_DONE", WINDOW_MAP_TIMEOUT)
    _, o3_text = run_bash(vm, run_status, f"cat {o3_out_path} 2>/dev/null")
    (vm.out / "o3-scan.txt").write_text(o3_text)
    o3_core = re.findall(r"(?m)^CORE \d+ (\S+) \d+$", o3_text)
    o3_restricted = re.findall(r"(?m)^RESTRICTED \S+ \d+ (\S+) \d+$", o3_text)
    record["o3_scan"] = {"ok": o3_ok, "core": o3_core, "restricted": o3_restricted}
    if not o3_ok or o3_restricted or not {"wl_compositor", "wl_shm", "xdg_wm_base"}.issubset(set(o3_core)):
        problems.append(f"O3 scan failed: core={o3_core} restricted={o3_restricted}")

    hello_env = f"{runtime_dir}/hello-env.txt"
    hello_out = f"{runtime_dir}/hello.out"
    hello_err = f"{runtime_dir}/hello.err"
    run_bash(vm, run_status, f"rm -f {hello_env} {hello_out} {hello_err}")
    wm_spawn_sh(
        f"printf 'EUID=%s WAYLAND_DISPLAY=%s XDG_RUNTIME_DIR=%s\\n' \"$(id -u)\" \"$WAYLAND_DISPLAY\" \"$XDG_RUNTIME_DIR\" > {hello_env}; "
        f"exec env RUNE_GPU_BACKEND=cpu rune-hello --borderless --theme dark --log-switch > {hello_out} 2> {hello_err}"
    )

    hello_ready, hello_out_text = wait_for_file(hello_out, "HELLO_READY", WINDOW_MAP_TIMEOUT)
    hello_win = wait_for_window("Hello", WINDOW_MAP_TIMEOUT)
    _, hello_env_text = run_bash(vm, run_status, f"cat {hello_env} 2>/dev/null")
    _, hello_err_text = run_bash(vm, run_status, f"cat {hello_err} 2>/dev/null")
    (vm.out / "hello.out").write_text(hello_out_text)
    (vm.out / "hello.err").write_text(hello_err_text)
    hello_euids = context_owners(vm, run_status, "rune-hello")
    record["hello"]["ready"] = hello_ready
    record["hello"]["env"] = hello_env_text.strip()
    record["hello"]["euids"] = hello_euids
    if not hello_ready:
        problems.append(f"rune-hello did not print HELLO_READY within {WINDOW_MAP_TIMEOUT}s")
    if hello_win is None:
        problems.append(f"rune-hello window did not map within {WINDOW_MAP_TIMEOUT}s")
    if user_uid not in hello_euids or f"EUID={user_uid} " not in hello_env_text:
        problems.append(f"rune-hello not running as euid {user_uid}: euids={hello_euids} env={hello_env_text!r}")

    if hello_win is not None:
        hid = hello_win["id"]
        wm_msg(f"action move-window-to-floating --id {hid}")
        wm_msg(f"action set-window-width --id {hid} {HELLO_W}")
        wm_msg(f"action set-window-height --id {hid} {HELLO_H}")
        wm_msg(f"action move-floating-window --id {hid} --x {HELLO_X} --y {HELLO_Y}")
        wm_msg(f"action focus-window --id {hid}")
        qmp_move_px(qmp, IDLE_POINTER_PX[0], IDLE_POINTER_PX[1], screen_w, screen_h)
        time.sleep(0.5)

        hello_win_after = wait_for_window("Hello", 10) or hello_win
        record["hello"]["window"] = hello_win_after
        cx, cy, cw, ch = window_content_rect(hello_win_after, screen_w, screen_h)
        record["hello"]["content_rect"] = [cx, cy, cw, ch]

        live_frame = capture_settled(vm, timeout=SETTLE_TIMEOUT, still=SETTLE_STILL_S)
        if live_frame is None:
            problems.append(f"rune-hello frame did not settle within {SETTLE_TIMEOUT}s")
        else:
            live_frame.save_png(vm.out / "hello-live.png")
            live_crop_rgb = crop_frame_rgb(live_frame, cx, cy, cw, ch)
            save_rgb_png(vm.out / "hello-crop.png", cw, ch, live_crop_rgb)

            lvp_icd = f"/usr/share/vulkan/icd.d/lvp_icd.{args.arch}.json"
            ru2_png_path = f"{runtime_dir}/ru2-hello.png"
            ru2_tree_path = f"{runtime_dir}/ru2-hello.tree.json"
            ref_png_path = f"{runtime_dir}/ref-hello.png"
            ref_tree_path = f"{runtime_dir}/ref-hello.tree.json"
            ref_done_path = f"{runtime_dir}/ref-hello.done"
            run_bash(vm, run_status, f"rm -f {ru2_png_path} {ru2_tree_path} {ref_png_path} {ref_tree_path} {ref_done_path}")
            wm_spawn_sh(
                f"VK_DRIVER_FILES={lvp_icd} rune-hello --snap {ru2_png_path} --tree {ru2_tree_path} --at 0 --scale 2 --theme dark > {runtime_dir}/ref-ru2.log 2>&1; "
                f"R1=$?; "
                f"VK_DRIVER_FILES={lvp_icd} rune-hello --snap {ref_png_path} --tree {ref_tree_path} --at 5 --scale 1 --theme dark --size {cw}x{ch} > {runtime_dir}/ref-s1.log 2>&1; "
                f"R2=$?; printf 'REF_DONE %s %s\\n' \"$R1\" \"$R2\" > {ref_done_path}"
            )
            ref_ok, ref_done_text = wait_for_file(ref_done_path, "REF_DONE", 180)
            record["hello"]["ref_done"] = ref_done_text.strip()
            ru2_png_bytes = fetch_guest_bytes(vm, ru2_png_path, "RU2PNG") if ref_ok else None
            ru2_tree_bytes = fetch_guest_bytes(vm, ru2_tree_path, "RU2TREE") if ref_ok else None
            ref_png_bytes = fetch_guest_bytes(vm, ref_png_path, "REFPNG") if ref_ok else None
            ref_tree_bytes = fetch_guest_bytes(vm, ref_tree_path, "REFTREE") if ref_ok else None

            if ru2_png_bytes and ru2_tree_bytes:
                got_p = hashlib.sha256(ru2_png_bytes).hexdigest()
                got_t = hashlib.sha256(ru2_tree_bytes).hexdigest()
                record["hello"]["ru2_png_sha256"] = got_p
                record["hello"]["ru2_tree_sha256"] = got_t
                if got_p != RU2_HELLO_PNG_SHA256:
                    problems.append(f"in-guest RU2 hello PNG digest mismatch: {got_p} != {RU2_HELLO_PNG_SHA256}")
                if got_t != RU2_HELLO_TREE_SHA256:
                    problems.append(f"in-guest RU2 hello tree digest mismatch: {got_t} != {RU2_HELLO_TREE_SHA256}")
            else:
                problems.append("failed to render/fetch in-guest RU2 reference")

            ref_tree = None
            if ref_png_bytes and ref_tree_bytes:
                (vm.out / "hello-ref.png").write_bytes(ref_png_bytes)
                (vm.out / "hello-ref.tree.json").write_bytes(ref_tree_bytes)
                ref_tree = json.loads(ref_tree_bytes.decode("utf-8"))
                ref_img = Image.open(io.BytesIO(ref_png_bytes)).convert("RGB")
                if (ref_img.width, ref_img.height) != (cw, ch):
                    problems.append(f"ref PNG size {ref_img.width}x{ref_img.height} != window {cw}x{ch}")
                else:
                    px_cmp = compare_rgb_exact(cw, ch, live_crop_rgb, ref_img.tobytes())
                    record["hello"]["pixel_compare"] = px_cmp
                    if px_cmp["differing_pixels"] != 0:
                        problems.append(
                            f"hello live vs ref diff {px_cmp['differing_pixels']}/{px_cmp['total_pixels']} "
                            f"(max_ch={px_cmp['max_channel_diff']})"
                        )
            else:
                problems.append("failed to render/fetch scale-1 hello reference")

            wm_msg(f"action focus-window --id {hid}")
            time.sleep(0.2)
            vm.command("sendkey tab")
            time.sleep(0.2)
            vm.command("sendkey spc")
            key_ok, out_after_key = wait_for_file(hello_out, "HELLO_SWITCH state=on", INPUT_TIMEOUT)
            record["input"]["key_toggle_logged"] = key_ok
            if not key_ok:
                problems.append(f"Tab+Space did not log HELLO_SWITCH state=on within {INPUT_TIMEOUT}s")
            else:
                qmp_move_px(qmp, IDLE_POINTER_PX[0], IDLE_POINTER_PX[1], screen_w, screen_h)
                on_frame = capture_settled(vm, timeout=SETTLE_TIMEOUT, still=SETTLE_STILL_S)
                if on_frame is None:
                    problems.append("hello on-state frame did not settle")
                else:
                    on_frame.save_png(vm.out / "hello-on.png")
                    on_crop = crop_frame_rgb(on_frame, cx, cy, cw, ch)
                    on_diff = compare_rgb_exact(cw, ch, on_crop, live_crop_rgb)
                    record["input"]["key_changed_pixels"] = on_diff["differing_pixels"]
                    if on_diff["differing_pixels"] == 0:
                        problems.append("Tab+Space changed 0 pixels in hello window")

                switch_x, switch_y = cx + cw // 2, cy + int(ch * 0.58)
                if ref_tree is not None:
                    for el in ref_tree.get("elements", []):
                        if el.get("key") == "switch" and isinstance(el.get("rect"), list) and len(el["rect"]) == 4:
                            rx, ry, rw, rh = el["rect"]
                            switch_x = cx + int(round(rx + rw / 2.0))
                            switch_y = cy + int(round(ry + rh / 2.0))
                            break
                record["input"]["switch_click_px"] = [switch_x, switch_y]
                qmp_click_px(qmp, switch_x, switch_y, screen_w, screen_h)
                click_ok, out_after_click = wait_for_file(hello_out, "HELLO_SWITCH state=off", INPUT_TIMEOUT)
                (vm.out / "hello.out").write_text(out_after_click)
                record["input"]["click_toggle_logged"] = click_ok
                if not click_ok:
                    problems.append(f"pointer click at ({switch_x},{switch_y}) did not log HELLO_SWITCH state=off within {INPUT_TIMEOUT}s")
                else:
                    qmp_move_px(qmp, IDLE_POINTER_PX[0], IDLE_POINTER_PX[1], screen_w, screen_h)
                    off_frame = capture_settled(vm, timeout=SETTLE_TIMEOUT, still=SETTLE_STILL_S)
                    if off_frame is not None:
                        off_frame.save_png(vm.out / "hello-off.png")
                        off_crop = crop_frame_rgb(off_frame, cx, cy, cw, ch)
                        if on_frame is not None:
                            back_diff = compare_rgb_exact(cw, ch, off_crop, on_crop)
                            record["input"]["click_changed_pixels"] = back_diff["differing_pixels"]
                            if back_diff["differing_pixels"] == 0:
                                problems.append("pointer click changed 0 pixels in hello window")

        wm_msg(f"action close-window --id {hid}")
        time.sleep(0.5)

    gpu_out = f"{runtime_dir}/gpu-hello.out"
    gpu_err = f"{runtime_dir}/gpu-hello.err"
    gpu_done = f"{runtime_dir}/gpu-hello.done"
    run_bash(vm, run_status, f"rm -f {gpu_out} {gpu_err} {gpu_done}")
    wm_spawn_sh(
        f"rune-hello --borderless --theme dark > {gpu_out} 2> {gpu_err}; "
        f"printf 'GPU_EXIT=%s\\n' \"$?\" > {gpu_done}"
    )
    gpu_finished, gpu_done_text = wait_for_file(gpu_done, "GPU_EXIT=", GPU_ERROR_TIMEOUT)
    _, gpu_err_text = run_bash(vm, run_status, f"cat {gpu_err} 2>/dev/null")
    _, gpu_out_text = run_bash(vm, run_status, f"cat {gpu_out} 2>/dev/null")
    (vm.out / "gpu-hello.err").write_text(gpu_err_text)
    (vm.out / "gpu-hello.out").write_text(gpu_out_text)
    gpu_exit_m = re.search(r"GPU_EXIT=(\d+)", gpu_done_text)
    gpu_exit = int(gpu_exit_m.group(1)) if gpu_exit_m else None
    record["gpu_probe"] = {
        "finished": gpu_finished,
        "exit": gpu_exit,
        "stderr": gpu_err_text.strip(),
        "stdout": gpu_out_text.strip(),
    }
    if not gpu_finished:
        gpu_win = wait_for_window("Hello", 3)
        if gpu_win is not None:
            record["gpu_probe"]["mapped_window"] = gpu_win
            wm_msg(f"action close-window --id {gpu_win['id']}")
        problems.append(f"GPU rune-hello did not exit with defined error within {GPU_ERROR_TIMEOUT}s")
    else:
        if gpu_exit != 1:
            problems.append(f"GPU rune-hello exited {gpu_exit}, expected 1")
        if "panicked at" in gpu_err_text:
            problems.append(f"GPU rune-hello panicked: {gpu_err_text.strip()!r}")
        if not gpu_err_text.strip():
            problems.append("GPU rune-hello exited nonzero with empty stderr")

    gpu_dbg_out = f"{runtime_dir}/gpu-hello-debug.out"
    gpu_dbg_err = f"{runtime_dir}/gpu-hello-debug.err"
    gpu_dbg_done = f"{runtime_dir}/gpu-hello-debug.done"
    run_bash(vm, run_status, f"rm -f {gpu_dbg_out} {gpu_dbg_err} {gpu_dbg_done}")
    wm_spawn_sh(
        f"WAYLAND_DEBUG=client rune-hello --borderless --theme dark > {gpu_dbg_out} 2> {gpu_dbg_err}; "
        f"printf 'GPU_DBG_EXIT=%s\\n' \"$?\" > {gpu_dbg_done}"
    )
    gpu_dbg_finished, gpu_dbg_done_text = wait_for_file(gpu_dbg_done, "GPU_DBG_EXIT=", GPU_ERROR_TIMEOUT)
    gpu_dbg_bytes = fetch_guest_bytes(vm, gpu_dbg_err, "GPUDBG") if gpu_dbg_finished else None
    if gpu_dbg_bytes is not None:
        gpu_dbg_err_text = gpu_dbg_bytes.decode("utf-8", errors="replace")
    else:
        _, gpu_dbg_err_text = run_bash(vm, run_status, f"cat {gpu_dbg_err} 2>/dev/null")
    (vm.out / "gpu-hello-debug.err").write_text(gpu_dbg_err_text)
    gpu_dbg_m = re.search(r"GPU_DBG_EXIT=(\d+)", gpu_dbg_done_text)
    gpu_dbg_exit = int(gpu_dbg_m.group(1)) if gpu_dbg_m else None
    record["gpu_probe"]["wayland_debug_exit"] = gpu_dbg_exit
    if not gpu_dbg_finished or gpu_dbg_exit != 1:
        problems.append(f"WAYLAND_DEBUG GPU rune-hello exited {gpu_dbg_exit}, expected 1")

    save_record()
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'rune-window.json'}"
    px = record["hello"].get("pixel_compare", {})
    return (
        True,
        f"euid={user_uid} o3=0-restricted hello_diff={px.get('differing_pixels')}/{px.get('total_pixels')} "
        f"key+click=ok gpu_exit={record['gpu_probe'].get('exit')}; see {vm.out / 'rune-window.json'}",
    )
