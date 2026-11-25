"""PFR1d check: Loupe under its installed manifest (display, files-pictures) with nested glycin loaders.

Launches Loupe through ferrion-run --env GSETTINGS_BACKEND --env GSK_RENDERER -- /usr/bin/loupe
with GSK_RENDERER=cairo (gpu is not declared in Loupe's manifest and remains PFR1's open gap).
Opening files outside ~/Pictures fails by design until the file picker (Q5).
"""
import base64
import json
import re
import shutil
import tempfile
import time
from pathlib import Path

from loupe_view import (
    ALBUM_SPEC,
    CLOSE_POLL_ITERS,
    MIN_IMAGE_PIXELS,
    MP24_POLL_ITERS,
    MP24_RGB,
    SIDEBAR_MAX_MATCH_DIFF,
    SIDEBAR_MIN_DIFF,
    WAYLAND_SOCKET_WAIT_ITERS,
    WINDOW_POLL_ITERS,
    count_rgb_box,
    generate_fixtures,
    parse_json_array,
    sidebar_mean_abs_diff,
    start_fixture_server,
    window_rect,
)

NEGATIVE_POLL_ITERS = 20
CONTEXT_SAMPLE_MAX = 12


def run_loupe_manifest(vm, args, helpers):
    """Verifies Loupe under /usr/share/ferrion/apps/loupe.toml (display, files-pictures) with nested glycin loaders."""
    Blocked = helpers["Blocked"]
    sendkeys = helpers["sendkeys"]
    settle = helpers["settle"]
    read_root_contexts = helpers["read_root_contexts"]

    if not shutil.which("ffmpeg"):
        raise Blocked("host lacks ffmpeg")
    if not (shutil.which("avifenc") or shutil.which("heif-enc")):
        raise Blocked("host lacks avifenc/heif-enc")

    record = {
        "launch_path": (
            "ferrion-wm msg action spawn -> ferrion-run --env GSETTINGS_BACKEND "
            "--env GSK_RENDERER -- /usr/bin/loupe (manifest: display, files-pictures; "
            "glycin loaders nested inside)"
        ),
        "renderer_requested": "cairo",
        "gpu_gap": "GSK_RENDERER=cairo; gpu is not declared in loupe.toml and remains PFR1's open gap.",
        "outside_pictures_note": "Opening files outside ~/Pictures fails by design until the file picker (Q5).",
    }

    def save_record():
        (vm.out / "loupe-manifest.json").write_text(
            json.dumps(record, indent=2) + "\n", encoding="utf-8"
        )

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
        vm.type(f"bash -c '{line}; echo lm-end-{tag}-$((40+2))-$?'\n")
        found = vm.wait_serial(rf"(?s)\n(.*)lm-end-{tag}-42-(\d+)", timeout, since)
        if not found:
            timed_out = True
            return None, vm.text(since)
        return int(found.group(2)), found.group(1)

    def run_sudo(line, timeout=45):
        nonlocal seq, timed_out
        if timed_out:
            return None, ""
        assert "'" not in line, f"single quote disallowed in sudo bash -c payload: {line!r}"
        seq += 1
        tag = seq
        since = vm.mark()
        vm.type(f"sudo bash -c '{line}; echo lms-end-{tag}-$((40+2))-$?'\n")
        if vm.wait_serial(rf"\[sudo\] password|lms-end-{tag}-42-\d", timeout, since):
            if not re.search(rf"lms-end-{tag}-42-\d", vm.text(since)):
                vm.type("\n")
        found = vm.wait_serial(rf"(?s)\n(.*)lms-end-{tag}-42-(\d+)", timeout, since)
        if not found:
            timed_out = True
            return None, vm.text(since)
        return int(found.group(2)), found.group(1)

    def write_root_file(path, text):
        b64 = base64.b64encode(text.encode("utf-8")).decode("ascii")
        rc, out = run_sudo(
            f"mkdir -p $(dirname {path}) && echo {b64} | base64 -d > {path} && chmod 644 {path}",
            timeout=30,
        )
        return rc, out

    rc_bin, _ = run_user("command -v loupe && command -v ferrion-run && command -v ferrion-run-probe", timeout=30)
    if rc_bin != 0:
        record["failure"] = "loupe, ferrion-run, or ferrion-run-probe not installed"
        save_record()
        return False, f"required binaries missing in guest; see {vm.out}"

    rc_man, loupe_manifest_txt = run_user("cat /usr/share/ferrion/apps/loupe.toml", timeout=15)
    record["manifest_installed"] = rc_man == 0 and bool(loupe_manifest_txt.strip())
    if not record["manifest_installed"]:
        record["failure"] = "/usr/share/ferrion/apps/loupe.toml missing"
        save_record()
        return False, f"/usr/share/ferrion/apps/loupe.toml missing in guest; see {vm.out}"

    if "[capabilities.gpu]" in loupe_manifest_txt:
        record["failure"] = "loupe.toml unexpectedly declares gpu"
        save_record()
        return False, f"/usr/share/ferrion/apps/loupe.toml must not declare gpu; see {vm.out}"

    rc_vendor, _ = run_user("test ! -e /usr/share/glycin/vendor", timeout=15)
    record["glycin_vendor_absent"] = rc_vendor == 0
    if rc_vendor != 0:
        record["failure"] = "/usr/share/glycin/vendor still present in runtime image"
        save_record()
        return False, f"/usr/share/glycin/vendor is still present in runtime image; see {vm.out}"

    rc_sc, sc_out = run_user(
        "test -s /usr/share/glib-2.0/schemas/gschemas.compiled && gsettings list-schemas",
        timeout=30,
    )
    schemas = [s.strip() for s in sc_out.splitlines() if s.strip()]
    has_gtk_schema = "org.gtk.gtk4.Settings.FileChooser" in schemas
    has_loupe_schema = "org.gnome.Loupe" in schemas
    record["schemas_check"] = {
        "rc": rc_sc,
        "has_gtk4_filechooser": has_gtk_schema,
        "has_loupe": has_loupe_schema,
        "schema_count": len(schemas),
    }
    if rc_sc != 0 or not has_gtk_schema or not has_loupe_schema:
        save_record()
        return False, f"compiled GSettings schema check failed: {sc_out!r}; see {vm.out}"

    rc_uid, uid_out = run_user('echo "UID:$(id -u)"', timeout=20)
    uid_match = re.search(r"UID:(\d+)", uid_out)
    if rc_uid != 0 or not uid_match:
        return False, f"could not determine user uid: rc={rc_uid} out={uid_out!r}"
    user_uid = int(uid_match.group(1))
    runtime_dir = f"/var/run/user/{user_uid}"
    record["user_uid"] = user_uid

    caps_idx = loupe_manifest_txt.find("[capabilities.")
    if caps_idx < 0:
        return False, f"unexpected loupe.toml format: {loupe_manifest_txt!r}"
    audit_manifest = (
        'format = 1\n'
        'id = "pfr1-loupe-audit"\n'
        'name = "Loupe Manifest Audit"\n'
        'kind = "native"\n'
        'exec = "/usr/bin/pfr1-loupe-audit"\n\n'
        + loupe_manifest_txt[caps_idx:]
    )
    grants_toml = (
        "format = 1\n\n"
        "[apps.loupe]\n"
        "files-pictures = true\n\n"
        "[apps.pfr1-loupe-audit]\n"
        "files-pictures = true\n"
    )
    rc_setup, out_setup = run_sudo(
        "cp /usr/bin/ferrion-run-probe /usr/bin/pfr1-loupe-ctrl && chmod 755 /usr/bin/pfr1-loupe-ctrl && "
        "cp /usr/bin/bash /usr/bin/pfr1-loupe-audit && chmod 755 /usr/bin/pfr1-loupe-audit",
        timeout=30,
    )
    if rc_setup != 0:
        return False, f"failed installing snapshot helper binaries (rc={rc_setup}): {out_setup!r}"
    for path, body in (
        (f"/var/lib/ferrion/grants/{user_uid}.toml", grants_toml),
        ("/usr/share/ferrion/apps/pfr1-loupe-audit.toml", audit_manifest),
    ):
        rc_w, out_w = write_root_file(path, body)
        if rc_w != 0:
            return False, f"failed writing {path} (rc={rc_w}): {out_w!r}"

    sendkeys(vm.command, "user\n")

    wayland_sock = None
    wm_sock = None
    for _ in range(WAYLAND_SOCKET_WAIT_ITERS):
        rc_s, out_s = run_user(
            f'for p in {runtime_dir}/wayland-* {runtime_dir}/ferrion-wm.*.sock; do '
            f'[ -e "$p" ] || continue; b=$(basename "$p"); '
            f'case "$b" in *.lock) ;; wayland-[0-9]*) echo "WL:$b";; ferrion-wm.*.sock) echo "WM:$p";; esac; done',
            timeout=15,
        )
        m_wl = re.search(r"WL:(wayland-[0-9]+)\b", out_s)
        m_wm = re.search(r"WM:(\S+\.sock)\b", out_s)
        if rc_s == 0 and m_wl and m_wm:
            wayland_sock = m_wl.group(1)
            wm_sock = m_wm.group(1)
            break
        time.sleep(1.0)
    if not wayland_sock or not wm_sock:
        vm.screenshot().save_png(vm.out / "no-wm-socket.png")
        save_record()
        return False, f"ferrion-wm sockets missing in {runtime_dir}; see {vm.out}"
    record["wayland_display"] = wayland_sock
    record["ferrion_wm_socket"] = wm_sock
    settle(vm, 10, still=1.0).save_png(vm.out / "session.png")

    def query_loupe_windows():
        rc_w, out_w = run_user(
            f"FERRION_WM_SOCKET={wm_sock} ferrion-wm msg --json windows",
            timeout=20,
        )
        if rc_w != 0:
            return []
        return [
            w for w in parse_json_array(out_w)
            if isinstance(w, dict) and w.get("app_id") in ("org.gnome.Loupe", "loupe")
        ]

    def close_loupe_windows():
        for w in query_loupe_windows():
            wid = w.get("id")
            if wid is not None:
                run_user(
                    f"FERRION_WM_SOCKET={wm_sock} ferrion-wm msg action close-window --id {wid}",
                    timeout=15,
                )
        for _ in range(CLOSE_POLL_ITERS):
            if not query_loupe_windows():
                return True
            time.sleep(0.5)
        return False

    def spawn_loupe(target_path, env_file, log_file, rc_file, gsettings_override=None):
        run_user(f"rm -f {env_file} {log_file} {rc_file}", timeout=15)
        wrapper = "/tmp/loupe-manifest-launch.sh"
        override_line = (
            f"export GSETTINGS_BACKEND={gsettings_override}\n"
            if gsettings_override is not None
            else ""
        )
        script_body = (
            "#!/usr/bin/env bash\n"
            "export GSK_RENDERER=cairo\n"
            f"{override_line}"
            f"env > {env_file}\n"
            f"ferrion-run --env GSETTINGS_BACKEND --env GSK_RENDERER -- /usr/bin/loupe {target_path} "
            f"</dev/null 2>&1 | cat > {log_file}\n"
            f"echo \"${{PIPESTATUS[0]}}\" > {rc_file}\n"
        )
        b64 = base64.b64encode(script_body.encode("utf-8")).decode("ascii")
        rc_w, out_w = run_user(f"echo {b64} | base64 -d > {wrapper} && chmod +x {wrapper}", timeout=15)
        if rc_w != 0:
            raise RuntimeError(f"failed writing launcher wrapper: {out_w!r}")
        rc_sp, out_sp = run_user(
            f"FERRION_WM_SOCKET={wm_sock} ferrion-wm msg action spawn -- {wrapper}",
            timeout=20,
        )
        if rc_sp != 0:
            raise RuntimeError(f"ferrion-wm msg action spawn failed (rc={rc_sp}): {out_sp!r}")

    rc_net, _ = run_user(
        'deadline=$((SECONDS+60)); server=$(cat /etc/net/dns); '
        'while [[ -z "$server" || "$server" == 9.9.9.9 ]] && (( SECONDS < deadline )); do '
        'sleep 0.1; server=$(cat /etc/net/dns); done; '
        '[[ -n "$server" && "$server" != 9.9.9.9 ]]',
        timeout=70,
    )
    if rc_net != 0:
        save_record()
        return False, f"DHCP did not finish before fixture download; see {vm.out}"

    album_dir = "/home/user/Pictures/loupe-album"
    mp24_dir = "/home/user/Pictures/loupe-24mp"
    docs_dir = "/home/user/Documents"
    with tempfile.TemporaryDirectory(prefix="ferrion-loupe-manifest-") as tmp:
        fixtures = generate_fixtures(Path(tmp))
        server = start_fixture_server(fixtures)
        port = server.server_address[1]
        try:
            rc_mk, _ = run_user(
                f"rm -rf {album_dir} {mp24_dir} && mkdir -p {album_dir} {mp24_dir} {docs_dir}",
                timeout=15,
            )
            if rc_mk != 0:
                return False, f"failed creating guest fixture dirs; see {vm.out}"
            for filename, _, _ in ALBUM_SPEC:
                rc_dl, dl_out = run_user(
                    f"curl --http1.0 -fsS --max-time 45 -o {album_dir}/{filename} http://10.0.2.2:{port}/{filename}",
                    timeout=60,
                )
                if rc_dl != 0:
                    return False, f"failed fetching {filename}: {dl_out!r}; see {vm.out}"
            rc_dl, dl_out = run_user(
                f"curl --http1.0 -fsS --max-time 60 -o {mp24_dir}/large_24mp.png http://10.0.2.2:{port}/large_24mp.png",
                timeout=75,
            )
            if rc_dl != 0:
                return False, f"failed fetching large_24mp.png: {dl_out!r}; see {vm.out}"
            rc_cp, _ = run_user(
                f"cp {album_dir}/01_png.png {docs_dir}/secret.png && cp {album_dir}/08_svg.svg {docs_dir}/secret.svg",
                timeout=15,
            )
            if rc_cp != 0:
                return False, f"failed planting Documents negative images; see {vm.out}"
        finally:
            server.shutdown()
            server.server_close()

    run_user(
        "rm -f /tmp/lm-ctrl.pid && (/usr/bin/pfr1-loupe-ctrl sleep 300 </dev/null >/dev/null 2>&1 & echo $! > /tmp/lm-ctrl.pid)",
        timeout=15,
    )

    nested_obs = {
        "saw_ctrl": False,
        "saw_loupe": False,
        "max_ferrion_run": 0,
        "saw_loader": [],
        "samples": 0,
    }

    def sample_nested_contexts(tag):
        if nested_obs["samples"] >= CONTEXT_SAMPLE_MAX:
            return
        nested_obs["samples"] += 1
        ctx_res = read_root_contexts(vm, tag, timeout=30)
        if ctx_res is None:
            return
        rows, _ = ctx_res
        user_rows = [r for r in rows if r.euid == user_uid]
        if any("pfr1-loupe-ctrl" in r.name for r in user_rows):
            nested_obs["saw_ctrl"] = True
        if any("loupe" in r.name and "pfr1-loupe-" not in r.name for r in user_rows):
            nested_obs["saw_loupe"] = True
        fr_count = sum(1 for r in user_rows if "ferrion-run" in r.name)
        if fr_count > nested_obs["max_ferrion_run"]:
            nested_obs["max_ferrion_run"] = fr_count
        for r in user_rows:
            if "glycin-" in r.name and r.name not in nested_obs["saw_loader"]:
                nested_obs["saw_loader"].append(r.name)

    app_home_keyfile = "/home/user/.local/share/ferrion/apps/loupe/home/.config/glib-2.0/settings/keyfile"
    host_keyfile = "/home/user/.config/glib-2.0/settings/keyfile"
    run_user(f"rm -f {app_home_keyfile} {host_keyfile}", timeout=15)

    album_env = "/tmp/lm-album.env"
    album_log = "/tmp/lm-album.log"
    album_rc = "/tmp/lm-album.rc"
    t_first = time.monotonic()
    spawn_loupe(f"{album_dir}/01_png.png", album_env, album_log, album_rc)

    formats_results = []
    for idx, (filename, fmt, rgb) in enumerate(ALBUM_SPEC):
        if idx > 0:
            t_step = time.monotonic()
            vm.command("sendkey right")
        else:
            t_step = t_first
        matched = False
        pixels = 0
        win_title = ""
        for poll in range(WINDOW_POLL_ITERS):
            if fmt == "svg" and poll in (1, 3) and not nested_obs["saw_loader"]:
                sample_nested_contexts(f"lm-svg-{poll}")
            wins = query_loupe_windows()
            if wins:
                win_title = str(wins[0].get("title") or "")
            frame = vm.screenshot()
            pixels = count_rgb_box(frame, rgb)
            if wins and pixels >= MIN_IMAGE_PIXELS and filename in win_title:
                elapsed_ms = round((time.monotonic() - t_step) * 1000, 1)
                frame.save_png(vm.out / f"loupe-{filename}.png")
                formats_results.append({
                    "file": filename,
                    "format": fmt,
                    "pixels": pixels,
                    "title": win_title,
                    "elapsed_ms": elapsed_ms,
                })
                matched = True
                break
            time.sleep(0.25)
        if not matched:
            vm.screenshot().save_png(vm.out / f"loupe-fail-{filename}.png")
            _, log_txt = run_user(f"cat {album_log} 2>/dev/null || true", timeout=15)
            _, rc_txt = run_user(f"cat {album_rc} 2>/dev/null || true", timeout=15)
            record["formats"] = formats_results
            record["album_log"] = log_txt
            record["album_rc"] = rc_txt.strip()
            record["nested_contexts"] = nested_obs
            save_record()
            return False, (
                f"Sandboxed Loupe did not render {filename} ({fmt}): pixels={pixels}, "
                f"title={win_title!r}, rc={rc_txt.strip()!r}; log={log_txt!r}; see {vm.out}"
            )

    record["formats"] = formats_results
    _, env_txt = run_user(f"cat {album_env} 2>/dev/null || true", timeout=15)
    m_env = re.search(r"^GSETTINGS_BACKEND=(\S+)$", env_txt, re.MULTILINE)
    session_backend = m_env.group(1) if m_env else "<unset>"
    record["session_gsettings_backend"] = session_backend
    if session_backend != "keyfile":
        save_record()
        return False, f"expected GSETTINGS_BACKEND=keyfile from session, got {session_backend!r}; see {vm.out}"

    _, album_log_txt = run_user(f"cat {album_log} 2>/dev/null || true", timeout=15)
    record["album_log"] = album_log_txt
    if "GLib-GIO-CRITICAL" in album_log_txt:
        save_record()
        return False, f"Sandboxed Loupe logged GLib-GIO-CRITICAL: {album_log_txt!r}; see {vm.out}"

    if not close_loupe_windows():
        save_record()
        return False, f"Sandboxed Loupe window did not close after album check; see {vm.out}"

    mp24_env = "/tmp/lm-24mp.env"
    mp24_log = "/tmp/lm-24mp.log"
    mp24_rc = "/tmp/lm-24mp.rc"
    t_mp24 = time.monotonic()
    spawn_loupe(f"{mp24_dir}/large_24mp.png", mp24_env, mp24_log, mp24_rc)
    mp24_ms = None
    mp24_pixels = 0
    for poll in range(MP24_POLL_ITERS):
        if poll in (4, 10, 18, 28, 40) and not (
            nested_obs["saw_ctrl"]
            and nested_obs["saw_loupe"]
            and nested_obs["max_ferrion_run"] >= 2
            and nested_obs["saw_loader"]
        ):
            sample_nested_contexts(f"lm-24mp-{poll}")
        wins = query_loupe_windows()
        frame = vm.screenshot()
        mp24_pixels = count_rgb_box(frame, MP24_RGB)
        if wins and mp24_pixels >= MIN_IMAGE_PIXELS:
            mp24_ms = round((time.monotonic() - t_mp24) * 1000, 1)
            frame.save_png(vm.out / "loupe-24mp.png")
            break
        time.sleep(0.1)
    _, mp24_log_txt = run_user(f"cat {mp24_log} 2>/dev/null || true", timeout=15)
    record["mp24"] = {
        "first_frame_ms": mp24_ms,
        "pixels": mp24_pixels,
        "log": mp24_log_txt,
    }
    record["nested_contexts"] = nested_obs
    if mp24_ms is None or "GLib-GIO-CRITICAL" in mp24_log_txt:
        vm.screenshot().save_png(vm.out / "loupe-24mp-fail.png")
        save_record()
        return False, f"Sandboxed Loupe 24 MP image failed (ms={mp24_ms}, px={mp24_pixels}): {mp24_log_txt!r}; see {vm.out}"
    if not close_loupe_windows():
        save_record()
        return False, f"Sandboxed Loupe did not close after 24 MP check; see {vm.out}"

    if not nested_obs["saw_ctrl"]:
        save_record()
        return False, f"independent positive control pfr1-loupe-ctrl not seen in read_root_contexts: {nested_obs}; see {vm.out}"
    if not nested_obs["saw_loupe"] or nested_obs["max_ferrion_run"] < 2 or not nested_obs["saw_loader"]:
        save_record()
        return False, f"nested glycin loaders were not observed under ferrion-run in read_root_contexts: {nested_obs}; see {vm.out}"

    run_user(f"rm -f {app_home_keyfile} {host_keyfile}", timeout=15)
    green_env = "/tmp/lm-green.env"
    green_log = "/tmp/lm-green.log"
    green_rc = "/tmp/lm-green.rc"
    spawn_loupe(f"{album_dir}/01_png.png", green_env, green_log, green_rc)
    green_win = None
    for _ in range(WINDOW_POLL_ITERS):
        wins = query_loupe_windows()
        frame = vm.screenshot()
        if wins and count_rgb_box(frame, ALBUM_SPEC[0][2]) >= MIN_IMAGE_PIXELS:
            green_win = wins[0]
            break
        time.sleep(0.25)
    if green_win is None:
        save_record()
        return False, f"Sandboxed Loupe did not open for GSettings check; see {vm.out}"
    closed_frame = settle(vm, 5, still=0.8)
    closed_frame.save_png(vm.out / "gsettings-green-closed.png")
    green_rect = window_rect(green_win, closed_frame.width, closed_frame.height)

    vm.command("sendkey f9")
    open_frame = None
    open_diff = 0.0
    keyfile_txt = ""
    for _ in range(WINDOW_POLL_ITERS):
        _, keyfile_txt = run_user(f"cat {app_home_keyfile} 2>/dev/null || true", timeout=15)
        frame = vm.screenshot()
        diff = sidebar_mean_abs_diff(closed_frame, frame, green_rect)
        if "show-properties=true" in keyfile_txt and diff >= SIDEBAR_MIN_DIFF:
            open_frame = settle(vm, 5, still=0.8)
            open_diff = sidebar_mean_abs_diff(closed_frame, open_frame, green_rect)
            open_frame.save_png(vm.out / "gsettings-green-open.png")
            break
        time.sleep(0.25)
    _, host_kf_txt = run_user(f"cat {host_keyfile} 2>/dev/null || true", timeout=15)
    if open_frame is None:
        vm.screenshot().save_png(vm.out / "gsettings-green-f9-fail.png")
        record["gsettings_green"] = {
            "app_home_keyfile": keyfile_txt,
            "host_keyfile": host_kf_txt,
            "open_diff": open_diff,
        }
        save_record()
        return False, f"F9 did not write show-properties=true into app home keyfile: {keyfile_txt!r}; see {vm.out}"
    if host_kf_txt.strip():
        save_record()
        return False, f"Sandboxed Loupe unexpectedly wrote host keyfile {host_keyfile}: {host_kf_txt!r}; see {vm.out}"

    if not close_loupe_windows():
        return False, f"Sandboxed Loupe did not close before green restart; see {vm.out}"

    spawn_loupe(f"{album_dir}/01_png.png", "/tmp/lm-green2.env", "/tmp/lm-green2.log", "/tmp/lm-green2.rc")
    reopened_frame = None
    for _ in range(WINDOW_POLL_ITERS):
        wins = query_loupe_windows()
        frame = vm.screenshot()
        if wins and count_rgb_box(frame, ALBUM_SPEC[0][2]) >= MIN_IMAGE_PIXELS:
            reopened_frame = settle(vm, 5, still=0.8)
            break
        time.sleep(0.25)
    if reopened_frame is None:
        return False, f"Sandboxed Loupe did not reopen on green restart; see {vm.out}"
    reopened_frame.save_png(vm.out / "gsettings-green-reopened.png")
    diff_reopened_vs_closed = sidebar_mean_abs_diff(reopened_frame, closed_frame, green_rect)
    diff_reopened_vs_open = sidebar_mean_abs_diff(reopened_frame, open_frame, green_rect)
    record["gsettings_green"] = {
        "app_home_keyfile_path": app_home_keyfile,
        "keyfile": keyfile_txt.strip(),
        "open_vs_closed_diff": open_diff,
        "reopened_vs_closed_diff": diff_reopened_vs_closed,
        "reopened_vs_open_diff": diff_reopened_vs_open,
    }
    if diff_reopened_vs_closed < SIDEBAR_MIN_DIFF or diff_reopened_vs_open > SIDEBAR_MAX_MATCH_DIFF:
        save_record()
        return False, (
            f"show-properties did not survive restart in app home "
            f"(vs_closed={diff_reopened_vs_closed}, vs_open={diff_reopened_vs_open}); see {vm.out}"
        )
    if not close_loupe_windows():
        return False, f"Sandboxed Loupe did not close after green restart; see {vm.out}"

    run_user(f"rm -f {app_home_keyfile}", timeout=15)
    spawn_loupe(f"{album_dir}/01_png.png", "/tmp/lm-mem.env", "/tmp/lm-mem.log", "/tmp/lm-mem.rc", gsettings_override="memory")
    mem_win = None
    for _ in range(WINDOW_POLL_ITERS):
        wins = query_loupe_windows()
        frame = vm.screenshot()
        if wins and count_rgb_box(frame, ALBUM_SPEC[0][2]) >= MIN_IMAGE_PIXELS:
            mem_win = wins[0]
            break
        time.sleep(0.25)
    if mem_win is None:
        return False, f"Sandboxed Loupe did not open under GSETTINGS_BACKEND=memory; see {vm.out}"
    mem_closed = settle(vm, 5, still=0.8)
    mem_rect = window_rect(mem_win, mem_closed.width, mem_closed.height)
    vm.command("sendkey f9")
    mem_open = None
    for _ in range(WINDOW_POLL_ITERS):
        frame = vm.screenshot()
        if sidebar_mean_abs_diff(mem_closed, frame, mem_rect) >= SIDEBAR_MIN_DIFF:
            mem_open = settle(vm, 5, still=0.8)
            break
        time.sleep(0.25)
    if mem_open is None:
        return False, f"F9 did not toggle properties sidebar under memory backend; see {vm.out}"
    _, mem_kf_txt = run_user(f"cat {app_home_keyfile} 2>/dev/null || true", timeout=15)
    if not close_loupe_windows():
        return False, f"Sandboxed Loupe did not close before memory restart; see {vm.out}"

    spawn_loupe(f"{album_dir}/01_png.png", "/tmp/lm-mem2.env", "/tmp/lm-mem2.log", "/tmp/lm-mem2.rc", gsettings_override="memory")
    mem_reopened = None
    for _ in range(WINDOW_POLL_ITERS):
        wins = query_loupe_windows()
        frame = vm.screenshot()
        if wins and count_rgb_box(frame, ALBUM_SPEC[0][2]) >= MIN_IMAGE_PIXELS:
            mem_reopened = settle(vm, 5, still=0.8)
            break
        time.sleep(0.25)
    if mem_reopened is None:
        return False, f"Sandboxed Loupe did not reopen under memory backend; see {vm.out}"
    mem_vs_closed = sidebar_mean_abs_diff(mem_reopened, mem_closed, mem_rect)
    mem_vs_open = sidebar_mean_abs_diff(mem_reopened, mem_open, mem_rect)
    record["gsettings_memory"] = {
        "keyfile_after_f9": mem_kf_txt.strip(),
        "reopened_vs_closed_diff": mem_vs_closed,
        "reopened_vs_open_diff": mem_vs_open,
    }
    if mem_vs_closed > SIDEBAR_MAX_MATCH_DIFF or mem_vs_open < SIDEBAR_MIN_DIFF or "show-properties=true" in mem_kf_txt:
        save_record()
        return False, (
            f"show-properties unexpectedly persisted under GSETTINGS_BACKEND=memory "
            f"(vs_closed={mem_vs_closed}, vs_open={mem_vs_open}); see {vm.out}"
        )
    if not close_loupe_windows():
        return False, f"Sandboxed Loupe did not close after memory restart; see {vm.out}"

    neg_results = {}
    for neg_name, neg_path, neg_rgb in (
        ("secret_png", f"{docs_dir}/secret.png", ALBUM_SPEC[0][2]),
        ("secret_svg", f"{docs_dir}/secret.svg", ALBUM_SPEC[7][2]),
    ):
        spawn_loupe(neg_path, f"/tmp/lm-{neg_name}.env", f"/tmp/lm-{neg_name}.log", f"/tmp/lm-{neg_name}.rc")
        max_px = 0
        saw_win = False
        for _ in range(NEGATIVE_POLL_ITERS):
            if query_loupe_windows():
                saw_win = True
            frame = vm.screenshot()
            px = count_rgb_box(frame, neg_rgb)
            if px > max_px:
                max_px = px
            time.sleep(0.25)
        vm.screenshot().save_png(vm.out / f"loupe-neg-{neg_name}.png")
        _, neg_log = run_user(f"cat /tmp/lm-{neg_name}.log 2>/dev/null || true", timeout=15)
        close_loupe_windows()
        neg_results[neg_name] = {"saw_window": saw_win, "max_pixels": max_px, "log": neg_log}
        if max_px >= MIN_IMAGE_PIXELS:
            record["negatives"] = neg_results
            save_record()
            return False, f"Sandboxed Loupe unexpectedly rendered {neg_path} ({max_px} px); see {vm.out}"

    audit_script = (
        "echo SCHEMES:$(ferrion-run-probe schemes); "
        "ferrion-run-probe cap-check 10.0.2.2:80 / /home/user/Documents/secret.svg "
        "/home/user/Pictures/loupe-album/08_svg.svg /home/user/Downloads/none; "
        "ferrion-run --no-network --read-only /home/user/Pictures/loupe-album -- true >/dev/null 2>&1; echo NEST_PIC_RC:$?; "
        "OUT=$(ferrion-run --no-network --read-only /home/user/Documents -- true 2>&1); "
        "echo NEST_DOC_RC:$?; echo NEST_DOC_OUT:$OUT"
    )
    audit_b64 = base64.b64encode(audit_script.encode("utf-8")).decode("ascii")
    rc_aud, aud_out = run_user(
        f"XDG_RUNTIME_DIR={runtime_dir} WAYLAND_DISPLAY={wayland_sock} "
        f"ferrion-run -- /usr/bin/pfr1-loupe-audit -c \"$(echo {audit_b64} | base64 -d)\" </dev/null 2>&1 | cat",
        timeout=60,
    )
    m_sch = re.search(r"SCHEMES:SCHEMES (.*)", aud_out)
    schemes = set(m_sch.group(1).split()) if m_sch else set()
    caps = dict(re.findall(r"CAP (\S+) (REACHED|ABSENT|SILENT)", aud_out))
    m_npic = re.search(r"NEST_PIC_RC:(\d+)", aud_out)
    m_ndoc = re.search(r"NEST_DOC_RC:(\d+)", aud_out)
    m_ndoc_out = re.search(r"NEST_DOC_OUT:(.*)", aud_out)
    neg_results["manifest_audit"] = {
        "rc": rc_aud,
        "schemes": sorted(schemes),
        "caps": caps,
        "nested_pictures_rc": int(m_npic.group(1)) if m_npic else None,
        "nested_documents_rc": int(m_ndoc.group(1)) if m_ndoc else None,
        "nested_documents_out": m_ndoc_out.group(1).strip() if m_ndoc_out else "",
    }
    record["negatives"] = neg_results

    run_user("kill $(cat /tmp/lm-ctrl.pid 2>/dev/null) 2>/dev/null || true", timeout=15)

    audit_problems = []
    if rc_aud != 0 or not m_sch:
        audit_problems.append(f"manifest audit failed (rc={rc_aud}): {aud_out!r}")
    if schemes & {"tcp", "ip", "icmp", "udp"}:
        audit_problems.append(f"Loupe manifest unexpectedly granted network schemes: {sorted(schemes)}")
    if "audio" in schemes:
        audit_problems.append(f"Loupe manifest unexpectedly granted audio scheme: {sorted(schemes)}")
    if caps.get("net") != "ABSENT":
        audit_problems.append(f"TCP was not absent under Loupe manifest: caps={caps!r}")
    if caps.get("audio-playback") != "ABSENT":
        audit_problems.append(f"audio was not absent under Loupe manifest: caps={caps!r}")
    if caps.get("docs") != "ABSENT":
        audit_problems.append(f"Documents was not absent under Loupe manifest: caps={caps!r}")
    if caps.get("pics") != "REACHED":
        audit_problems.append(f"Pictures was not reachable under Loupe manifest: caps={caps!r}")
    if not m_npic or int(m_npic.group(1)) != 0:
        audit_problems.append(f"nested --read-only ~/Pictures/loupe-album failed: {aud_out!r}")
    if not m_ndoc or int(m_ndoc.group(1)) != 125 or "wider-than-parent" not in (m_ndoc_out.group(1) if m_ndoc_out else ""):
        audit_problems.append(f"nested --read-only ~/Documents did not exit 125 wider-than-parent: {aud_out!r}")
    if audit_problems:
        save_record()
        return False, "; ".join(audit_problems) + f"; see {vm.out}"

    save_record()
    svg_ms = next((f["elapsed_ms"] for f in formats_results if f["format"] == "svg"), None)
    return True, (
        f"Loupe under installed manifest (display, files-pictures; GSK_RENDERER=cairo, gpu open gap): "
        f"8/8 formats paged via Right arrow (svg={svg_ms}ms), 24MP={mp24_ms}ms, "
        f"nested loaders observed ({nested_obs['saw_loader']}, ferrion-run={nested_obs['max_ferrion_run']}, "
        f"ctrl=ok), show-properties persisted in app home (memory backend did not), "
        f"Documents/secret.png & secret.svg absent, nested ~/Documents 125 wider-than-parent, "
        f"no tcp/audio; see {vm.out}"
    )
