"""M3a check: GTK 4 / libadwaita Wayland window, key delivery, and AppStream negative check."""
import http.server
import json
import re
import shutil
import subprocess
import tempfile
import threading
import time
from pathlib import Path

COLOR_TOLERANCE = 4
MIN_WINDOW_PIXELS = 40000
MAX_HTTP_REQUESTS = 64
PROBE_POLL_ITERS = 60
PROBE_EXIT_POLL_ITERS = 30
WAYLAND_SOCKET_WAIT_ITERS = 30


def rgb_matches(rgb_bytes, target_rgb, tol=COLOR_TOLERANCE):
    tr, tg, tb = target_rgb
    return {
        bytes((r, g, b))
        for r in range(max(0, tr - tol), min(255, tr + tol) + 1)
        for g in range(max(0, tg - tol), min(255, tg + tol) + 1)
        for b in range(max(0, tb - tol), min(255, tb + tol) + 1)
    }


def count_color(frame, palette):
    return sum(
        1
        for i in range(0, len(frame.rgb), 3)
        if frame.rgb[i : i + 3] in palette
    )


def start_fixture_server(payload):
    requests_log = []
    lock = threading.Lock()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            with lock:
                if len(requests_log) < MAX_HTTP_REQUESTS:
                    requests_log.append({
                        "method": "GET",
                        "path": self.path,
                        "time": round(time.monotonic(), 3),
                    })
            if self.path == "/about-fixture.gresource":
                data = payload["bytes"]
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(data)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(data)
                return
            if self.path == "/releases.xml":
                body = b'<?xml version="1.0" encoding="UTF-8"?><releases><release version="1.0.0" date="2026-11-24"/></releases>\n'
                self.send_response(200)
                self.send_header("Content-Type", "application/xml")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(body)
                return
            self.send_response(404)
            self.end_headers()

        def log_message(self, format, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, requests_log, lock


def build_about_gresource(port, out_dir):
    compiler = shutil.which("glib-compile-resources")
    if not compiler:
        raise RuntimeError("host lacks glib-compile-resources")
    with tempfile.TemporaryDirectory(prefix="ferrion-gtk4-about-") as tmp:
        tmp_path = Path(tmp)
        metainfo = (
            '<?xml version="1.0" encoding="UTF-8"?>\n'
            '<component type="desktop-application">\n'
            '  <id>org.ferrionos.Gtk4WindowProbe</id>\n'
            '  <name>FerrionOS GTK4 Probe</name>\n'
            '  <summary>GTK 4 and libadwaita Wayland probe</summary>\n'
            '  <metadata_license>CC0-1.0</metadata_license>\n'
            '  <project_license>MIT</project_license>\n'
            '  <developer id="org.ferrionos"><name>FerrionOS</name></developer>\n'
            f'  <releases type="external" url="http://10.0.2.2:{port}/releases.xml"/>\n'
            '</component>\n'
        )
        (tmp_path / "org.ferrionos.Gtk4WindowProbe.metainfo.xml").write_text(metainfo, encoding="utf-8")
        gresource_xml = (
            '<?xml version="1.0" encoding="UTF-8"?>\n'
            '<gresources>\n'
            '  <gresource prefix="/org/ferrionos/Gtk4WindowProbe">\n'
            '    <file>org.ferrionos.Gtk4WindowProbe.metainfo.xml</file>\n'
            '  </gresource>\n'
            '</gresources>\n'
        )
        (tmp_path / "probe.gresource.xml").write_text(gresource_xml, encoding="utf-8")
        target = out_dir / "about-fixture.gresource"
        subprocess.run(
            [
                compiler,
                f"--sourcedir={tmp_path}",
                f"--target={target}",
                str(tmp_path / "probe.gresource.xml"),
            ],
            check=True,
        )
        return target.read_bytes()


def run_gtk4_window(vm, args, helpers):
    Blocked = helpers["Blocked"]
    sendkeys = helpers["sendkeys"]
    settle = helpers["settle"]

    if not shutil.which("glib-compile-resources"):
        raise Blocked("host lacks glib-compile-resources")

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
        vm.type(f"bash -c '{line}; echo g4-end-{tag}-$((40+2))-$?'\n")
        found = vm.wait_serial(rf"(?s)\n(.*)g4-end-{tag}-42-(\d+)", timeout, since)
        if not found:
            timed_out = True
            return None, vm.text(since)
        return int(found.group(2)), found.group(1)

    code, _ = run_user("command -v gtk4-window-probe", timeout=30)
    if code != 0:
        record["failure"] = "gtk4-window-probe not installed"
        (vm.out / "gtk4-window.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        return False, f"gtk4-window-probe is not installed in guest; see {vm.out}"

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

    blue_pal = rgb_matches(None, (0x24, 0x68, 0xAC))
    green_pal = rgb_matches(None, (0x22, 0xAA, 0x44))

    probe_log = "/tmp/gtk4-probe.log"
    probe_rc = "/tmp/gtk4-probe.rc"
    launch_cmd = (
        f"rm -f {probe_log} {probe_rc}; "
        f"(XDG_RUNTIME_DIR={runtime_dir} WAYLAND_DISPLAY={wayland_sock} "
        f"GDK_BACKEND=wayland GSK_RENDERER=cairo gtk4-window-probe > {probe_log} 2>&1; "
        f"echo $? > {probe_rc}) </dev/null >/dev/null 2>&1 & true"
    )
    rc_l, out_l = run_user(launch_cmd, timeout=20)
    if rc_l != 0:
        return False, f"failed to start gtk4-window-probe (rc={rc_l}): {out_l!r}; see {vm.out}"

    blue_matched = False
    blue_pixels = 0
    log_text = ""
    for _ in range(PROBE_POLL_ITERS):
        _, log_text = run_user(f"cat {probe_log} 2>/dev/null || true", timeout=15)
        frame = vm.screenshot()
        blue_pixels = count_color(frame, blue_pal)
        if blue_pixels >= MIN_WINDOW_PIXELS and "GTK4-WINDOW: display=" in log_text:
            blue_matched = True
            frame.save_png(vm.out / "gtk4-blue.png")
            break
        rc_done, _ = run_user(f"test -f {probe_rc}", timeout=10)
        if rc_done == 0:
            _, log_text = run_user(f"cat {probe_log} 2>/dev/null || true", timeout=15)
            _, rc_text = run_user(f"cat {probe_rc} 2>/dev/null || true", timeout=10)
            record["early_exit_rc"] = rc_text.strip()
            frame.save_png(vm.out / "gtk4-early-exit.png")
            break
        time.sleep(0.5)

    record["blue_pixels"] = blue_pixels
    record["window_log_initial"] = log_text
    if not blue_matched:
        (vm.out / "gtk4-window.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        return False, f"GTK4 window did not paint blue ({blue_pixels} px); log={log_text!r}; see {vm.out}"

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
        green_pixels = count_color(frame, green_pal)
        if green_pixels >= MIN_WINDOW_PIXELS and "GTK4-WINDOW: key=k" in log_text:
            green_matched = True
            frame.save_png(vm.out / "gtk4-green.png")
            break
        time.sleep(0.5)

    record["green_pixels"] = green_pixels
    if not green_matched:
        frame.save_png(vm.out / "gtk4-no-key.png")
        record["window_log_after_key"] = log_text
        (vm.out / "gtk4-window.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        return False, f"key 'k' did not reach GTK4 window (green_px={green_pixels}); log={log_text!r}; see {vm.out}"

    vm.command("sendkey q")
    exit_code = None
    for _ in range(PROBE_EXIT_POLL_ITERS):
        rc_f, out_f = run_user(f"cat {probe_rc} 2>/dev/null || true", timeout=10)
        lines = [l.strip() for l in out_f.splitlines() if l.strip().isdigit()]
        if rc_f == 0 and lines:
            exit_code = int(lines[-1])
            break
        time.sleep(0.5)
    _, final_log = run_user(f"cat {probe_log} 2>/dev/null || true", timeout=15)
    record["window_exit_code"] = exit_code
    record["window_log_final"] = final_log
    if exit_code != 0 or "GTK4-WINDOW: closed" not in final_log:
        (vm.out / "gtk4-window.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        return False, f"GTK4 window did not exit 0 cleanly (rc={exit_code}); log={final_log!r}"
    if "GLib-GIO-CRITICAL" in final_log:
        (vm.out / "gtk4-window.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        return False, f"GTK4 window logged GLib-GIO-CRITICAL (missing gschemas.compiled?): {final_log!r}"

    closed = settle(vm, 5, still=1.0)
    closed.save_png(vm.out / "gtk4-closed.png")
    rem_green = count_color(closed, green_pal)
    rem_blue = count_color(closed, blue_pal)
    record["remaining_pixels_after_close"] = {"blue": rem_blue, "green": rem_green}
    if rem_green > 200 or rem_blue > 200:
        return False, f"GTK4 window left pixels on screen after exit: blue={rem_blue}, green={rem_green}"

    rc_net, _ = run_user(
        'deadline=$((SECONDS+60)); server=$(cat /etc/net/dns); '
        'while [[ -z "$server" || "$server" == 9.9.9.9 ]] && (( SECONDS < deadline )); do '
        'sleep 0.1; server=$(cat /etc/net/dns); done; '
        '[[ -n "$server" && "$server" != 9.9.9.9 ]]',
        timeout=70,
    )
    if rc_net != 0:
        return False, f"DHCP did not finish before AppStream about check; see {vm.out}"

    payload = {"bytes": b""}
    server, req_log, req_lock = start_fixture_server(payload)
    port = server.server_address[1]
    payload["bytes"] = build_about_gresource(port, vm.out)
    try:
        res_path = "/org/ferrionos/Gtk4WindowProbe/org.ferrionos.Gtk4WindowProbe.metainfo.xml"
        guest_gres = "/tmp/about-fixture.gresource"
        rc_dl, dl_out = run_user(
            f"curl --http1.0 -fsS --max-time 30 -o {guest_gres} http://10.0.2.2:{port}/about-fixture.gresource",
            timeout=45,
        )
        if rc_dl != 0:
            return False, f"failed to fetch about-fixture.gresource: {dl_out!r}"

        with req_lock:
            before_unsandboxed = sum(1 for r in req_log if r["path"] == "/releases.xml")

        rc_u, out_u = run_user(
            f"XDG_RUNTIME_DIR={runtime_dir} WAYLAND_DISPLAY={wayland_sock} "
            f"GDK_BACKEND=wayland GSK_RENDERER=cairo "
            f"gtk4-window-probe --about-gresource {guest_gres} {res_path} --present 2>&1",
            timeout=45,
        )
        with req_lock:
            after_unsandboxed = sum(1 for r in req_log if r["path"] == "/releases.xml")
        record["about_unsandboxed"] = {
            "rc": rc_u,
            "releases_requests": after_unsandboxed - before_unsandboxed,
            "output": out_u,
        }
        if rc_u != 0 or "GTK4-ABOUT:" not in out_u:
            (vm.out / "gtk4-window.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
            return False, f"unsandboxed AdwAboutDialog failed (rc={rc_u}): {out_u!r}"
        if (after_unsandboxed - before_unsandboxed) != 0:
            (vm.out / "gtk4-window.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
            return False, f"unsandboxed AdwAboutDialog made {after_unsandboxed - before_unsandboxed} release requests"
        if "GLib-GIO-CRITICAL" in out_u:
            (vm.out / "gtk4-window.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
            return False, f"unsandboxed AdwAboutDialog logged GLib-GIO-CRITICAL: {out_u!r}"

        with req_lock:
            before_appstream_net = sum(1 for r in req_log if r["path"] == "/releases.xml")

        rc_an, out_an = run_user(
            f"gtk4-window-probe --appstream-releases {guest_gres} {res_path} 2>&1",
            timeout=45,
        )
        with req_lock:
            after_appstream_net = sum(1 for r in req_log if r["path"] == "/releases.xml")
        record["appstream_unsandboxed"] = {
            "rc": rc_an,
            "releases_requests": after_appstream_net - before_appstream_net,
            "output": out_an,
        }
        if (
            rc_an != 0
            or "APPSTREAM-RELEASES: loaded=1 count=1 version=1.0.0" not in out_an
            or (after_appstream_net - before_appstream_net) < 1
        ):
            (vm.out / "gtk4-window.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
            return False, (
                f"unsandboxed libappstream probe did not fetch /releases.xml "
                f"(rc={rc_an}, reqs={after_appstream_net - before_appstream_net}): {out_an!r}"
            )

        with req_lock:
            before_sandboxed = sum(1 for r in req_log if r["path"] == "/releases.xml")

        rc_s, out_s = run_user(
            f"ferrion-run --no-network --read-only {guest_gres} "
            f"-- gtk4-window-probe --appstream-releases {guest_gres} {res_path} </dev/null 2>&1",
            timeout=45,
        )
        with req_lock:
            after_sandboxed = sum(1 for r in req_log if r["path"] == "/releases.xml")
            all_reqs = list(req_log)
        record["appstream_sandboxed_no_network"] = {
            "rc": rc_s,
            "releases_requests": after_sandboxed - before_sandboxed,
            "output": out_s,
        }
        record["http_requests"] = all_reqs
        (vm.out / "gtk4-window.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")

        if rc_s != 0 or "APPSTREAM-RELEASES: loaded=0 count=0" not in out_s:
            return False, f"ferrion-run --no-network libappstream probe failed (rc={rc_s}): {out_s!r}"
        if (after_sandboxed - before_sandboxed) != 0:
            return False, f"ferrion-run --no-network leaked {after_sandboxed - before_sandboxed} HTTP requests"
        if re.search(r"(?i)(critical|glib-.*error|assertion.*failed)", out_s):
            return False, f"ferrion-run --no-network libappstream probe logged a GLib error: {out_s!r}"
    finally:
        server.shutdown()
        server.server_close()

    return True, (
        f"Wayland GTK4/Adwaita CPU ({record['renderer_type']}): blue={blue_pixels}px, "
        f"key 'k'->green={green_pixels}px, clean exit 0; "
        f"AdwAboutDialog req={record['about_unsandboxed']['releases_requests']}, "
        f"appstream unsandboxed req={record['appstream_unsandboxed']['releases_requests']}, "
        f"ferrion-run --no-network rc=0 req=0; see {vm.out}"
    )
