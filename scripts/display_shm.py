"""Checks session-private shm prefix: GTK 4 Wayland window under ferrion-run --display and shm isolation."""
import json
import re
import time

import gtk4_window

MIN_WINDOW_PIXELS = 40000
PROBE_POLL_ITERS = 60
PROBE_EXIT_POLL_ITERS = 30
WAYLAND_SOCKET_WAIT_ITERS = 30
PEER_READY_POLL_ITERS = 30


def run_display_shm(vm, args, helpers):
    """Checks that a sandboxed app's shm objects live in a private prefix and it cannot reach any object outside it (outside object, another session's object with the same name, or audio buffers), and that GTK 4 Wayland windowing works under ferrion-run --display."""
    sendkeys = helpers["sendkeys"]
    settle = helpers["settle"]

    record = {"renderer_requested": "cairo", "backend_requested": "wayland"}

    deadline = time.monotonic() + args.timeout
    login_frame = None
    while time.monotonic() < deadline:
        frame = vm.screenshot()
        if frame.kind() == "text":
            login_frame = settle(vm, 10, still=1.5)
            break
        time.sleep(0.5)
    if login_frame is None:
        vm.screenshot().save_png(vm.out / "no-text-login.png")
        return False, f"no text login on VT 3 within {args.timeout}s; see {vm.out}"
    login_frame.save_png(vm.out / "login.png")

    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s; see {vm.out}"
    vm.type("user\n")
    time.sleep(3)

    seq = 0
    timed_out = False

    def run_user(line, timeout=45):
        nonlocal seq, timed_out
        if timed_out:
            return None, ""
        assert "'" not in line, f"single quote disallowed in bash -c payload: {line!r}"
        seq += 1
        tag = seq
        since = vm.mark()
        vm.type(f"bash -c '{line}; echo ds-end-{tag}-$((40+2))-$?'\n")
        found = vm.wait_serial(rf"(?s)\n(.*)ds-end-{tag}-42-(\d+)", timeout, since)
        if not found:
            timed_out = True
            return None, vm.text(since)
        return int(found.group(2)), found.group(1)

    code, _ = run_user("command -v gtk4-window-probe && command -v ferrion-run-probe", timeout=30)
    if code != 0:
        return False, f"gtk4-window-probe or ferrion-run-probe missing in guest; see {vm.out}"

    rc_uid, uid_out = run_user('echo "UID:$(id -u)"', timeout=20)
    uid_match = re.search(r"UID:(\d+)", uid_out)
    if rc_uid != 0 or not uid_match:
        return False, f"could not determine user uid: rc={rc_uid} out={uid_out!r}"
    user_uid = int(uid_match.group(1))
    runtime_dir = f"/var/run/user/{user_uid}"
    record["user_uid"] = user_uid

    sendkeys(vm.command, "user\n")

    wayland_sock = None
    for _ in range(WAYLAND_SOCKET_WAIT_ITERS):
        rc_s, out_s = run_user(
            f'for p in {runtime_dir}/wayland-*; do '
            f'[ -e "$p" ] || continue; b=$(basename "$p"); '
            f'case "$b" in *.lock) ;; wayland-[0-9]*) echo "SOCK:$b"; break;; esac; done',
            timeout=15,
        )
        m_sock = re.search(r"SOCK:(wayland-[0-9]+)\b", out_s)
        if rc_s == 0 and m_sock:
            wayland_sock = m_sock.group(1)
            break
        time.sleep(1.0)
    if not wayland_sock:
        vm.screenshot().save_png(vm.out / "no-wayland-socket.png")
        return False, f"ferrion-wm did not create a Wayland socket in {runtime_dir}; see {vm.out}"
    record["wayland_display"] = wayland_sock

    session_frame = settle(vm, 10, still=1.0)
    session_frame.save_png(vm.out / "session.png")

    blue_pal = gtk4_window.rgb_matches(None, (0x24, 0x68, 0xAC))
    green_pal = gtk4_window.rgb_matches(None, (0x22, 0xAA, 0x44))

    probe_log = "/tmp/dshm-gtk4.log"
    probe_rc = "/tmp/dshm-gtk4.rc"
    launch_cmd = (
        f"rm -f {probe_log} {probe_rc}; "
        f"(XDG_RUNTIME_DIR={runtime_dir} WAYLAND_DISPLAY={wayland_sock} "
        f"GDK_BACKEND=wayland GSK_RENDERER=cairo "
        f"ferrion-run --display --no-network --env GDK_BACKEND --env GSK_RENDERER "
        f"-- gtk4-window-probe </dev/null 2>&1 | cat > {probe_log}; "
        f"echo \"${{PIPESTATUS[0]}}\" > {probe_rc}) </dev/null >/dev/null 2>&1 & true"
    )
    rc_l, out_l = run_user(launch_cmd, timeout=20)
    if rc_l != 0:
        return False, f"failed to start sandboxed gtk4-window-probe (rc={rc_l}): {out_l!r}; see {vm.out}"

    blue_matched = False
    blue_pixels = 0
    log_text = ""
    early_rc = None
    for _ in range(PROBE_POLL_ITERS):
        _, log_text = run_user(f"cat {probe_log} 2>/dev/null || true", timeout=15)
        frame = vm.screenshot()
        blue_pixels = gtk4_window.count_color(frame, blue_pal)
        if blue_pixels >= MIN_WINDOW_PIXELS and "GTK4-WINDOW: display=" in log_text:
            blue_matched = True
            frame.save_png(vm.out / "gtk4-blue.png")
            break
        rc_done, _ = run_user(f"test -f {probe_rc}", timeout=10)
        if rc_done == 0:
            _, log_text = run_user(f"cat {probe_log} 2>/dev/null || true", timeout=15)
            _, rc_text = run_user(f"cat {probe_rc} 2>/dev/null || true", timeout=10)
            early_rc = rc_text.strip()
            record["early_exit_rc"] = early_rc
            frame.save_png(vm.out / "gtk4-early-exit.png")
            break
        time.sleep(0.5)

    record["blue_pixels"] = blue_pixels
    record["window_log_initial"] = log_text
    if not blue_matched:
        (vm.out / "display-shm.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        return False, (
            f"sandboxed GTK4 window did not paint blue (rc={early_rc}, blue_px={blue_pixels}); "
            f"log={log_text!r}; see {vm.out}"
        )

    m = re.search(r"GTK4-WINDOW: display=(\S+) type=(\S+) renderer=(\S+)", log_text)
    if not m:
        return False, f"missing GTK4-WINDOW display/renderer line: {log_text!r}"
    record["display"] = m.group(1)
    record["display_type"] = m.group(2)
    record["renderer_type"] = m.group(3)
    if m.group(2) != "GdkWaylandDisplay":
        return False, f"GDK fell back from Wayland: type={m.group(2)}"
    if m.group(3) != "GskCairoRenderer":
        return False, f"GSK did not use CPU Cairo renderer: renderer={m.group(3)}"

    vm.command("sendkey k")
    green_matched = False
    green_pixels = 0
    for _ in range(PROBE_POLL_ITERS):
        _, log_text = run_user(f"cat {probe_log} 2>/dev/null || true", timeout=15)
        frame = vm.screenshot()
        green_pixels = gtk4_window.count_color(frame, green_pal)
        if green_pixels >= MIN_WINDOW_PIXELS and "GTK4-WINDOW: key=k" in log_text:
            green_matched = True
            frame.save_png(vm.out / "gtk4-green.png")
            break
        time.sleep(0.5)

    record["green_pixels"] = green_pixels
    if not green_matched:
        frame.save_png(vm.out / "gtk4-no-key.png")
        record["window_log_after_key"] = log_text
        (vm.out / "display-shm.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        return False, f"key k did not reach sandboxed GTK4 window (green_px={green_pixels}); log={log_text!r}; see {vm.out}"

    vm.command("sendkey q")
    exit_code = None
    for _ in range(PROBE_EXIT_POLL_ITERS):
        rc_f, out_f = run_user(f"cat {probe_rc} 2>/dev/null || true", timeout=10)
        lines = [line.strip() for line in out_f.splitlines() if line.strip().isdigit()]
        if rc_f == 0 and lines:
            exit_code = int(lines[-1])
            break
        time.sleep(0.5)
    _, final_log = run_user(f"cat {probe_log} 2>/dev/null || true", timeout=15)
    record["window_exit_code"] = exit_code
    record["window_log_final"] = final_log
    if exit_code != 0 or "GTK4-WINDOW: closed" not in final_log:
        (vm.out / "display-shm.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        return False, f"sandboxed GTK4 window did not exit 0 cleanly (rc={exit_code}); log={final_log!r}"
    if "GLib-GIO-CRITICAL" in final_log:
        (vm.out / "display-shm.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        return False, f"sandboxed GTK4 window logged GLib-GIO-CRITICAL: {final_log!r}"

    closed = settle(vm, 5, still=1.0)
    closed.save_png(vm.out / "gtk4-closed.png")
    rem_green = gtk4_window.count_color(closed, green_pal)
    rem_blue = gtk4_window.count_color(closed, blue_pal)
    record["remaining_pixels_after_close"] = {"blue": rem_blue, "green": rem_green}
    if rem_green > 200 or rem_blue > 200:
        return False, f"sandboxed GTK4 window left pixels after exit: blue={rem_blue}, green={rem_green}"

    problems = []
    rc_out, out_create = run_user(
        "ferrion-run-probe shm-create /pfr-secret PFR-SHM-SECRET && ferrion-run-probe shm-exists /pfr-secret",
        timeout=30,
    )
    record["outside_create"] = {"rc": rc_out, "output": out_create}
    if rc_out != 0 or "SHM_CREATED /pfr-secret" not in out_create or "SHM_EXISTS /pfr-secret OK" not in out_create:
        problems.append(f"outside shm positive control failed (rc={rc_out})")

    peer_log = "/tmp/dshm-peer.log"
    rc_p1, _ = run_user(
        f"rm -f {peer_log}; "
        f"(ferrion-run --no-network -- ferrion-run-probe shm-create /same-name SESS1-SECRET 12 "
        f"</dev/null 2>&1 | cat > {peer_log}) </dev/null >/dev/null 2>&1 & true",
        timeout=20,
    )
    peer_ready = False
    peer_out = ""
    if rc_p1 == 0:
        for _ in range(PEER_READY_POLL_ITERS):
            _, peer_out = run_user(f"cat {peer_log} 2>/dev/null || true", timeout=10)
            if "SHM_CREATED /same-name" in peer_out:
                peer_ready = True
                break
            time.sleep(0.25)
    record["peer_session"] = {"rc": rc_p1, "ready": peer_ready, "output": peer_out}
    if not peer_ready:
        problems.append(f"peer session did not create /same-name: {peer_out!r}")

    rc_chk, chk_out = run_user(
        "ferrion-run --no-network -- ferrion-run-probe shm-check /pfr-secret /same-name /gdk-wayland-probe </dev/null 2>&1 | cat",
        timeout=45,
    )
    record["shm_check"] = {"rc": rc_chk, "output": chk_out}
    if rc_chk != 0:
        problems.append(f"shm-check exited {rc_chk}")
    expected_lines = (
        ("outside shm_open", "SHM_OUTSIDE_API ERR 2"),
        ("outside raw open", "SHM_OUTSIDE_RAW ERR 2"),
        ("peer session shm_open", "SHM_PEER ERR 2"),
        ("audio shm buffer", "SHM_AUDIO_SHM ERR 2"),
        ("own shm create/mmap", "SHM_OWN_CREATE OK"),
        ("own shm O_EXCL EEXIST", "SHM_OWN_EXCL ERR 17"),
        ("own shm_unlink", "SHM_OWN_UNLINK OK"),
        ("own shm post-unlink ENOENT", "SHM_OWN_AFTER_UNLINK ERR 2"),
    )
    for label, token in expected_lines:
        if token not in chk_out:
            problems.append(f"missing {label} ({token})")
    if not re.search(r"SHM_AUDIOHW ERR (?:2|19)\b", chk_out):
        problems.append("audiohw buffer check did not return ENOENT/ENODEV")

    run_user("ferrion-run-probe shm-unlink /pfr-secret 2>/dev/null || true", timeout=15)

    (vm.out / "display-shm.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out}"

    return True, (
        f"sandboxed GTK4/Adwaita ({record['renderer_type']}): blue={blue_pixels}px, "
        f"key k->green={green_pixels}px, exit 0; "
        f"shm private prefix: outside ENOENT (api+raw), peer session ENOENT, audio buffers unreachable, "
        f"O_EXCL EEXIST, shm_unlink->ENOENT; see {vm.out}"
    )
