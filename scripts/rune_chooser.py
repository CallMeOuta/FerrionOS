"""RU12a check: out-of-process /usr/libexec/ferrion-file-chooser under user-owned ferrion-wm."""
import json
import re
import shlex
import time

from rune_window import capture_settled, context_owners, run_bash

LOGIN_TIMEOUT = 90
WM_START_TIMEOUT = 60
WINDOW_MAP_TIMEOUT = 45
SETTLE_TIMEOUT = 20
SETTLE_STILL_S = 0.5
KEY_STEP_TIMEOUT = 20
REAP_TIMEOUT = 10
KILL_EOF_TIMEOUT = 15
POLL_STEP_S = 0.25

CHOOSER_PATH = "/usr/libexec/ferrion-file-chooser"
CHOOSER_PROC_NAME = CHOOSER_PATH[:32]
FORBIDDEN_BIN_PATH = "/usr/bin/ferrion-file-chooser"
DEMO_PATH = "/usr/bin/rune-chooser-demo"
CHOOSER_APP_ID = "org.ferrionos.FileChooser"
DEMO_APP_ID = "org.ferrionos.ChooserDemo"


def context_entries(vm, run_status, pattern):
    _, out = run_bash(vm, run_status, f"grep -F {shlex.quote(pattern)} /scheme/sys/context 2>/dev/null || true")
    rows = []
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 11 and parts[0].isdigit() and parts[1].isdigit():
            name = parts[-1]
            if pattern in name:
                rows.append({"pid": int(parts[0]), "euid": int(parts[1]), "name": name})
    return rows


def ps_entries(vm, run_status, pattern):
    _, out = run_bash(vm, run_status, f"grep -F {shlex.quote(pattern)} /scheme/proc/ps 2>/dev/null || true")
    rows = []
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 11 and parts[0].isdigit() and parts[2].isdigit() and parts[6].isdigit():
            name = " ".join(parts[10:])
            if pattern in name:
                rows.append({
                    "pid": int(parts[0]),
                    "ppid": int(parts[2]),
                    "euid": int(parts[6]),
                    "status": parts[9],
                    "name": name,
                })
    return rows


