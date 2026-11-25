"""M2c-a check: mpv Wayland wlshm window + ferrion audio at 1080p and 4K AV1 on wm-session."""
import json
import os
import re
import shutil
import subprocess
import time
from pathlib import Path

import media_decode

CLIP_DURATION_S = 4.0
CLIP_FPS = 30
CLIP_FRAMES = int(CLIP_DURATION_S * CLIP_FPS)
CLIP_SPECS = (
    ("1080p", 1920, 1080, "av1_1080p_tone.mkv"),
    ("4k", 3840, 2160, "av1_4k_tone.mkv"),
)
COUNTER_BITS = 8
MIN_MARKER_SAMPLES = 8
WAYLAND_SOCKET_WAIT_ITERS = 30
WINDOW_POLL_ITERS = 80
EXIT_POLL_ITERS = 160
SCREENDUMP_MAX_SHOTS = 4
SCREENDUMP_INTERVAL_S = 0.65
BG_RGB = (15, 14, 12)
BG_TOLERANCE = 10
MIN_NON_BG_RATIO = 0.25
MIN_FRAME_DIFF = 1.5
MAX_LOG_LINES = 20000


def read_host_loadavg():
    raw = Path("/proc/loadavg").read_text(encoding="utf-8").strip()
    parts = raw.split()
    return {
        "raw": raw,
        "load1": float(parts[0]),
        "load5": float(parts[1]),
        "load15": float(parts[2]),
    }


