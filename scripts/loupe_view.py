"""M3c check: Loupe 51.2 on wm-session, 8 formats + arrow paging, 24 MP timing, and GSettings persistence."""
import http.server
import json
import re
import shutil
import subprocess
import tempfile
import threading
import time
from pathlib import Path

COLOR_TOLERANCE = 18
MIN_IMAGE_PIXELS = 15000
MAX_HTTP_REQUESTS = 128
WAYLAND_SOCKET_WAIT_ITERS = 30
WINDOW_POLL_ITERS = 80
CLOSE_POLL_ITERS = 30
MP24_POLL_ITERS = 120
SIDEBAR_MIN_DIFF = 18.0
SIDEBAR_MAX_MATCH_DIFF = 12.0

ALBUM_SPEC = (
    ("01_png.png", "png", (220, 40, 40)),
    ("02_jpg.jpg", "jpeg", (40, 200, 40)),
    ("03_webp.webp", "webp", (40, 60, 220)),
    ("04_tiff.tiff", "tiff", (230, 210, 40)),
    ("05_avif.avif", "avif", (210, 40, 210)),
    ("06_heif.heif", "heif-av1", (40, 210, 210)),
    ("07_jxl.jxl", "jxl", (230, 130, 30)),
    ("08_svg.svg", "svg", (130, 50, 220)),
)
MP24_RGB = (220, 40, 120)


def make_solid_ppm(path, rgb, width=256, height=256):
    r, g, b = rgb
    header = f"P6\n{width} {height}\n255\n".encode("ascii")
    path.write_bytes(header + bytes((r, g, b)) * (width * height))


def run_host(cmd):
    res = subprocess.run(cmd, capture_output=True, text=True)
    if res.returncode != 0:
        raise RuntimeError(f"host command failed ({res.returncode}): {cmd}\n{res.stderr}")
    return res


def generate_fixtures(tmp_dir):
    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        raise RuntimeError("host lacks ffmpeg for M3c fixture generation")
    avifenc = shutil.which("avifenc")
    cjxl = shutil.which("cjxl")
    heif_enc = shutil.which("heif-enc")

    fixtures = {}
    for filename, fmt, rgb in ALBUM_SPEC:
        ppm = tmp_dir / f"{filename}.ppm"
        out_path = tmp_dir / filename
        if fmt == "svg":
            r, g, b = rgb
            svg = (
                '<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">\n'
                f'  <rect x="0" y="0" width="256" height="256" fill="#{r:02x}{g:02x}{b:02x}"/>\n'
                "</svg>\n"
            )
            fixtures[filename] = svg.encode("utf-8")
            continue
        make_solid_ppm(ppm, rgb, 256, 256)
        if fmt == "png":
            run_host([ffmpeg, "-y", "-f", "image2", "-i", str(ppm), "-c:v", "png", str(out_path)])
        elif fmt == "jpeg":
            run_host([ffmpeg, "-y", "-f", "image2", "-i", str(ppm), "-c:v", "mjpeg", "-q:v", "2", str(out_path)])
        elif fmt == "webp":
            run_host([ffmpeg, "-y", "-f", "image2", "-i", str(ppm), "-c:v", "libwebp", "-lossless", "1", str(out_path)])
        elif fmt == "tiff":
            run_host([ffmpeg, "-y", "-f", "image2", "-i", str(ppm), "-c:v", "tiff", str(out_path)])
        elif fmt in ("avif", "heif-av1"):
            src_png = tmp_dir / f"{filename}.src.png"
            run_host([ffmpeg, "-y", "-f", "image2", "-i", str(ppm), "-c:v", "png", str(src_png)])
            if avifenc:
                run_host([avifenc, "--lossless", str(src_png), str(out_path)])
            elif heif_enc:
                run_host([heif_enc, "--avif", "-L", str(src_png), "-o", str(out_path)])
            else:
                raise RuntimeError("host lacks avifenc and heif-enc")
        elif fmt == "jxl":
            src_png = tmp_dir / f"{filename}.src.png"
            run_host([ffmpeg, "-y", "-f", "image2", "-i", str(ppm), "-c:v", "png", str(src_png)])
            if cjxl:
                run_host([cjxl, "-d", "0", str(src_png), str(out_path)])
            else:
                run_host([ffmpeg, "-y", "-f", "image2", "-i", str(ppm), "-c:v", "libjxl", str(out_path)])
        fixtures[filename] = out_path.read_bytes()

    r, g, b = MP24_RGB
    mp24_path = tmp_dir / "large_24mp.png"
    run_host([
        ffmpeg, "-y", "-f", "lavfi",
        "-i", f"color=c=#{r:02x}{g:02x}{b:02x}:s=6000x4000:d=0.04,format=rgba",
        "-frames:v", "1", "-c:v", "png", str(mp24_path),
    ])
    fixtures["large_24mp.png"] = mp24_path.read_bytes()
    return fixtures