def run_rune_chooser(vm, args, helpers):
    root_shell = helpers["root_shell"]
    run_status = helpers["run_status"]
    sendkeys = helpers["sendkeys"]
    settle = helpers["settle"]

    problems = []
    record = {
        "timeouts": {
            "login": LOGIN_TIMEOUT,
            "wm_start": WM_START_TIMEOUT,
            "window_map": WINDOW_MAP_TIMEOUT,
            "key_step": KEY_STEP_TIMEOUT,
            "reap": REAP_TIMEOUT,
            "kill_eof": KILL_EOF_TIMEOUT,
        },
        "install_check": {},
        "user_uid": None,
        "o3_scan": None,
        "fixture": {},
        "demo": {},
        "steps": {},
        "kill_eof": {},
        "problems": problems,
    }

    def save_record():
        (vm.out / "rune-chooser.json").write_text(json.dumps(record, indent=2) + "\n")

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

    st_ch, _ = run_bash(vm, run_status, f"test -x {shlex.quote(CHOOSER_PATH)}")
    st_no_bin, _ = run_bash(vm, run_status, f"test ! -e {shlex.quote(FORBIDDEN_BIN_PATH)}")
    st_demo, _ = run_bash(vm, run_status, f"test -x {shlex.quote(DEMO_PATH)}")
    record["install_check"] = {
        "libexec_present": st_ch == 0,
        "usr_bin_absent": st_no_bin == 0,
        "demo_present": st_demo == 0,
    }
    if st_ch != 0 or st_no_bin != 0 or st_demo != 0:
        problems.append(
            f"install layout check failed: libexec={st_ch == 0} usr_bin_absent={st_no_bin == 0} demo={st_demo == 0}"
        )
        save_record()
        return False, "; ".join(problems) + f"; see {vm.out / 'rune-chooser.json'}"

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
        return False, f"no ferrion-wm IPC socket in {runtime_dir} within {WM_START_TIMEOUT}s"
    record["wm_socket"] = wm_sock

    screen_deadline = time.monotonic() + WM_START_TIMEOUT
    while time.monotonic() < screen_deadline:
        frame = vm.screenshot()
        if frame.has_ground_row() and frame.rgb != login_frame.rgb:
            break
        time.sleep(0.5)
    if capture_settled(vm, timeout=SETTLE_TIMEOUT, still=SETTLE_STILL_S) is None:
        return False, f"session frame never settled within {SETTLE_TIMEOUT}s"

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

    def wait_for_window(app_id, timeout):
        dl = time.monotonic() + timeout
        while time.monotonic() < dl:
            for win in wm_windows():
                if (win.get("app_id") or "") == app_id:
                    return win
            time.sleep(POLL_STEP_S)
        return None

    def wait_chooser_reaped(timeout):
        dl = time.monotonic() + timeout
        while time.monotonic() < dl:
            ctx = context_entries(vm, run_status, CHOOSER_PROC_NAME)
            ps_rows = ps_entries(vm, run_status, CHOOSER_PROC_NAME)
            live_ps = [r for r in ps_rows if r["status"] != "X"]
            wins = [w for w in wm_windows() if (w.get("app_id") or "") == CHOOSER_APP_ID]
            if not ctx and not live_ps and not wins:
                return True, {"context": ctx, "ps": ps_rows, "live_ps": live_ps, "windows": wins}
            time.sleep(POLL_STEP_S)
        ps_rows = ps_entries(vm, run_status, CHOOSER_PROC_NAME)
        return False, {
            "context": context_entries(vm, run_status, CHOOSER_PROC_NAME),
            "ps": ps_rows,
            "live_ps": [r for r in ps_rows if r["status"] != "X"],
            "windows": [w for w in wm_windows() if (w.get("app_id") or "") == CHOOSER_APP_ID],
        }

    def wait_for_focused(app_id, timeout):
        dl = time.monotonic() + timeout
        last = None
        while time.monotonic() < dl:
            for win in wm_windows():
                if (win.get("app_id") or "") == app_id:
                    last = win
                    if win.get("is_focused"):
                        return win
            time.sleep(POLL_STEP_S)
        return last

    o3_out = f"{runtime_dir}/o3-chooser.txt"
    o3_done = f"{runtime_dir}/o3-chooser.done"
    run_bash(vm, run_status, f"rm -f {o3_out} {o3_done}")
    wm_spawn_sh(f"wayland-deny-probe --scan > {o3_out} 2>&1; echo O3_DONE > {o3_done}")
    o3_ok, _ = wait_for_file(o3_done, "O3_DONE", WINDOW_MAP_TIMEOUT)
    _, o3_text = run_bash(vm, run_status, f"cat {o3_out} 2>/dev/null")
    (vm.out / "o3-scan.txt").write_text(o3_text)
    o3_core = re.findall(r"(?m)^CORE \d+ (\S+) \d+$", o3_text)
    o3_restricted = re.findall(r"(?m)^RESTRICTED \S+ \d+ (\S+) \d+$", o3_text)
    record["o3_scan"] = {"ok": o3_ok, "core": o3_core, "restricted": o3_restricted}
    if not o3_ok or o3_restricted or not {"wl_compositor", "wl_shm", "xdg_wm_base"}.issubset(set(o3_core)):
        problems.append(f"O3 scan failed: core={o3_core} restricted={o3_restricted}")

    fix_dir = f"{runtime_dir}/ru12a-fix"
    fix_done = f"{runtime_dir}/ru12a-fix.done"
    demo_env = f"{runtime_dir}/chooser-demo.env"
    demo_out = f"{runtime_dir}/chooser-demo.out"
    demo_err = f"{runtime_dir}/chooser-demo.err"
    run_bash(vm, run_status, f"rm -rf {fix_dir} {fix_done} {demo_env} {demo_out} {demo_err}")
    wm_spawn_sh(
        f"mkdir -p {fix_dir}/sub && "
        f"printf 'nested\\n' > {fix_dir}/sub/nested.txt && "
        f"printf '{{\"a\":1}}\\n' > {fix_dir}/alpha.json && "
        f"printf 'beta\\n' > {fix_dir}/beta.txt && "
        f"printf 'old\\n' > {fix_dir}/existing.txt && "
        f"ln -sf {fix_dir}/beta.txt {fix_dir}/link-to-beta.json && "
        f"printf 'FIX_UID=%s\\n' \"$(id -u)\" > {fix_done}"
    )
    fix_ok, fix_text = wait_for_file(fix_done, f"FIX_UID={user_uid}", KEY_STEP_TIMEOUT)
    record["fixture"] = {"dir": fix_dir, "ok": fix_ok, "meta": fix_text.strip()}
    if not fix_ok:
        problems.append(f"failed to create fixture as user uid {user_uid}: {fix_text!r}")

    wm_spawn_sh(
        f"unset FERRION_FILE_CHOOSER; "
        f"printf 'EUID=%s OVERRIDE=%s\\n' \"$(id -u)\" \"${{FERRION_FILE_CHOOSER:-}}\" > {demo_env}; "
        f"exec env RUNE_GPU_BACKEND=cpu {DEMO_PATH} --single-window --fixture-dir={fix_dir} > {demo_out} 2> {demo_err}"
    )
    demo_ready, demo_out_text = wait_for_file(demo_out, "CHOOSER_DEMO_READY", WINDOW_MAP_TIMEOUT)
    demo_win = wait_for_window(DEMO_APP_ID, WINDOW_MAP_TIMEOUT)
    _, demo_env_text = run_bash(vm, run_status, f"cat {demo_env} 2>/dev/null")
    pid_match = re.search(r"DEMO_PID=(\d+)", demo_out_text)
    demo_pid = int(pid_match.group(1)) if pid_match else None
    record["demo"] = {
        "ready": demo_ready,
        "pid": demo_pid,
        "env": demo_env_text.strip(),
        "window": demo_win,
    }
    if not demo_ready or demo_win is None or demo_pid is None:
        problems.append(f"rune-chooser-demo did not map within {WINDOW_MAP_TIMEOUT}s")
    if f"EUID={user_uid} OVERRIDE=" not in demo_env_text or demo_env_text.strip().split("OVERRIDE=", 1)[-1]:
        problems.append(f"demo env unexpected: {demo_env_text!r}")

    def send_qmp_keys(*keys):
        for k in keys:
            vm.command(f"sendkey {k}")
            time.sleep(0.35)

    def wait_answer(marker):
        ok, out = wait_for_file(demo_out, marker, 3)
        if ok:
            return True, out
        vm.command("sendkey shift")
        return wait_for_file(demo_out, marker, KEY_STEP_TIMEOUT)

    if not problems:
        wm_msg(f"action focus-window --id {demo_win['id']}")
        time.sleep(0.3)
        send_qmp_keys("spc")
        focus_before_ok, _ = wait_for_file(demo_out, "DEMO_KEY:main:count=1", KEY_STEP_TIMEOUT)
        record["steps"]["focus_before"] = focus_before_ok
        if not focus_before_ok:
            problems.append(f"demo did not receive initial space key within {KEY_STEP_TIMEOUT}s")

        send_qmp_keys("o")
        ch_win = wait_for_focused(CHOOSER_APP_ID, WINDOW_MAP_TIMEOUT)
        ch_ctx = context_entries(vm, run_status, CHOOSER_PROC_NAME)
        ch_ps = ps_entries(vm, run_status, CHOOSER_PROC_NAME)
        record["steps"]["open_window"] = ch_win
        record["steps"]["open_context"] = ch_ctx
        record["steps"]["open_ps"] = ch_ps
        if ch_win is None:
            problems.append(f"chooser window ({CHOOSER_APP_ID}) did not map within {WINDOW_MAP_TIMEOUT}s")
        else:
            if not ch_win.get("is_focused"):
                problems.append(f"chooser window not focused on open: {ch_win}")
            if not any(r["euid"] == user_uid and r["name"] == CHOOSER_PROC_NAME for r in ch_ctx):
                problems.append(f"chooser missing/wrong euid or path in /scheme/sys/context: {ch_ctx}")
            if not any(
                r["euid"] == user_uid and r["ppid"] == demo_pid and r["name"] == CHOOSER_PROC_NAME
                for r in ch_ps
            ):
                problems.append(f"chooser parent != demo_pid ({demo_pid}) or wrong euid in /scheme/proc/ps: {ch_ps}")

            send_qmp_keys("down", "down", "ret")
            expected_open = f"ANSWER:main:open:ok:{fix_dir}/link-to-beta.json"
            open_ok, _ = wait_answer(expected_open)
            reaped_ok, reap_info = wait_chooser_reaped(REAP_TIMEOUT)
            record["steps"]["open_symlink"] = {"ok": open_ok, "expected": expected_open, "reaped": reaped_ok}
            if not open_ok:
                problems.append(f"Open symlink answer missing (expected {expected_open})")
            if not reaped_ok:
                problems.append(f"chooser not reaped after Open within {REAP_TIMEOUT}s: {reap_info}")

            demo_after = wait_for_focused(DEMO_APP_ID, KEY_STEP_TIMEOUT)
            send_qmp_keys("spc")
            focus_back_ok, _ = wait_for_file(demo_out, "DEMO_KEY:main:count=2", KEY_STEP_TIMEOUT)
            record["steps"]["focus_return"] = {
                "demo_focused_flag": bool(demo_after and demo_after.get("is_focused")),
                "space_key_received": focus_back_ok,
            }
            if not focus_back_ok:
                problems.append(f"focus did not return to demo after chooser closed within {KEY_STEP_TIMEOUT}s")

        send_qmp_keys("f")
        ch_win = wait_for_focused(CHOOSER_APP_ID, WINDOW_MAP_TIMEOUT)
        if ch_win is None:
            problems.append("chooser window did not map for Folder")
        else:
            send_qmp_keys("ret")
            expected_folder = f"ANSWER:main:folder:ok:{fix_dir}/sub"
            folder_ok, _ = wait_answer(expected_folder)
            reaped_ok, _ = wait_chooser_reaped(REAP_TIMEOUT)
            record["steps"]["folder"] = {"ok": folder_ok, "expected": expected_folder, "reaped": reaped_ok}
            if not folder_ok or not reaped_ok:
                problems.append(f"Folder check failed: ok={folder_ok} reaped={reaped_ok}")
            wait_for_focused(DEMO_APP_ID, KEY_STEP_TIMEOUT)

        send_qmp_keys("s")
        ch_win = wait_for_focused(CHOOSER_APP_ID, WINDOW_MAP_TIMEOUT)
        if ch_win is None:
            problems.append("chooser window did not map for Save new")
        else:
            send_qmp_keys("ret")
            expected_save = f"ANSWER:main:save:ok:{fix_dir}/fresh.txt"
            save_ok, _ = wait_answer(expected_save)
            reaped_ok, _ = wait_chooser_reaped(REAP_TIMEOUT)
            record["steps"]["save_new"] = {"ok": save_ok, "expected": expected_save, "reaped": reaped_ok}
            if not save_ok or not reaped_ok:
                problems.append(f"Save new check failed: ok={save_ok} reaped={reaped_ok}")
            wait_for_focused(DEMO_APP_ID, KEY_STEP_TIMEOUT)

        send_qmp_keys("w")
        ch_win = wait_for_focused(CHOOSER_APP_ID, WINDOW_MAP_TIMEOUT)
        if ch_win is None:
            problems.append("chooser window did not map for Save overwrite")
        else:
            send_qmp_keys("ret")
            time.sleep(0.5)
            still_open_1 = wait_for_window(CHOOSER_APP_ID, 2) is not None
            send_qmp_keys("esc")
            time.sleep(0.5)
            still_open_2 = wait_for_window(CHOOSER_APP_ID, 2) is not None
            send_qmp_keys("ret")
            time.sleep(0.5)
            still_open_3 = wait_for_window(CHOOSER_APP_ID, 2) is not None
            send_qmp_keys("ret")
            expected_ov = f"ANSWER:main:save:ok:{fix_dir}/existing.txt"
            ov_ok, _ = wait_answer(expected_ov)
            reaped_ok, _ = wait_chooser_reaped(REAP_TIMEOUT)
            record["steps"]["save_overwrite"] = {
                "prompted": still_open_1,
                "esc_kept_open": still_open_2,
                "second_prompted": still_open_3,
                "ok": ov_ok,
                "expected": expected_ov,
                "reaped": reaped_ok,
            }
            if not (still_open_1 and still_open_2 and still_open_3 and ov_ok and reaped_ok):
                problems.append(f"Save overwrite check failed: {record['steps']['save_overwrite']}")
            wait_for_focused(DEMO_APP_ID, KEY_STEP_TIMEOUT)

        send_qmp_keys("u")
        ch_win = wait_for_focused(CHOOSER_APP_ID, WINDOW_MAP_TIMEOUT)
        if ch_win is None:
            problems.append("chooser window did not map for Cancel")
        else:
            send_qmp_keys("esc")
            cancel_ok, _ = wait_answer("ANSWER:main:open:cancel")
            reaped_ok, _ = wait_chooser_reaped(REAP_TIMEOUT)
            record["steps"]["cancel"] = {"ok": cancel_ok, "reaped": reaped_ok}
            if not cancel_ok or not reaped_ok:
                problems.append(f"Cancel check failed: ok={cancel_ok} reaped={reaped_ok}")
            wait_for_focused(DEMO_APP_ID, KEY_STEP_TIMEOUT)

        send_qmp_keys("u")
        ch_win = wait_for_focused(CHOOSER_APP_ID, WINDOW_MAP_TIMEOUT)
        kill_ctx_before = context_entries(vm, run_status, CHOOSER_PROC_NAME)
        kill_ps_before = ps_entries(vm, run_status, CHOOSER_PROC_NAME)
        if ch_win is None or not kill_ctx_before:
            problems.append("chooser did not open for mid-dialog SIGKILL check")
        else:
            run_bash(vm, run_status, f"kill -9 {demo_pid}")
            eof_ok, eof_after = wait_chooser_reaped(KILL_EOF_TIMEOUT)
            record["kill_eof"] = {
                "demo_pid": demo_pid,
                "chooser_before": kill_ctx_before,
                "chooser_ps_before": kill_ps_before,
                "exited_on_eof": eof_ok,
                "after": eof_after,
            }
            if not eof_ok:
                problems.append(f"chooser outlived SIGKILLed demo after {KILL_EOF_TIMEOUT}s: {eof_after}")

    _, final_out = run_bash(vm, run_status, f"cat {demo_out} 2>/dev/null")
    _, final_err = run_bash(vm, run_status, f"cat {demo_err} 2>/dev/null")
    (vm.out / "chooser-demo.out").write_text(final_out)
    (vm.out / "chooser-demo.err").write_text(final_err)

    save_record()
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'rune-chooser.json'}"
    return (
        True,
        f"euid={user_uid} path={CHOOSER_PATH} app_id={CHOOSER_APP_ID} focus=ok "
        f"open+folder+save+overwrite+cancel=ok kill_eof=ok; see {vm.out / 'rune-chooser.json'}",
    )