def detect_av1_encoder():
    proc = subprocess.run(
        ["ffmpeg", "-hide_banner", "-encoders"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"ffmpeg -encoders failed ({proc.returncode})")
    text = proc.stdout + proc.stderr
    if "libsvtav1" in text:
        return "libsvtav1"
    if "libaom-av1" in text:
        return "libaom-av1"
    return None


def build_counter_vf():
    filters = [
        "drawbox=x=0:y=0:w=iw:h=ih*0.14:color=black@1.0:t=fill",
        "drawbox=x=iw*0.04:y=ih*0.02:w=iw*0.06:h=ih*0.10:color=0xFF00FF@1.0:t=fill",
        "drawbox=x=iw*0.90:y=ih*0.02:w=iw*0.06:h=ih*0.10:color=0xFF8000@1.0:t=fill",
    ]
    for bit in range(COUNTER_BITS):
        x_frac = 0.13 + bit * 0.0925
        divisor = 1 << bit
        filters.append(
            f"drawbox=x=iw*{x_frac:.4f}:y=ih*0.03:w=iw*0.0725:h=ih*0.08"
            f":color=white@1.0:t=fill:enable='mod(floor(n/{divisor}),2)'"
        )
    return ",".join(filters)


def generate_clip(path, width, height, encoder):
    vf = build_counter_vf()
    if encoder == "libsvtav1":
        vcodec_args = ["-c:v", "libsvtav1", "-preset", "10", "-crf", "32", "-g", str(CLIP_FPS)]
    else:
        vcodec_args = ["-c:v", "libaom-av1", "-cpu-used", "8", "-crf", "32", "-g", str(CLIP_FPS)]
    media_decode.run_host_ffmpeg(
        [
            "-f", "lavfi",
            "-i", f"testsrc2=size={width}x{height}:rate={CLIP_FPS}:duration={CLIP_DURATION_S}",
            "-f", "lavfi",
            "-i", f"sine=frequency={media_decode.SOUND_TONE_HZ}:sample_rate=48000:duration={CLIP_DURATION_S}",
            "-vf", vf,
            "-af", "volume=2.0,pan=stereo|c0=c0|c1=c0",
            "-map", "0:v:0",
            "-map", "1:a:0",
            "-pix_fmt", "yuv420p",
            *vcodec_args,
            "-c:a", "libopus",
            "-b:a", "192k",
            "-shortest",
            str(path),
        ],
        timeout=240,
    )
    return media_decode.sha256_file(path)


def parse_json_array(text):
    for match in re.finditer(r"\[", text):
        try:
            value, _ = json.JSONDecoder().raw_decode(text[match.start():])
            if isinstance(value, list):
                return value
        except json.JSONDecodeError:
            continue
    return []


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


def rect_non_bg_ratio(frame, rect, step=4):
    rx, ry, rw, rh = rect
    x1 = min(frame.width, rx + rw)
    y1 = min(frame.height, ry + rh)
    rgb = frame.rgb
    bg_r, bg_g, bg_b = BG_RGB
    non_bg = 0
    total = 0
    for y in range(ry, y1, step):
        row = y * frame.width * 3
        for x in range(rx, x1, step):
            i = row + x * 3
            total += 1
            if (
                abs(rgb[i] - bg_r) > BG_TOLERANCE
                or abs(rgb[i + 1] - bg_g) > BG_TOLERANCE
                or abs(rgb[i + 2] - bg_b) > BG_TOLERANCE
            ):
                non_bg += 1
    return round(non_bg / max(1, total), 4)


def rect_mean_abs_diff(frame_a, frame_b, rect, step=4):
    rx, ry, rw, rh = rect
    x1 = min(frame_a.width, frame_b.width, rx + rw)
    y1 = min(frame_a.height, frame_b.height, ry + rh)
    ra = frame_a.rgb
    rb = frame_b.rgb
    total = 0
    count = 0
    for y in range(ry, y1, step):
        row_a = y * frame_a.width * 3
        row_b = y * frame_b.width * 3
        for x in range(rx, x1, step):
            ia = row_a + x * 3
            ib = row_b + x * 3
            total += abs(ra[ia] - rb[ib]) + abs(ra[ia + 1] - rb[ib + 1]) + abs(ra[ia + 2] - rb[ib + 2])
            count += 3
    return round(total / max(1, count), 2)


def decode_frame_counter(frame, rect):
    rx, ry, rw, rh = rect
    x1 = min(frame.width, rx + rw)
    y1 = min(frame.height, ry + rh)
    rgb = frame.rgb
    left_limit = rx + rw // 4
    right_start = rx + (rw * 3) // 4
    mag_pts = []
    org_pts = []
    for y in range(ry, y1, 2):
        row = y * frame.width * 3
        for x in range(rx, left_limit, 2):
            i = row + x * 3
            r, g, b = rgb[i], rgb[i + 1], rgb[i + 2]
            if r > 175 and g < 80 and b > 175:
                mag_pts.append((x, y))
        for x in range(right_start, x1, 2):
            i = row + x * 3
            r, g, b = rgb[i], rgb[i + 1], rgb[i + 2]
            if r > 190 and 85 <= g <= 170 and b < 60:
                org_pts.append((x, y))
    if len(mag_pts) < MIN_MARKER_SAMPLES or len(org_pts) < MIN_MARKER_SAMPLES:
        return None
    mag_min_y = min(p[1] for p in mag_pts)
    org_min_y = min(p[1] for p in org_pts)
    mag_top = [p for p in mag_pts if p[1] <= mag_min_y + 60]
    org_top = [p for p in org_pts if p[1] <= org_min_y + 60]
    if len(mag_top) < MIN_MARKER_SAMPLES or len(org_top) < MIN_MARKER_SAMPLES:
        return None
    cx = sum(p[0] for p in mag_top) / len(mag_top)
    cy = sum(p[1] for p in mag_top) / len(mag_top)
    yx = sum(p[0] for p in org_top) / len(org_top)
    yy = sum(p[1] for p in org_top) / len(org_top)
    if yx <= cx + 40 or abs(cy - yy) > 20:
        return None
    y_mid = (cy + yy) / 2.0
    value = 0
    for bit in range(COUNTER_BITS):
        t = (0.16625 + bit * 0.0925 - 0.07) / 0.86
        bx = int(round(cx + t * (yx - cx)))
        by = int(round(y_mid))
        luma_sum = 0.0
        samples = 0
        for dy in range(-2, 3):
            py = by + dy
            if py < 0 or py >= frame.height:
                continue
            row = py * frame.width * 3
            for dx in range(-2, 3):
                px = bx + dx
                if px < 0 or px >= frame.width:
                    continue
                i = row + px * 3
                luma_sum += 0.299 * rgb[i] + 0.587 * rgb[i + 1] + 0.114 * rgb[i + 2]
                samples += 1
        if samples == 0:
            return None
        luma = luma_sum / samples
        if luma >= 170.0:
            value |= 1 << bit
        elif luma <= 85.0:
            pass
        else:
            return None
    return value


def parse_mpv_window_log(log_text):
    lines = log_text.splitlines()[:MAX_LOG_LINES]
    vo_line = None
    vo_display_line = None
    vo_scale_line = None
    vo_scaled = None
    vo_src = None
    vo_dst = None
    sws_line = None
    avsync_vals = []
    frame_drop_max = None
    dec_drop_max = None
    delayed_max = None
    clamped = None
    dropped_status = None

    for line in lines:
        if vo_line is None and "VO: [wlshm]" in line:
            vo_line = line.strip()
        if "[vo/wlshm] Video display:" in line:
            vo_display_line = line.strip()
            m_vd = re.search(r"Video display:\s*\([^)]*\)\s*(\d+x\d+)\s*->\s*\([^)]*\)\s*(\d+x\d+)", line)
            if m_vd:
                vo_src = m_vd.group(1)
                vo_dst = m_vd.group(2)
        if "[vo/wlshm] Video scale:" in line:
            vo_scale_line = line.strip()
        if sws_line is None and "swscaler:" in line:
            sws_line = line.strip()
        m_stat = re.search(
            r"MPVSTAT\s+av=(\S+)\s+drop=(\S+)\s+decdrop=(\S+)\s+delay=(\S+)\s+pos=(\S+)",
            line,
        )
        if m_stat:
            try:
                avsync_vals.append(float(m_stat.group(1)))
            except ValueError:
                pass
            try:
                d = int(m_stat.group(2))
                frame_drop_max = d if frame_drop_max is None else max(frame_drop_max, d)
            except ValueError:
                pass
            try:
                dd = int(m_stat.group(3))
                dec_drop_max = dd if dec_drop_max is None else max(dec_drop_max, dd)
            except ValueError:
                pass
            try:
                dl = int(m_stat.group(4))
                delayed_max = dl if delayed_max is None else max(delayed_max, dl)
            except ValueError:
                pass
        m_drop = re.search(r"Dropped:\s*(\d+)", line)
        if m_drop:
            val = int(m_drop.group(1))
            dropped_status = val if dropped_status is None else max(dropped_status, val)
        m_clamp = re.search(r"clamped samples:\s*(\d+)", line)
        if m_clamp:
            clamped = int(m_clamp.group(1))

    if vo_src and vo_dst:
        vo_scaled = vo_src != vo_dst

    avsync_summary = None
    if avsync_vals:
        avsync_summary = {
            "samples": len(avsync_vals),
            "min_s": round(min(avsync_vals), 4),
            "max_s": round(max(avsync_vals), 4),
            "max_abs_s": round(max(abs(v) for v in avsync_vals), 4),
            "last_s": round(avsync_vals[-1], 4),
        }

    return {
        "vo_wlshm": "VO: [wlshm]" in log_text,
        "vo_line": vo_line,
        "vo_display_line": vo_display_line,
        "vo_scale_line": vo_scale_line,
        "vo_src": vo_src,
        "vo_dst": vo_dst,
        "vo_scaled": vo_scaled,
        "sws_line": sws_line,
        "ao_ferrion": "AO: [ferrion]" in log_text,
        "eof": "Exiting... (End of file)" in log_text,
        "frame_drop_count": frame_drop_max if frame_drop_max is not None else (dropped_status or 0),
        "decoder_frame_drop_count": dec_drop_max if dec_drop_max is not None else 0,
        "vo_delayed_frame_count": delayed_max if delayed_max is not None else 0,
        "avsync": avsync_summary,
        "clamped_samples": clamped,
    }


def run_single_clip(args, clip_out, label, width, height, clip_name, clip_sha, clips_dir, helpers):
    audio_vm = helpers["audio_vm"]
    sendkeys = helpers["sendkeys"]
    settle = helpers["settle"]

    clip_out.mkdir(parents=True, exist_ok=True)
    guest_dir = clip_out / "guest-out"
    guest_dir.mkdir(parents=True, exist_ok=True)
    wav_path = clip_out / "audio.wav"
    wav_path.unlink(missing_ok=True)

    server = media_decode.start_clip_server(clips_dir, guest_dir)
    port = server.server_address[1]
    base = f"http://10.0.2.2:{port}"
    curl_post = "curl --http1.0 -H Expect: -fsS --max-time 30 --retry 2 --data-binary"
    curl_get = "curl --http1.0 -fsS --max-time 60 --retry 2"

    clip_record = {
        "label": label,
        "width": width,
        "height": height,
        "clip": clip_name,
        "clip_sha256": clip_sha,
        "launch_path": "ferrion-wm msg action spawn (unsandboxed mpv in ferrion-wm session)",
        "red_missing_wlshm": False,
        "problems": [],
    }

    try:
        vm = audio_vm(args, clip_out, wav_path)
        try:
            deadline = time.monotonic() + args.timeout
            login_frame = None
            while time.monotonic() < deadline:
                frame = vm.screenshot()
                if frame.kind() == "text":
                    login_frame = settle(vm, 10, still=1.5)
                    break
                time.sleep(0.5)
            if login_frame is None:
                vm.screenshot().save_png(clip_out / "no-text-login.png")
                clip_record["problems"].append(f"no text login on VT 3 within {args.timeout}s")
                return clip_record

            if not vm.wait_serial(r"login: ", args.timeout):
                clip_record["problems"].append(f"no serial login prompt within {args.timeout}s")
                return clip_record
            vm.type("user\n")
            time.sleep(3)

            seq = 0
            timed_out = False

            def run_user(line, timeout=60):
                nonlocal seq, timed_out
                if timed_out:
                    return None, ""
                assert "'" not in line, f"single quote disallowed in bash -c payload: {line!r}"
                seq += 1
                tag = seq
                since = vm.mark()
                vm.type(f"bash -c '{line}; echo mw-end-{tag}-$((40+2))-$?'\n")
                found = vm.wait_serial(rf"(?s)\n(.*)mw-end-{tag}-42-(\d+)", timeout, since)
                if not found:
                    timed_out = True
                    return None, vm.text(since)
                return int(found.group(2)), found.group(1)

            rc_net, net_out = run_user(
                "deadline=$((SECONDS+60)); server=$(cat /etc/net/dns); "
                "while [[ -z \"$server\" || \"$server\" == 9.9.9.9 ]] && (( SECONDS < deadline )); do "
                "sleep 0.1; server=$(cat /etc/net/dns); done; "
                "[[ -n \"$server\" && \"$server\" != 9.9.9.9 ]]",
                timeout=70,
            )
            if rc_net != 0:
                clip_record["problems"].append(f"DHCP did not finish: {net_out.strip()[-200:]}")
                return clip_record

            rc_man, _ = run_user("test -s /usr/share/ferrion/apps/mpv.toml", timeout=15)
            clip_record["manifest_installed"] = rc_man == 0

            rc_help, help_out = run_user(
                "mkdir -p /tmp/media && mpv --vo=help > /tmp/media/vo-help.txt 2>&1 && "
                f"{curl_post} @/tmp/media/vo-help.txt {base}/vo-help.txt",
                timeout=45,
            )
            if rc_help != 0:
                clip_record["problems"].append(f"mpv --vo=help failed (rc={rc_help}): {help_out.strip()[-200:]}")
                return clip_record
            vo_help = (guest_dir / "vo-help.txt").read_text(encoding="utf-8", errors="replace")
            has_wlshm = any(line.strip().startswith("wlshm") for line in vo_help.splitlines())
            clip_record["vo_help_lists_wlshm"] = has_wlshm

            rc_dl, dl_out = run_user(
                f"{curl_get} {base}/{clip_name} -o /tmp/media/{clip_name} && "
                f"got=$(sha256sum /tmp/media/{clip_name} | cut -d\" \" -f1) && "
                f"test \"$got\" = \"{clip_sha}\"",
                timeout=120,
            )
            if rc_dl != 0:
                clip_record["problems"].append(f"download or sha256 failed for {clip_name}: {dl_out.strip()[-200:]}")
                return clip_record

            if not has_wlshm:
                rc_red, red_out = run_user(
                    f"rc=0; mpv --no-config --vo=wlshm --ao=ferrion --hwdec=no /tmp/media/{clip_name} "
                    f"> /tmp/media/red-wlshm.log 2>&1 || rc=$?; "
                    f"{curl_post} @/tmp/media/red-wlshm.log {base}/red-wlshm.log || true; "
                    "printf \"RED_RC=%d\\n\" \"$rc\"",
                    timeout=60,
                )
                m_red = re.search(r"(?m)^RED_RC=(\d+)\s*$", red_out or "")
                red_exit = int(m_red.group(1)) if m_red else rc_red
                red_log = ""
                red_path = guest_dir / "red-wlshm.log"
                if red_path.is_file():
                    red_log = red_path.read_text(encoding="utf-8", errors="replace").strip()
                red_head = "\n".join(red_log.splitlines()[:10])
                clip_record["red_missing_wlshm"] = True
                clip_record["red_wlshm_exit_code"] = red_exit
                clip_record["red_wlshm_log_head"] = red_head
                clip_record["problems"].append(
                    f"mpv --vo=help omits wlshm; --vo=wlshm rc={red_exit}: {red_head}"
                )
                return clip_record

            if not clip_record["manifest_installed"]:
                clip_record["problems"].append("/usr/share/ferrion/apps/mpv.toml missing")
                return clip_record

            rc_vol, vol_out = run_user("echo 100 > /scheme/audio/volume", timeout=30)
            if rc_vol != 0:
                clip_record["problems"].append(f"setting /scheme/audio/volume to 100 failed: {vol_out.strip()[-200:]}")
                return clip_record

            rc_uid, uid_out = run_user("echo \"UID:$(id -u)\"", timeout=20)
            uid_match = re.search(r"UID:(\d+)", uid_out or "")
            if rc_uid != 0 or not uid_match:
                clip_record["problems"].append(f"could not determine user uid: rc={rc_uid}")
                return clip_record
            runtime_dir = f"/var/run/user/{int(uid_match.group(1))}"

            sendkeys(vm.command, "user\n")
            wl_display = None
            wm_sock = None
            for _ in range(WAYLAND_SOCKET_WAIT_ITERS):
                rc_sock, sock_out = run_user(
                    f"for p in {runtime_dir}/wayland-* {runtime_dir}/ferrion-wm.*.sock; do "
                    "[ -e \"$p\" ] || continue; b=$(basename \"$p\"); "
                    "case \"$b\" in *.lock) ;; wayland-[0-9]*) echo \"WL:$b\";; ferrion-wm.*.sock) echo \"WM:$p\";; esac; done",
                    timeout=20,
                )
                m_wl = re.search(r"WL:(wayland-[0-9]+)\b", sock_out or "")
                m_wm = re.search(r"WM:(\S+\.sock)\b", sock_out or "")
                if rc_sock == 0 and m_wl and m_wm:
                    wl_display = m_wl.group(1)
                    wm_sock = m_wm.group(1)
                    break
                time.sleep(1.0)
            if not wl_display or not wm_sock:
                vm.screenshot().save_png(clip_out / "no-wm-socket.png")
                clip_record["problems"].append(f"ferrion-wm Wayland and IPC sockets missing in {runtime_dir}")
                return clip_record

            xdg_dir = runtime_dir
            clip_record["wayland_display"] = wl_display
            clip_record["ferrion_wm_socket"] = wm_sock
            settle(vm, 10, still=1.0).save_png(clip_out / "session.png")

            script_cmd = (
                "cat > /tmp/media/run-mpv.sh <<\\EOF\n"
                "#!/bin/sh\n"
                "rc=0\n"
                "mpv --no-config --terminal=yes --msg-level=statusline=status,all=v "
                "--vo=wlshm --ao=ferrion --hwdec=no "
                "--term-status-msg=\"MPVSTAT av=\\${avsync} drop=\\${frame-drop-count} "
                "decdrop=\\${decoder-frame-drop-count} delay=\\${vo-delayed-frame-count} "
                "pos=\\${time-pos}\" "
                f"/tmp/media/{clip_name} > /tmp/media/mpv.log 2>&1 || rc=$?\n"
                "printf \"%d\\n\" \"$rc\" > /tmp/media/mpv.rc\n"
                "EOF\n"
                "chmod 755 /tmp/media/run-mpv.sh && rm -f /tmp/media/mpv.rc /tmp/media/mpv.log"
            )
            rc_wr, wr_out = run_user(script_cmd, timeout=30)
            if rc_wr != 0:
                clip_record["problems"].append(f"failed to write mpv wrapper script: {wr_out.strip()[-200:]}")
                return clip_record

            clip_record["host_loadavg_before"] = read_host_loadavg()
            rc_spawn, spawn_out = run_user(
                f"XDG_RUNTIME_DIR={xdg_dir} WAYLAND_DISPLAY={wl_display} FERRION_WM_SOCKET={wm_sock} "
                "ferrion-wm msg action spawn -- /bin/sh /tmp/media/run-mpv.sh",
                timeout=30,
            )
            if rc_spawn != 0:
                clip_record["problems"].append(f"ferrion-wm spawn failed (rc={rc_spawn}): {spawn_out.strip()[-200:]}")
                return clip_record

            mpv_win = None
            for _ in range(WINDOW_POLL_ITERS):
                rc_w, w_out = run_user(
                    f"FERRION_WM_SOCKET={wm_sock} ferrion-wm msg --json windows",
                    timeout=15,
                )
                if rc_w == 0:
                    wins = parse_json_array(w_out)
                    for win in wins:
                        app_id = str(win.get("app_id") or "").lower()
                        title = str(win.get("title") or "").lower()
                        if "mpv" in app_id or clip_name.lower() in title:
                            mpv_win = win
                            break
                if mpv_win is not None:
                    break
                rc_done, _ = run_user("test -f /tmp/media/mpv.rc", timeout=10)
                if rc_done == 0:
                    break
                time.sleep(0.1)

            shots = []
            rect = None
            if mpv_win is not None:
                layout = mpv_win.get("layout") if isinstance(mpv_win, dict) else {}
                clip_record["window"] = {
                    "app_id": mpv_win.get("app_id"),
                    "title": mpv_win.get("title"),
                    "window_size": layout.get("window_size") if isinstance(layout, dict) else None,
                    "tile_pos_in_workspace_view": (
                        layout.get("tile_pos_in_workspace_view") if isinstance(layout, dict) else None
                    ),
                }
                for idx in range(SCREENDUMP_MAX_SHOTS):
                    if idx > 0:
                        time.sleep(SCREENDUMP_INTERVAL_S)
                    frame = vm.screenshot()
                    frame.save_png(clip_out / f"shot_{idx}.png")
                    if rect is None:
                        rect = window_rect(mpv_win, frame.width, frame.height)
                        clip_record["window"]["rect"] = list(rect)
                    non_bg = rect_non_bg_ratio(frame, rect)
                    counter = decode_frame_counter(frame, rect)
                    shots.append(
                        {
                            "index": idx,
                            "frame": frame,
                            "non_bg_ratio": non_bg,
                            "counter": counter,
                        }
                    )
                    rc_done, _ = run_user("test -f /tmp/media/mpv.rc", timeout=10)
                    if rc_done == 0:
                        break
            else:
                vm.screenshot().save_png(clip_out / "no-mpv-window.png")
                clip_record["problems"].append("mpv window never appeared in ferrion-wm msg --json windows")

            mpv_rc = None
            for _ in range(EXIT_POLL_ITERS):
                rc_check, rc_out = run_user("cat /tmp/media/mpv.rc 2>/dev/null", timeout=15)
                if rc_check == 0:
                    m_rc = re.search(r"(?m)^\s*(\d+)\s*$", rc_out)
                    if m_rc:
                        mpv_rc = int(m_rc.group(1))
                        break
                time.sleep(0.25)

            clip_record["host_loadavg_after"] = read_host_loadavg()
            run_user(
                f"test -f /tmp/media/mpv.log && {curl_post} @/tmp/media/mpv.log {base}/mpv.log || true",
                timeout=45,
            )
            time.sleep(0.5)

            log_path = guest_dir / "mpv.log"
            log_text = log_path.read_text(encoding="utf-8", errors="replace") if log_path.is_file() else ""
            parsed = parse_mpv_window_log(log_text)
            parsed["exit_code"] = mpv_rc
            if clip_record.get("window") and clip_record["window"].get("window_size"):
                win_w, win_h = clip_record["window"]["window_size"]
                parsed["window_smaller_than_source"] = (win_w < width or win_h < height)
            clip_record["mpv"] = parsed

            shot_records = []
            for s in shots:
                shot_records.append(
                    {
                        "index": s["index"],
                        "non_bg_ratio": s["non_bg_ratio"],
                        "counter": s["counter"],
                    }
                )
            clip_record["shots"] = shot_records

            valid_shots = [s for s in shots if s["counter"] is not None]
            if rect is not None and len(shots) >= 2:
                pair_diffs = []
                for i in range(len(shots) - 1):
                    pair_diffs.append(rect_mean_abs_diff(shots[i]["frame"], shots[i + 1]["frame"], rect))
                clip_record["shot_diffs"] = pair_diffs
                max_diff = max(pair_diffs)
            else:
                clip_record["shot_diffs"] = []
                max_diff = 0.0

            max_non_bg = max((s["non_bg_ratio"] for s in shots), default=0.0)
            counters = [s["counter"] for s in valid_shots]
            counter_advanced = any(counters[i + 1] > counters[i] for i in range(len(counters) - 1))
            clip_record["picture"] = {
                "max_non_bg_ratio": max_non_bg,
                "max_shot_diff": max_diff,
                "decoded_counters": counters,
                "counter_advanced": counter_advanced,
            }

            if mpv_rc != 0:
                clip_record["problems"].append(f"mpv exited {mpv_rc}: {log_text.strip()[-300:]}")
            if not parsed["vo_wlshm"]:
                clip_record["problems"].append("mpv log missing VO: [wlshm]")
            if not parsed["ao_ferrion"]:
                clip_record["problems"].append("mpv log missing AO: [ferrion]")
            if not parsed["eof"]:
                clip_record["problems"].append("mpv did not reach End of file")
            if parsed["avsync"] is None:
                clip_record["problems"].append("mpv log missing parsed MPVSTAT telemetry lines")
            if max_non_bg < MIN_NON_BG_RATIO:
                clip_record["problems"].append(
                    f"mpv window stayed near background (max_non_bg_ratio={max_non_bg} < {MIN_NON_BG_RATIO})"
                )
            if max_diff < MIN_FRAME_DIFF:
                clip_record["problems"].append(
                    f"mpv window did not change between screendumps (max_diff={max_diff} < {MIN_FRAME_DIFF})"
                )
            if not counter_advanced:
                clip_record["problems"].append(
                    f"burned-in frame counter did not advance across screendumps (decoded={counters})"
                )
        finally:
            vm.close()
    finally:
        server.shutdown()
        server.server_close()

    wav_report = (
        media_decode.analyze_mpv_wav(wav_path.read_bytes(), expected_duration_s=CLIP_DURATION_S)
        if wav_path.is_file()
        else {"ok": False, "reason": "audio.wav missing"}
    )
    clip_record["wav"] = wav_report
    if not wav_report.get("ok"):
        clip_record["problems"].append(f"WAV verification failed: {wav_report}")
    return clip_record


def run_mpv_window(args, out, helpers):
    Blocked = helpers["Blocked"]
    if not shutil.which("ffmpeg"):
        raise Blocked("host lacks ffmpeg")
    encoder = detect_av1_encoder()
    if encoder is None:
        raise Blocked("host ffmpeg lacks libsvtav1 and libaom-av1")

    out.mkdir(parents=True, exist_ok=True)
    clips_dir = out / "clips"
    clips_dir.mkdir(parents=True, exist_ok=True)

    record = {
        "encoder": encoder,
        "clip_duration_s": CLIP_DURATION_S,
        "clip_fps": CLIP_FPS,
        "clips": {},
    }

    def save_record():
        (out / "mpv-window.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")

    problems = []
    summaries = []
    for label, width, height, clip_name in CLIP_SPECS:
        clip_path = clips_dir / clip_name
        clip_sha = generate_clip(clip_path, width, height, encoder)
        clip_record = run_single_clip(
            args,
            out / label,
            label,
            width,
            height,
            clip_name,
            clip_sha,
            clips_dir,
            helpers,
        )
        record["clips"][label] = clip_record
        save_record()
        if clip_record["problems"]:
            problems.extend(f"{label}: {p}" for p in clip_record["problems"])
            if clip_record.get("red_missing_wlshm"):
                break
        else:
            mpv_info = clip_record["mpv"]
            win_size = (clip_record.get("window") or {}).get("window_size")
            load1 = (clip_record.get("host_loadavg_after") or {}).get("load1")
            av_max = (mpv_info.get("avsync") or {}).get("max_abs_s")
            summaries.append(
                f"{label} win={win_size} counters={clip_record['picture']['decoded_counters']} "
                f"drop={mpv_info['frame_drop_count']}/{mpv_info['decoder_frame_drop_count']} "
                f"av_max={av_max}s wav={clip_record['wav']['duration_s']:.3f}s load1={load1}"
            )

    record["problems"] = problems
    save_record()
    if problems:
        return False, "; ".join(problems) + f"; see {out}"
    return True, " | ".join(summaries) + f"; see {out}"
