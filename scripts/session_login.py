"""O1a check: console login starts a user-owned ferrion-wm session and recovers on exit."""
import json
import re
import time


def context_owners(vm, run_status, pattern):
    _, output = run_status(vm, f"grep '{pattern}' /scheme/sys/context")
    return [int(m) for m in re.findall(rf"(?m)^\s*\d+\s+(\d+)\s.*{re.escape(pattern)}\b", output)]


def extract_probe_value(text, key):
    match = re.search(rf"(?m)^{re.escape(key)}=([^\r\n]*)$", text)
    return match.group(1) if match else None


def run_session_login(vm, args, helpers):
    root_shell = helpers["root_shell"]
    run_status = helpers["run_status"]
    sendkeys = helpers["sendkeys"]
    settle = helpers["settle"]
    qmp = helpers.get("qmp")

    problems = []
    record = {"pointer_latency": None}

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

    if not root_shell(vm, args.timeout):
        return False, f"no root shell on serial console; see {vm.out}"

    _, passwd_line = run_status(vm, "grep '^user;' /etc/passwd")
    uid_match = re.search(r"user;(\d+);(\d+);[^;]*;([^;\r\n]+);", passwd_line)
    if not uid_match:
        return False, f"could not determine uid/home of user from /etc/passwd: {passwd_line!r}"
    user_uid = int(uid_match.group(1))
    user_gid = int(uid_match.group(2))
    user_home = uid_match.group(3)
    record["user_uid"] = user_uid
    record["user_home"] = user_home

    _, ps_before = run_status(vm, "ps")
    has_virtio = "virtio-gpud" in ps_before
    if not has_virtio:
        record["pointer_latency"] = "SKIPPED (no hardware cursor plane on vesad)"

    before_owners = context_owners(vm, run_status, "ferrion-wm")
    if before_owners:
        problems.append(f"ferrion-wm already running before VT 3 login: euids {before_owners}")

    runtime_dir = f"/var/run/user/{user_uid}"
    session_log = f"{runtime_dir}/session.log"
    canary_path = "/var/run/session-log-canary"
    canary_token = "CANARY_SECRET_O1A"
    prep_cmd = (
        f"mkdir -p {runtime_dir} && chmod 0755 /var/run/user && "
        f"chown {user_uid}:{user_gid} {runtime_dir} && chmod 0700 {runtime_dir} && "
        f"printf '%s\\n' '{canary_token}' > {canary_path} && "
        f"chown 0:0 {canary_path} && chmod 0600 {canary_path} && "
        f"rm -f {session_log}"
    )
    prep_status, prep_out = run_status(vm, prep_cmd)
    if prep_status != 0:
        return False, f"failed to prepare session.log canary bait: {prep_out!r}"
    ln_status, ln_out = run_status(vm, f"ln {canary_path} {session_log}")
    if ln_status == 0:
        record["session_log_bait"] = "hardlink"
    else:
        record["session_log_bait"] = "root-file"
        record["session_log_hardlink_refused"] = ln_out.strip().splitlines()[-1] if ln_out.strip() else f"exit {ln_status}"
        bait_status, bait_out = run_status(
            vm,
            f"printf '%s\\n' '{canary_token}' > {session_log} && "
            f"chown 0:0 {session_log} && chmod 0600 {session_log}",
        )
        if bait_status != 0:
            return False, f"failed to create root-owned session.log bait: {bait_out!r}"

    sendkeys(vm.command, "user\n")

    wm_deadline = time.monotonic() + args.timeout
    owners = []
    while time.monotonic() < wm_deadline:
        owners = context_owners(vm, run_status, "ferrion-wm")
        if user_uid in owners:
            break
        time.sleep(1.0)
    if user_uid not in owners:
        vm.screenshot().save_png(vm.out / "no-session.png")
        return False, f"no ferrion-wm running as euid {user_uid} after VT 3 login (saw {owners}); see {vm.out}"
    record["ferrion_wm_euids"] = owners

    session_frame = None
    screen_deadline = time.monotonic() + 30
    while time.monotonic() < screen_deadline:
        frame = vm.screenshot()
        if frame.has_ground_row() and frame.rgb != login_frame.rgb:
            session_frame = settle(vm, 10, still=1.5)
            break
        time.sleep(0.5)
    if session_frame is None:
        vm.screenshot().save_png(vm.out / "session-not-visible.png")
        problems.append("ferrion-wm started as user but session did not appear on screen")
    else:
        session_frame.save_png(vm.out / "session.png")

    if qmp is not None and session_frame is not None:
        qmp("input-send-event", {"events": [
            {"type": "abs", "data": {"axis": "x", "value": 6000}},
            {"type": "abs", "data": {"axis": "y", "value": 6000}},
        ]})
        time.sleep(0.5)
        pointer_a = settle(vm, 5, still=0.8)
        pointer_a.save_png(vm.out / "pointer-1.png")
        motion_times_ms = []
        for step in range(5):
            t0 = time.monotonic()
            qmp("input-send-event", {"events": [
                {"type": "abs", "data": {"axis": "x", "value": 10000 + step * 3000}},
                {"type": "abs", "data": {"axis": "y", "value": 10000 + step * 2000}},
            ]})
            time.sleep(0.15)
            motion_times_ms.append(round((time.monotonic() - t0) * 1000.0, 2))
        pointer_b = settle(vm, 5, still=0.8)
        pointer_b.save_png(vm.out / "pointer-2.png")
        diff_pixels = sum(
            1 for i in range(0, min(len(pointer_a.rgb), len(pointer_b.rgb)), 3)
            if pointer_a.rgb[i:i + 3] != pointer_b.rgb[i:i + 3]
        )
        record["pointer_motion_diff_pixels"] = diff_pixels
        record["pointer_motion_steps_ms"] = motion_times_ms
        if diff_pixels == 0:
            problems.append("QMP pointer motion changed zero pixels on screen (software cursor not drawn or did not move)")

    _, rt_stat = run_status(vm, f"ls -ld {runtime_dir}")
    record["runtime_dir_stat"] = rt_stat.strip()
    if "drwx------" not in rt_stat:
        problems.append(f"runtime dir {runtime_dir} does not have mode 0700: {rt_stat!r}")

    canary_status, canary_stat = run_status(vm, f"ls -l {canary_path}")
    canary_cat_status, canary_content = run_status(vm, f"cat {canary_path}")
    record["canary_stat"] = canary_stat.strip().splitlines()[-1] if canary_stat.strip() else ""
    record["canary_content"] = canary_content.strip().splitlines()[-1] if canary_content.strip() else ""
    if canary_status != 0 or "-rw-------" not in canary_stat or " root root " not in canary_stat:
        problems.append(f"canary {canary_path} owner or mode changed: {record['canary_stat']!r}")
    if canary_cat_status != 0 or canary_token not in canary_content:
        problems.append(f"canary {canary_path} content changed: {record['canary_content']!r}")

    log_status, log_stat = run_status(vm, f"ls -l {session_log}")
    record["session_log_stat"] = log_stat.strip().splitlines()[-1] if log_stat.strip() else ""
    if log_status != 0 or "-rw-------" not in log_stat or " user user " not in log_stat:
        problems.append(f"session log {session_log} missing or not 0600 user-owned: {record['session_log_stat']!r}")
    else:
        _, log_head = run_status(vm, f"head -n 20 {session_log}")
        (vm.out / "session-log.txt").write_text(log_head)
        if canary_token in log_head:
            problems.append(f"session log {session_log} retained canary content instead of a fresh file")
        if "ferrion_wm" not in log_head:
            problems.append(f"session log {session_log} has no ferrion_wm output")

    probe_file = f"{runtime_dir}/spawn-probe.txt"
    run_status(vm, f"rm -f {probe_file}")
    vm.command("sendkey meta_l-t")
    probe_text = ""
    probe_deadline = time.monotonic() + 30
    while time.monotonic() < probe_deadline:
        status, out = run_status(vm, f"cat {probe_file}")
        if status == 0 and "id=" in out:
            probe_text = out
            break
        time.sleep(1.0)
    (vm.out / "spawn-probe.txt").write_text(probe_text)
    if not probe_text:
        problems.append(f"Mod+T did not produce {probe_file}")
    else:
        if f"uid={user_uid}(" not in probe_text and f"uid={user_uid} " not in probe_text and f"uid={user_uid}\n" not in probe_text:
            problems.append(f"Mod+T child did not run as uid {user_uid}: {probe_text!r}")
        fd1_path = extract_probe_value(probe_text, "fd1_path")
        fd2_path = extract_probe_value(probe_text, "fd2_path")
        record["fd1_path"] = fd1_path
        record["fd2_path"] = fd2_path
        if not fd1_path or "pty" in fd1_path.lower():
            problems.append(f"Mod+T child fd1_path missing or points to pty: {fd1_path!r}")
        if not fd2_path or "pty" in fd2_path.lower():
            problems.append(f"Mod+T child fd2_path missing or points to pty: {fd2_path!r}")
        for required in (
            "env_consumer=unset",
            "env_login=unset",
            "env_init_notify=unset",
            "env_tty=unset",
            "gsettings_backend=keyfile",
            f"home={user_home}",
            "home_writable=yes",
            "input_scheme=denied",
            "open_fds=none",
            f"xdg_runtime_dir={runtime_dir}",
            "wayland_display=wayland-",
        ):
            if required not in probe_text:
                problems.append(f"Mod+T child probe missing {required!r}: {probe_text!r}")

    vm.command("sendkey meta_l-l")
    sleep_deadline = time.monotonic() + 15
    sleep_owners = []
    while time.monotonic() < sleep_deadline:
        sleep_owners = context_owners(vm, run_status, "sleep")
        if user_uid in sleep_owners:
            break
        time.sleep(0.5)
    record["lingering_sleep_before_exit"] = sleep_owners
    if user_uid not in sleep_owners:
        problems.append(f"Mod+L did not spawn lingering sleep child as uid {user_uid} (saw {sleep_owners})")

    vm.command("sendkey meta_l-shift-e")
    exit_deadline = time.monotonic() + 30
    while time.monotonic() < exit_deadline:
        if not context_owners(vm, run_status, "ferrion-wm"):
            break
        time.sleep(1.0)
    if context_owners(vm, run_status, "ferrion-wm"):
        problems.append("ferrion-wm did not exit on Mod+Shift+E")

    recovery_frame = None
    rec_deadline = time.monotonic() + args.timeout
    while time.monotonic() < rec_deadline:
        frame = vm.screenshot()
        if frame.kind() == "text":
            recovery_frame = settle(vm, 10, still=1.5)
            break
        time.sleep(0.5)
    record["lingering_sleep_after_recovery"] = context_owners(vm, run_status, "sleep")
    if recovery_frame is None:
        vm.screenshot().save_png(vm.out / "no-recovery.png")
        problems.append("VT 3 text login did not return after ferrion-wm exited with lingering child")
    else:
        recovery_frame.save_png(vm.out / "after-exit.png")
    if user_uid not in record["lingering_sleep_after_recovery"]:
        problems.append(
            f"lingering sleep child did not outlive ferrion-wm exit (saw {record['lingering_sleep_after_recovery']})"
        )

    record["problems"] = problems
    (vm.out / "session-login.json").write_text(json.dumps(record, indent=2) + "\n")
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out}"
    note = (
        f"ferrion-wm euid={user_uid}, session.log 0600, Mod+T child uid={user_uid} "
        f"(fd1={record.get('fd1_path')}, fd2={record.get('fd2_path')}, no leaked fd/env), "
        f"exit returned to VT 3 text login while lingering sleep child survived"
    )
    if record["pointer_latency"]:
        note += f"; pointer-latency {record['pointer_latency']}"
    return True, note