def start_fixture_server(fixtures):
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
            name = self.path.lstrip("/")
            data = fixtures.get(name)
            if data is None:
                self.send_response(404)
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, format, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


def count_rgb_box(frame, target_rgb, tol=COLOR_TOLERANCE, step=2):
    tr, tg, tb = target_rgb
    w = frame.width
    h = frame.height
    rgb = frame.rgb
    hits = 0
    for y in range(60, h - 30, step):
        row = y * w * 3
        for x in range(20, w - 20, step):
            i = row + x * 3
            if (
                abs(rgb[i] - tr) <= tol
                and abs(rgb[i + 1] - tg) <= tol
                and abs(rgb[i + 2] - tb) <= tol
            ):
                hits += 1
    return hits * step * step


def window_rect(win, frame_w, frame_h):
    layout = win.get("layout") if isinstance(win, dict) else None
    if isinstance(layout, dict):
        pos = layout.get("tile_pos_in_workspace_view")
        size = layout.get("window_size")
        if (
            isinstance(pos, list)
            and len(pos) == 2
            and isinstance(size, list)
            and len(size) == 2
        ):
            x = max(0, int(pos[0]))
            y = max(0, int(pos[1]))
            w = max(1, min(frame_w - x, int(size[0])))
            h = max(1, min(frame_h - y, int(size[1])))
            return x, y, w, h
    return 0, 0, frame_w, frame_h


def sidebar_mean_abs_diff(frame_a, frame_b, rect=None, step=4):
    w = min(frame_a.width, frame_b.width)
    h = min(frame_a.height, frame_b.height)
    if rect is not None:
        rx, ry, rw, rh = rect
        x0 = max(rx, rx + rw - 240)
        x1 = min(w, max(x0 + 1, rx + rw - 20))
        y0 = min(h - 1, ry + 60)
        y1 = min(h, max(y0 + 1, ry + rh - 30))
    else:
        x0 = max(0, w - 240)
        x1 = max(x0 + 1, w - 20)
        y0 = 80
        y1 = max(y0 + 1, h - 40)
    ra = frame_a.rgb
    rb = frame_b.rgb
    total = 0
    count = 0
    for y in range(y0, y1, step):
        row_a = y * frame_a.width * 3
        row_b = y * frame_b.width * 3
        for x in range(x0, x1, step):
            ia = row_a + x * 3
            ib = row_b + x * 3
            total += abs(ra[ia] - rb[ib]) + abs(ra[ia + 1] - rb[ib + 1]) + abs(ra[ia + 2] - rb[ib + 2])
            count += 3
    return round(total / max(1, count), 2)


def parse_json_array(text):
    for match in re.finditer(r"\[", text):
        try:
            value, _ = json.JSONDecoder().raw_decode(text[match.start():])
            if isinstance(value, list):
                return value
        except json.JSONDecodeError:
            continue
    return []


def run_loupe_view(vm, args, helpers):
    Blocked = helpers["Blocked"]
    sendkeys = helpers["sendkeys"]
    settle = helpers["settle"]

    if not shutil.which("ffmpeg"):
        raise Blocked("host lacks ffmpeg")
    if not (shutil.which("avifenc") or shutil.which("heif-enc")):
        raise Blocked("host lacks avifenc/heif-enc")

    record = {
        "launch_path": (
            "ferrion-wm msg action spawn (unsandboxed Loupe in ferrion-wm session; "
            "glycin loaders sandboxed under ferrion-run)"
        ),
        "renderer_requested": "cairo",
        "undo_trash_known_gap": (
            "Undo-trash depends on desktop GIO trash integration not yet built (Q9)."
        ),
    }

    def save_record():
        (vm.out / "loupe-view.json").write_text(
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
        vm.type(f"bash -c '{line}; echo lv-end-{tag}-$((40+2))-$?'\n")
        found = vm.wait_serial(rf"(?s)\n(.*)lv-end-{tag}-42-(\d+)", timeout, since)
        if not found:
            timed_out = True
            return None, vm.text(since)
        return int(found.group(2)), found.group(1)

    rc_bin, _ = run_user("command -v loupe", timeout=30)
    if rc_bin != 0:
        record["failure"] = "loupe not installed"
        save_record()
        return False, f"loupe is not installed in guest; see {vm.out}"

    rc_man, _ = run_user("test -s /usr/share/ferrion/apps/loupe.toml", timeout=15)
    record["manifest_installed"] = rc_man == 0
    if rc_man != 0:
        record["failure"] = "/usr/share/ferrion/apps/loupe.toml missing"
        save_record()
        return False, f"/usr/share/ferrion/apps/loupe.toml missing in guest; see {vm.out}"

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
        return False, (
            f"compiled GSettings schema check failed (gtk4={has_gtk_schema}, "
            f"loupe={has_loupe_schema}): {sc_out!r}; see {vm.out}"
        )

    rc_uid, uid_out = run_user('echo "UID:$(id -u)"', timeout=20)
    uid_match = re.search(r"UID:(\d+)", uid_out)
    if rc_uid != 0 or not uid_match:
        return False, f"could not determine user uid: rc={rc_uid} out={uid_out!r}"
    user_uid = int(uid_match.group(1))
    runtime_dir = f"/var/run/user/{user_uid}"
    record["user_uid"] = user_uid

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

    def spawn_loupe(target_path, env_file, log_file, gsettings_override=None):
        run_user(f"rm -f {env_file} {log_file}", timeout=15)
        wrapper = "/tmp/loupe-launch.sh"
        if gsettings_override is not None:
            script_body = (
                f"export GSETTINGS_BACKEND={gsettings_override}\n"
                f"env > {env_file}\n"
                f"exec env GSETTINGS_BACKEND={gsettings_override} GSK_RENDERER=cairo /usr/bin/loupe {target_path} > {log_file} 2>&1"
            )
        else:
            script_body = (
                f"env > {env_file}\n"
                f"exec env GSK_RENDERER=cairo /usr/bin/loupe {target_path} > {log_file} 2>&1"
            )
        rc_w, out_w = run_user(
            f"cat > {wrapper} <<EOF\n#!/bin/sh\n{script_body}\nEOF\nchmod +x {wrapper}",
            timeout=15,
        )
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

    with tempfile.TemporaryDirectory(prefix="ferrion-loupe-fixtures-") as tmp:
        fixtures = generate_fixtures(Path(tmp))
        server = start_fixture_server(fixtures)
        port = server.server_address[1]
        try:
            rc_mk, _ = run_user("rm -rf /tmp/loupe-album /tmp/loupe-24mp && mkdir -p /tmp/loupe-album /tmp/loupe-24mp", timeout=15)
            if rc_mk != 0:
                return False, f"failed creating guest fixture dirs; see {vm.out}"
            for filename, _, _ in ALBUM_SPEC:
                rc_dl, dl_out = run_user(
                    f"curl --http1.0 -fsS --max-time 45 -o /tmp/loupe-album/{filename} http://10.0.2.2:{port}/{filename}",
                    timeout=60,
                )
                if rc_dl != 0:
                    return False, f"failed fetching {filename}: {dl_out!r}; see {vm.out}"
            rc_dl, dl_out = run_user(
                f"curl --http1.0 -fsS --max-time 60 -o /tmp/loupe-24mp/large_24mp.png http://10.0.2.2:{port}/large_24mp.png",
                timeout=75,
            )
            if rc_dl != 0:
                return False, f"failed fetching large_24mp.png: {dl_out!r}; see {vm.out}"
        finally:
            server.shutdown()
            server.server_close()

    run_user("rm -f ~/.config/glib-2.0/settings/keyfile", timeout=15)

    album_env = "/tmp/loupe-album.env"
    album_log = "/tmp/loupe-album.log"
    t_first = time.monotonic()
    spawn_loupe("/tmp/loupe-album/01_png.png", album_env, album_log)

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
        for _ in range(WINDOW_POLL_ITERS):
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
            record["formats"] = formats_results
            record["album_log"] = log_txt
            save_record()
            return False, (
                f"Loupe did not render {filename} ({fmt}): pixels={pixels}, "
                f"title={win_title!r}; log={log_txt!r}; see {vm.out}"
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
        return False, f"Loupe logged GLib-GIO-CRITICAL: {album_log_txt!r}; see {vm.out}"

    if not close_loupe_windows():
        save_record()
        return False, f"Loupe window did not close after album check; see {vm.out}"

    run_user("rm -f ~/.config/glib-2.0/settings/keyfile", timeout=15)
    green_env = "/tmp/loupe-green.env"
    green_log = "/tmp/loupe-green.log"
    spawn_loupe("/tmp/loupe-album/01_png.png", green_env, green_log)
    green_win = None
    for _ in range(WINDOW_POLL_ITERS):
        wins = query_loupe_windows()
        frame = vm.screenshot()
        if wins and count_rgb_box(frame, ALBUM_SPEC[0][2]) >= MIN_IMAGE_PIXELS:
            green_win = wins[0]
            break
        time.sleep(0.25)
    closed_frame = settle(vm, 5, still=0.8)
    closed_frame.save_png(vm.out / "gsettings-green-closed.png")
    green_rect = window_rect(green_win, closed_frame.width, closed_frame.height)

    vm.command("sendkey f9")
    open_frame = None
    open_diff = 0.0
    keyfile_txt = ""
    for _ in range(WINDOW_POLL_ITERS):
        _, keyfile_txt = run_user("cat ~/.config/glib-2.0/settings/keyfile 2>/dev/null || true", timeout=15)
        frame = vm.screenshot()
        diff = sidebar_mean_abs_diff(closed_frame, frame, green_rect)
        if "show-properties=true" in keyfile_txt and diff >= SIDEBAR_MIN_DIFF:
            open_frame = settle(vm, 5, still=0.8)
            open_diff = sidebar_mean_abs_diff(closed_frame, open_frame, green_rect)
            open_frame.save_png(vm.out / "gsettings-green-open.png")
            break
        time.sleep(0.25)
    if open_frame is None:
        vm.screenshot().save_png(vm.out / "gsettings-green-f9-fail.png")
        record["gsettings_green"] = {"keyfile": keyfile_txt, "open_diff": open_diff, "rect": list(green_rect)}
        save_record()
        return False, f"F9 did not open properties sidebar / write keyfile: {keyfile_txt!r}; see {vm.out}"

    if not close_loupe_windows():
        return False, f"Loupe did not close before green restart; see {vm.out}"

    green2_env = "/tmp/loupe-green2.env"
    green2_log = "/tmp/loupe-green2.log"
    spawn_loupe("/tmp/loupe-album/01_png.png", green2_env, green2_log)
    reopened_frame = None
    for _ in range(WINDOW_POLL_ITERS):
        wins = query_loupe_windows()
        frame = vm.screenshot()
        if wins and count_rgb_box(frame, ALBUM_SPEC[0][2]) >= MIN_IMAGE_PIXELS:
            reopened_frame = settle(vm, 5, still=0.8)
            break
        time.sleep(0.25)
    if reopened_frame is None:
        return False, f"Loupe did not reopen on green restart; see {vm.out}"
    reopened_frame.save_png(vm.out / "gsettings-green-reopened.png")
    diff_reopened_vs_closed = sidebar_mean_abs_diff(reopened_frame, closed_frame, green_rect)
    diff_reopened_vs_open = sidebar_mean_abs_diff(reopened_frame, open_frame, green_rect)
    record["gsettings_green"] = {
        "session_backend": session_backend,
        "keyfile": keyfile_txt.strip(),
        "window_rect": list(green_rect),
        "open_vs_closed_diff": open_diff,
        "reopened_vs_closed_diff": diff_reopened_vs_closed,
        "reopened_vs_open_diff": diff_reopened_vs_open,
    }
    if diff_reopened_vs_closed < SIDEBAR_MIN_DIFF or diff_reopened_vs_open > SIDEBAR_MAX_MATCH_DIFF:
        save_record()
        return False, (
            f"show-properties did not survive restart under GSETTINGS_BACKEND=keyfile "
            f"(vs_closed={diff_reopened_vs_closed}, vs_open={diff_reopened_vs_open}); see {vm.out}"
        )
    if not close_loupe_windows():
        return False, f"Loupe did not close after green restart; see {vm.out}"

    run_user("rm -f ~/.config/glib-2.0/settings/keyfile", timeout=15)
    red_env = "/tmp/loupe-red.env"
    red_log = "/tmp/loupe-red.log"
    spawn_loupe("/tmp/loupe-album/01_png.png", red_env, red_log, gsettings_override="memory")
    red_win = None
    for _ in range(WINDOW_POLL_ITERS):
        wins = query_loupe_windows()
        frame = vm.screenshot()
        if wins and count_rgb_box(frame, ALBUM_SPEC[0][2]) >= MIN_IMAGE_PIXELS:
            red_win = wins[0]
            break
        time.sleep(0.25)
    red_closed = settle(vm, 5, still=0.8)
    red_closed.save_png(vm.out / "gsettings-red-closed.png")
    red_rect = window_rect(red_win, red_closed.width, red_closed.height)
    _, red_env_txt = run_user(f"cat {red_env} 2>/dev/null || true", timeout=15)
    m_red_env = re.search(r"^GSETTINGS_BACKEND=(\S+)$", red_env_txt, re.MULTILINE)
    red_backend = m_red_env.group(1) if m_red_env else "<unset>"
    if red_backend != "memory":
        save_record()
        return False, f"red check expected GSETTINGS_BACKEND=memory, got {red_backend!r}; see {vm.out}"

    vm.command("sendkey f9")
    red_open = None
    for _ in range(WINDOW_POLL_ITERS):
        frame = vm.screenshot()
        if sidebar_mean_abs_diff(red_closed, frame, red_rect) >= SIDEBAR_MIN_DIFF:
            red_open = settle(vm, 5, still=0.8)
            red_open.save_png(vm.out / "gsettings-red-open.png")
            break
        time.sleep(0.25)
    if red_open is None:
        return False, f"F9 did not toggle properties sidebar during memory-backend check; see {vm.out}"
    _, red_keyfile_txt = run_user("cat ~/.config/glib-2.0/settings/keyfile 2>/dev/null || true", timeout=15)

    if not close_loupe_windows():
        return False, f"Loupe did not close before red restart; see {vm.out}"

    red2_env = "/tmp/loupe-red2.env"
    red2_log = "/tmp/loupe-red2.log"
    spawn_loupe("/tmp/loupe-album/01_png.png", red2_env, red2_log, gsettings_override="memory")
    red_reopened = None
    for _ in range(WINDOW_POLL_ITERS):
        wins = query_loupe_windows()
        frame = vm.screenshot()
        if wins and count_rgb_box(frame, ALBUM_SPEC[0][2]) >= MIN_IMAGE_PIXELS:
            red_reopened = settle(vm, 5, still=0.8)
            break
        time.sleep(0.25)
    if red_reopened is None:
        return False, f"Loupe did not reopen on red restart; see {vm.out}"
    red_reopened.save_png(vm.out / "gsettings-red-reopened.png")
    red_vs_closed = sidebar_mean_abs_diff(red_reopened, red_closed, red_rect)
    red_vs_open = sidebar_mean_abs_diff(red_reopened, red_open, red_rect)
    record["gsettings_red_memory"] = {
        "backend_env": red_backend,
        "why_memory_not_unset": (
            "GLib 2.87 registers keyfile at priority 10 after memory at priority 10, "
            "so unset GSETTINGS_BACKEND still picks keyfile when dconf is absent."
        ),
        "keyfile_after_f9": red_keyfile_txt.strip(),
        "reopened_vs_closed_diff": red_vs_closed,
        "reopened_vs_open_diff": red_vs_open,
    }
    if red_vs_closed > SIDEBAR_MAX_MATCH_DIFF or red_vs_open < SIDEBAR_MIN_DIFF or "show-properties=true" in red_keyfile_txt:
        save_record()
        return False, (
            f"show-properties unexpectedly persisted under GSETTINGS_BACKEND=memory "
            f"(vs_closed={red_vs_closed}, vs_open={red_vs_open}); see {vm.out}"
        )
    if not close_loupe_windows():
        return False, f"Loupe did not close after red restart; see {vm.out}"

    mp24_env = "/tmp/loupe-24mp.env"
    mp24_log = "/tmp/loupe-24mp.log"
    t_mp24 = time.monotonic()
    spawn_loupe("/tmp/loupe-24mp/large_24mp.png", mp24_env, mp24_log)
    mp24_ms = None
    mp24_pixels = 0
    for _ in range(MP24_POLL_ITERS):
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
        "handoff": "96 MB RGBA pipe copy from sandboxed glycin-image-rs loader",
        "log": mp24_log_txt,
    }
    if mp24_ms is None or "GLib-GIO-CRITICAL" in mp24_log_txt:
        vm.screenshot().save_png(vm.out / "loupe-24mp-fail.png")
        save_record()
        return False, f"Loupe 24 MP image failed (ms={mp24_ms}, px={mp24_pixels}): {mp24_log_txt!r}; see {vm.out}"
    close_loupe_windows()

    save_record()
    svg_ms = next((f["elapsed_ms"] for f in formats_results if f["format"] == "svg"), None)
    return True, (
        f"Loupe 51.2 via ferrion-wm msg action spawn (unsandboxed app, sandboxed glycin loaders): "
        f"8/8 formats paged via Right arrow (svg={svg_ms}ms), 24MP pipe-copy first frame={mp24_ms}ms, "
        f"GSETTINGS_BACKEND={session_backend} F9 survived restart (memory backend did not), "
        f"0 GLib-GIO-CRITICAL, glycin vendor absent; undo-trash known gap (Q9); see {vm.out}"
    )
