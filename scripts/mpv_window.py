"""M2d-a check: mpv Wayland wlshm, gpu-next (EGL/GL), and gpu-next-vk (Vulkan/waylandvk) window + ferrion audio on wm-session."""
import json
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
ALL_VO_NAMES = ("wlshm", "gpu-next", "gpu-next-vk")
DEFAULT_VO_NAMES = ("wlshm", "gpu-next")
DEFAULT_GPU_MODE = "cpu"
GPU_MODE_EXPECTATIONS = {
    "cpu": {
        "gpu-next": {
            "allow_sw": True,
            "gl_renderer_substr": "llvmpipe",
            "gl_renderer_forbid_substr": "zink",
        },
        "gpu-next-vk": {
            "allow_sw": True,
            "vk_driver_id": "VK_DRIVER_ID_MESA_LLVMPIPE",
        },
    },
}
COUNTER_BITS = 8
MIN_MARKER_SAMPLES = 8
WAYLAND_SOCKET_WAIT_ITERS = 30
WINDOW_POLL_ITERS = 80
FULLSCREEN_SETTLE_ITERS = 25
EXIT_POLL_ITERS = 160
SCREENDUMP_MAX_SHOTS = 4
SCREENDUMP_INTERVAL_S = 0.65
BG_RGB = (15, 14, 12)
BG_TOLERANCE = 10
MIN_NON_BG_RATIO = 0.25
MIN_FRAME_DIFF = 1.5
MAX_LOG_LINES = 20000
MAX_JSON_SCAN_MATCHES = 128
WAV_CONTINUITY_EXEMPT_MIN_WIDTH = 3840
WAV_CONTINUITY_EXEMPT_GL_RENDERER = "llvmpipe"
WAV_CONTINUITY_EXEMPT_VK_DRIVER_ID = "VK_DRIVER_ID_MESA_LLVMPIPE"


def parse_vo_selection(raw_vo):
    if not raw_vo:
        return list(DEFAULT_VO_NAMES), None
    selected = []
    for part in str(raw_vo).split(","):
        token = part.strip()
        if not token:
            continue
        if token not in ALL_VO_NAMES:
            return None, f"unknown --mpv-vo value {token!r} (expected subset of {', '.join(ALL_VO_NAMES)})"
        if token not in selected:
            selected.append(token)
    if not selected:
        return None, f"empty --mpv-vo selection (expected subset of {', '.join(ALL_VO_NAMES)})"
    return selected, None


def resolve_vo_spec(vo_name, gpu_mode):
    if vo_name == "wlshm":
        return "--vo=wlshm", None, None
    mode_table = GPU_MODE_EXPECTATIONS.get(gpu_mode)
    if mode_table is None:
        return None, None, f"unknown --mpv-gpu mode {gpu_mode!r} (no expectation table entry)"
    expect = mode_table.get(vo_name)
    if expect is None:
        return None, None, f"no expected renderer entry for --mpv-gpu={gpu_mode!r} and vo={vo_name!r}"
    sw_flag = " --gpu-sw" if expect.get("allow_sw") else ""
    if vo_name == "gpu-next":
        return f"--vo=gpu-next --gpu-api=opengl --gpu-context=wayland{sw_flag}", expect, None
    if vo_name == "gpu-next-vk":
        return f"--vo=gpu-next --gpu-api=vulkan --gpu-context=waylandvk{sw_flag}", expect, None
    return None, None, f"unsupported vo {vo_name!r}"


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
    decoder = json.JSONDecoder()
    for idx, match in enumerate(re.finditer(r"\[", text)):
        if idx >= MAX_JSON_SCAN_MATCHES:
            break
        try:
            value, _ = decoder.raw_decode(text[match.start():])
            if isinstance(value, list):
                return value
        except json.JSONDecodeError:
            continue
    return []


def parse_json_object(text):
    decoder = json.JSONDecoder()
    for idx, match in enumerate(re.finditer(r"\{", text)):
        if idx >= MAX_JSON_SCAN_MATCHES:
            break
        try:
            value, _ = decoder.raw_decode(text[match.start():])
            if isinstance(value, dict):
                return value
        except json.JSONDecodeError:
            continue
    return {}


def extract_primary_output(outputs_obj):
    if not isinstance(outputs_obj, dict):
        return None
    for name in sorted(outputs_obj.keys()):
        entry = outputs_obj.get(name)
        if not isinstance(entry, dict):
            continue
        idx = entry.get("current_mode")
        modes = entry.get("modes") or []
        if isinstance(idx, int) and 0 <= idx < len(modes):
            mode = modes[idx]
            if isinstance(mode, dict) and "width" in mode and "height" in mode:
                return {
                    "name": name,
                    "mode": [int(mode["width"]), int(mode["height"])],
                    "refresh_rate": mode.get("refresh_rate"),
                    "logical": entry.get("logical"),
                }
    return None


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
        if isinstance(size, list) and len(size) == 2:
            w = max(1, min(frame_w, int(size[0])))
            h = max(1, min(frame_h, int(size[1])))
            return 0, 0, w, h
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
    vo_driver = None
    vo_drivers = []
    ao_line = None
    ao_driver = None
    vo_display_line = None
    vo_scale_line = None
    vo_scaled = None
    vo_src = None
    vo_dst = None
    sws_line = None
    gpu_context_line = None
    gpu_context = None
    egl_version_line = None
    egl_vendor_line = None
    gl_renderer = None
    gl_version = None
    pl_gl_renderer = None
    pl_gl_version = None
    vk_wayland_surface_ext = False
    vk_device_name = None
    vk_api_version = None
    vk_driver_id = None
    vk_driver_name = None
    vk_driver_info = None
    spirv_compiler = None
    vk_surface_config = None
    wl_has_fifo = False
    wl_present_v2 = False
    vk_present_mode_fallback = False
    avsync_vals = []
    frame_drop_max = None
    dec_drop_max = None
    delayed_max = None
    clamped = None
    dropped_status = None
    eof = False
    audio_underrun = False
    av_desync = False

    for line in lines:
        stripped = line.strip()
        m_vo = re.match(r"^VO:\s+\[([^\]]+)\]", stripped)
        if m_vo:
            drv = m_vo.group(1)
            vo_drivers.append(drv)
            if vo_line is None:
                vo_line = stripped
                vo_driver = drv
        m_ao = re.match(r"^AO:\s+\[([^\]]+)\]", stripped)
        if m_ao and ao_line is None:
            ao_line = stripped
            ao_driver = m_ao.group(1)
        if "Video display:" in line and "[vo/" in line:
            vo_display_line = stripped
            m_vd = re.search(r"Video display:\s*\([^)]*\)\s*(\d+x\d+)\s*->\s*\([^)]*\)\s*(\d+x\d+)", line)
            if m_vd:
                vo_src = m_vd.group(1)
                vo_dst = m_vd.group(2)
        if "Video scale:" in line and "[vo/" in line:
            vo_scale_line = stripped
        if sws_line is None and "swscaler:" in line:
            sws_line = stripped
        m_ctx = re.search(r"\[vo/[^\]]+\]\s+Initializing GPU context '([^']+)'", line)
        if m_ctx and gpu_context_line is None:
            gpu_context_line = stripped
            gpu_context = m_ctx.group(1)
        if egl_version_line is None and "[vo/" in line and "EGL_VERSION=" in line:
            egl_version_line = stripped
        if egl_vendor_line is None and "[vo/" in line and "EGL_VENDOR=" in line:
            egl_vendor_line = stripped
        m_glr_mpv = re.search(r"\[vo/[^\]]+\]\s+GL_RENDERER='([^']+)'", line)
        if m_glr_mpv and gl_renderer is None:
            gl_renderer = m_glr_mpv.group(1).strip()
        m_glv_mpv = re.search(r"\[vo/[^\]]+\]\s+GL_VERSION='([^']+)'", line)
        if m_glv_mpv and gl_version is None:
            gl_version = m_glv_mpv.group(1).strip()
        m_glr_pl = re.search(r"\[vo/[^\]]+\]\s+GL_RENDERER:\s+(.+)$", line)
        if m_glr_pl and pl_gl_renderer is None:
            pl_gl_renderer = m_glr_pl.group(1).strip()
        m_glv_pl = re.search(r"\[vo/[^\]]+\]\s+GL_VERSION:\s+(.+)$", line)
        if m_glv_pl and pl_gl_version is None:
            pl_gl_version = m_glv_pl.group(1).strip()
        if "[vo/" in line and "VK_KHR_wayland_surface" in line:
            vk_wayland_surface_ext = True
        m_vk_dev = re.search(r"\[vo/[^\]]+\]\s+Device Name:\s+(.+)$", line)
        if m_vk_dev and vk_device_name is None:
            vk_device_name = m_vk_dev.group(1).strip()
        m_vk_api = re.search(r"\[vo/[^\]]+\]\s+API version:\s+(.+)$", line)
        if m_vk_api and vk_api_version is None:
            vk_api_version = m_vk_api.group(1).strip()
        m_vk_drv_id = re.search(r"\[vo/[^\]]+\]\s+Driver ID:\s+(\S+)", line)
        if m_vk_drv_id and vk_driver_id is None:
            vk_driver_id = m_vk_drv_id.group(1).strip()
        m_vk_drv_name = re.search(r"\[vo/[^\]]+\]\s+Driver name:\s+(.+)$", line)
        if m_vk_drv_name and vk_driver_name is None:
            vk_driver_name = m_vk_drv_name.group(1).strip()
        m_vk_drv_info = re.search(r"\[vo/[^\]]+\]\s+Driver info:\s+(.+)$", line)
        if m_vk_drv_info and vk_driver_info is None:
            vk_driver_info = m_vk_drv_info.group(1).strip()
        m_spirv = re.search(r"\[vo/[^\]]+\]\s+Initialized SPIR-V compiler '([^']+)'", line)
        if m_spirv and spirv_compiler is None:
            spirv_compiler = m_spirv.group(1).strip()
        m_surf = re.search(r"\[vo/[^\]]+\]\s+Picked surface configuration\s+\d+:\s+(.+)$", line)
        if m_surf and vk_surface_config is None:
            vk_surface_config = m_surf.group(1).strip()
        if "[vo/" in line and "Registered interface wp_fifo_manager_v1 at version" in line:
            wl_has_fifo = True
        if "[vo/" in line and "Registered interface wp_presentation at version 2" in line:
            wl_present_v2 = True
        if "[vo/" in line and "falling back to VK_PRESENT_MODE_FIFO_KHR" in line:
            vk_present_mode_fallback = True
        if stripped == "Exiting... (End of file)":
            eof = True
        if "Audio device underrun detected" in line:
            audio_underrun = True
        if "Audio/Video desynchronisation detected" in line:
            av_desync = True
        m_stat = re.match(
            r"^MPVSTAT\s+av=([-+]?\d+(?:\.\d+)?)\s+drop=(\d+)\s+decdrop=(\d+)\s+delay=(\d+)\s+pos=(\S+)",
            stripped,
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

    effective_gl_renderer = gl_renderer or pl_gl_renderer
    effective_gl_version = gl_version or pl_gl_version
    vk_present_mode = None
    if gpu_context == "waylandvk" and vk_surface_config:
        if vk_present_mode_fallback or (wl_has_fifo and wl_present_v2):
            vk_present_mode = "VK_PRESENT_MODE_FIFO_KHR"
        else:
            vk_present_mode = "VK_PRESENT_MODE_MAILBOX_KHR"

    return {
        "vo_driver": vo_driver,
        "vo_drivers": vo_drivers,
        "vo_wlshm": "wlshm" in vo_drivers,
        "vo_gpu_next": "gpu-next" in vo_drivers,
        "vo_line": vo_line,
        "vo_display_line": vo_display_line,
        "vo_scale_line": vo_scale_line,
        "vo_src": vo_src,
        "vo_dst": vo_dst,
        "vo_scaled": vo_scaled,
        "sws_line": sws_line,
        "gpu_context_line": gpu_context_line,
        "gpu_context": gpu_context,
        "egl_version_line": egl_version_line,
        "egl_vendor_line": egl_vendor_line,
        "gl_renderer": effective_gl_renderer,
        "gl_version": effective_gl_version,
        "pl_gl_renderer": pl_gl_renderer,
        "pl_gl_version": pl_gl_version,
        "vk_wayland_surface_ext": vk_wayland_surface_ext,
        "vk_device_name": vk_device_name,
        "vk_api_version": vk_api_version,
        "vk_driver_id": vk_driver_id,
        "vk_driver_name": vk_driver_name,
        "vk_driver_info": vk_driver_info,
        "spirv_compiler": spirv_compiler,
        "vk_surface_config": vk_surface_config,
        "wl_has_fifo": wl_has_fifo,
        "wl_present_v2": wl_present_v2,
        "vk_present_mode_fallback": vk_present_mode_fallback,
        "vk_present_mode": vk_present_mode,
        "ao_line": ao_line,
        "ao_driver": ao_driver,
        "ao_ferrion": ao_driver == "ferrion",
        "eof": eof,
        "audio_underrun": audio_underrun,
        "av_desync": av_desync,
        "frame_drop_count": frame_drop_max if frame_drop_max is not None else (dropped_status or 0),
        "decoder_frame_drop_count": dec_drop_max if dec_drop_max is not None else 0,
        "vo_delayed_frame_count": delayed_max if delayed_max is not None else 0,
        "avsync": avsync_summary,
        "clamped_samples": clamped,
    }


def help_lists_token(text, token):
    for line in text.splitlines():
        parts = line.strip().split()
        if parts and parts[0] == token:
            return True
    return False


def run_single_clip(
    args,
    clip_out,
    label,
    width,
    height,
    clip_name,
    clip_sha,
    vo_name,
    vo_flags,
    gpu_mode,
    expected_renderer,
    fullscreen,
    clips_dir,
    helpers,
):
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
        "vo": vo_name,
        "vo_flags": vo_flags,
        "gpu_mode": gpu_mode,
        "expected_renderer": expected_renderer,
        "fullscreen": bool(fullscreen),
        "launch_path": "ferrion-wm msg action spawn (unsandboxed mpv in ferrion-wm session)",
        "red_missing_vo": False,
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
                "mkdir -p /tmp/media && "
                "mpv --vo=help > /tmp/media/vo-help.txt 2>&1 && "
                "mpv --gpu-api=help > /tmp/media/gpu-api-help.txt 2>&1 && "
                "mpv --gpu-context=help > /tmp/media/gpu-context-help.txt 2>&1 && "
                f"{curl_post} @/tmp/media/vo-help.txt {base}/vo-help.txt && "
                f"{curl_post} @/tmp/media/gpu-api-help.txt {base}/gpu-api-help.txt && "
                f"{curl_post} @/tmp/media/gpu-context-help.txt {base}/gpu-context-help.txt",
                timeout=45,
            )
            if rc_help != 0:
                clip_record["problems"].append(f"mpv help probes failed (rc={rc_help}): {help_out.strip()[-200:]}")
                return clip_record
            vo_help = (guest_dir / "vo-help.txt").read_text(encoding="utf-8", errors="replace")
            gpu_api_help = (guest_dir / "gpu-api-help.txt").read_text(encoding="utf-8", errors="replace")
            gpu_ctx_help = (guest_dir / "gpu-context-help.txt").read_text(encoding="utf-8", errors="replace")
            has_wlshm = help_lists_token(vo_help, "wlshm")
            has_gpu_next = help_lists_token(vo_help, "gpu-next")
            has_opengl = help_lists_token(gpu_api_help, "opengl")
            has_vulkan = help_lists_token(gpu_api_help, "vulkan")
            has_wayland_ctx = help_lists_token(gpu_ctx_help, "wayland")
            has_waylandvk_ctx = help_lists_token(gpu_ctx_help, "waylandvk")
            clip_record["vo_help_lists_wlshm"] = has_wlshm
            clip_record["vo_help_lists_gpu_next"] = has_gpu_next
            clip_record["gpu_api_help_lists_opengl"] = has_opengl
            clip_record["gpu_api_help_lists_vulkan"] = has_vulkan
            clip_record["gpu_context_help_lists_wayland"] = has_wayland_ctx
            clip_record["gpu_context_help_lists_waylandvk"] = has_waylandvk_ctx

            rc_dl, dl_out = run_user(
                f"{curl_get} {base}/{clip_name} -o /tmp/media/{clip_name} && "
                f"got=$(sha256sum /tmp/media/{clip_name} | cut -d\" \" -f1) && "
                f"test \"$got\" = \"{clip_sha}\"",
                timeout=120,
            )
            if rc_dl != 0:
                clip_record["problems"].append(f"download or sha256 failed for {clip_name}: {dl_out.strip()[-200:]}")
                return clip_record

            missing_requested = (
                (vo_name == "wlshm" and not has_wlshm)
                or (vo_name == "gpu-next" and not (has_gpu_next and has_opengl and has_wayland_ctx))
                or (vo_name == "gpu-next-vk" and not (has_gpu_next and has_vulkan and has_waylandvk_ctx))
            )
            if missing_requested:
                rc_red, _ = run_user(
                    f"rc=0; mpv --no-config {vo_flags} --ao=ferrion --hwdec=no /tmp/media/{clip_name} "
                    "> /tmp/media/red-vo.log 2>&1 || rc=$?; "
                    "printf \"%d\\n\" \"$rc\" > /tmp/media/red-vo.rc; "
                    f"{curl_post} @/tmp/media/red-vo.log {base}/red-vo.log || true; "
                    f"{curl_post} @/tmp/media/red-vo.rc {base}/red-vo.rc || true",
                    timeout=60,
                )
                red_rc_path = guest_dir / "red-vo.rc"
                red_exit = (
                    int(red_rc_path.read_text(encoding="utf-8", errors="replace").strip())
                    if red_rc_path.is_file() and red_rc_path.read_text(encoding="utf-8", errors="replace").strip().isdigit()
                    else rc_red
                )
                red_log = ""
                red_path = guest_dir / "red-vo.log"
                if red_path.is_file():
                    red_log = red_path.read_text(encoding="utf-8", errors="replace").strip()
                red_head = "\n".join(red_log.splitlines()[:12])
                clip_record["red_missing_vo"] = True
                clip_record["red_vo_exit_code"] = red_exit
                clip_record["red_vo_log_head"] = red_head
                clip_record["problems"].append(
                    f"mpv lacks requested {vo_name} support (vo={has_wlshm if vo_name == 'wlshm' else has_gpu_next}, "
                    f"opengl={has_opengl}, vulkan={has_vulkan}, wayland_ctx={has_wayland_ctx}, "
                    f"waylandvk_ctx={has_waylandvk_ctx}); rc={red_exit}: {red_head}"
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

            rc_out, out_json = run_user(
                f"FERRION_WM_SOCKET={wm_sock} ferrion-wm msg --json outputs",
                timeout=15,
            )
            outputs_obj = parse_json_object(out_json) if rc_out == 0 else {}
            primary_output = extract_primary_output(outputs_obj)
            clip_record["output"] = primary_output
            clip_record["output_mode"] = primary_output["mode"] if primary_output else None
            if primary_output is None:
                clip_record["problems"].append(
                    f"ferrion-wm msg --json outputs returned no output mode (rc={rc_out}): {out_json.strip()[-200:]}"
                )
                return clip_record

            fs_flag = "--fs " if fullscreen else ""
            script_cmd = (
                "cat > /tmp/media/run-mpv.sh <<\\EOF\n"
                "#!/bin/sh\n"
                "rc=0\n"
                "mpv --no-config --terminal=yes --msg-level=statusline=status,all=v "
                f"{vo_flags} {fs_flag}--ao=ferrion --hwdec=no "
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

            def find_mpv_window():
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
                            return win
                return None

            mpv_win = None
            for _ in range(WINDOW_POLL_ITERS):
                mpv_win = find_mpv_window()
                if mpv_win is not None:
                    break
                rc_done, _ = run_user("test -f /tmp/media/mpv.rc", timeout=10)
                if rc_done == 0:
                    break
                time.sleep(0.1)

            if mpv_win is not None and fullscreen:
                expected_mode = primary_output["mode"]
                for _ in range(FULLSCREEN_SETTLE_ITERS):
                    layout = mpv_win.get("layout") if isinstance(mpv_win, dict) else {}
                    win_size = layout.get("window_size") if isinstance(layout, dict) else None
                    if win_size == expected_mode:
                        break
                    rc_done, _ = run_user("test -f /tmp/media/mpv.rc", timeout=10)
                    if rc_done == 0:
                        break
                    time.sleep(0.1)
                    updated = find_mpv_window()
                    if updated is not None:
                        mpv_win = updated

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
            else:
                vm.screenshot().save_png(clip_out / "no-mpv-window.png")
                clip_record["problems"].append("mpv window never appeared in ferrion-wm msg --json windows")

            mpv_rc = None
            run_user(
                f"i=0; while [ ! -f /tmp/media/mpv.rc ] && [ \"$i\" -lt {EXIT_POLL_ITERS} ]; do "
                "sleep 0.25; i=$((i + 1)); done; [ -f /tmp/media/mpv.rc ]",
                timeout=max(60, int(EXIT_POLL_ITERS * 0.25) + 30),
            )

            clip_record["host_loadavg_after"] = read_host_loadavg()
            run_user(
                f"test -f /tmp/media/mpv.rc && {curl_post} @/tmp/media/mpv.rc {base}/mpv.rc || true; "
                f"test -f /tmp/media/mpv.log && {curl_post} @/tmp/media/mpv.log {base}/mpv.log || true",
                timeout=45,
            )
            time.sleep(0.5)

            rc_path = guest_dir / "mpv.rc"
            if rc_path.is_file():
                rc_text = rc_path.read_text(encoding="utf-8", errors="replace").strip()
                if rc_text.isdigit():
                    mpv_rc = int(rc_text)

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
            if not log_path.is_file() or not log_text:
                clip_record["problems"].append("mpv.log was not uploaded from guest")
            if vo_name == "wlshm":
                if not parsed["vo_wlshm"]:
                    clip_record["problems"].append("mpv.log missing VO: [wlshm]")
                if parsed["vo_gpu_next"]:
                    clip_record["problems"].append("mpv.log unexpectedly reported VO: [gpu-next] on wlshm run")
            elif vo_name == "gpu-next":
                if not parsed["vo_gpu_next"]:
                    clip_record["problems"].append("mpv.log missing VO: [gpu-next]")
                if parsed["vo_wlshm"]:
                    clip_record["problems"].append("mpv.log fell back to VO: [wlshm] on gpu-next run")
                if parsed["gpu_context"] != "wayland":
                    clip_record["problems"].append(
                        f"mpv.log missing Wayland GPU context init (got {parsed['gpu_context']!r})"
                    )
                if not parsed["egl_version_line"]:
                    clip_record["problems"].append("mpv.log missing EGL_VERSION context line")
                renderer = parsed["gl_renderer"]
                if not renderer:
                    clip_record["problems"].append("mpv.log missing GL_RENDERER")
                elif not expected_renderer:
                    clip_record["problems"].append(
                        f"no expected renderer configured for gpu_mode={gpu_mode!r} vo={vo_name!r}"
                    )
                else:
                    want_sub = (expected_renderer.get("gl_renderer_substr") or "").lower()
                    forbid_sub = (expected_renderer.get("gl_renderer_forbid_substr") or "").lower()
                    if not want_sub:
                        clip_record["problems"].append(
                            f"empty gl_renderer_substr expectation for gpu_mode={gpu_mode!r}"
                        )
                    elif want_sub not in renderer.lower() or (forbid_sub and forbid_sub in renderer.lower()):
                        clip_record["problems"].append(
                            f"unexpected GL_RENDERER={renderer!r} for gpu_mode={gpu_mode!r} "
                            f"(expected {want_sub!r}, forbid {forbid_sub!r})"
                        )
                if not parsed["gl_version"]:
                    clip_record["problems"].append("mpv.log missing GL_VERSION")
            elif vo_name == "gpu-next-vk":
                if not parsed["vo_gpu_next"]:
                    clip_record["problems"].append("mpv.log missing VO: [gpu-next]")
                if parsed["vo_wlshm"]:
                    clip_record["problems"].append("mpv.log fell back to VO: [wlshm] on gpu-next-vk run")
                if parsed["gpu_context"] != "waylandvk":
                    clip_record["problems"].append(
                        f"mpv.log missing Wayland Vulkan context init 'waylandvk' (got {parsed['gpu_context']!r})"
                    )
                if not parsed["vk_wayland_surface_ext"]:
                    clip_record["problems"].append("mpv.log missing VK_KHR_wayland_surface extension")
                if not parsed["vk_surface_config"]:
                    clip_record["problems"].append("mpv.log missing Vulkan swapchain surface configuration")
                if not parsed["vk_present_mode"]:
                    clip_record["problems"].append("mpv.log missing Vulkan swapchain present mode")
                if not parsed["vk_device_name"]:
                    clip_record["problems"].append("mpv.log missing Vulkan Device Name")
                if not parsed["vk_api_version"]:
                    clip_record["problems"].append("mpv.log missing Vulkan API version")
                if not parsed["spirv_compiler"]:
                    clip_record["problems"].append("mpv.log missing Initialized SPIR-V compiler line")
                driver_id = parsed["vk_driver_id"]
                if not driver_id:
                    clip_record["problems"].append("mpv.log missing Vulkan Driver ID")
                elif not expected_renderer:
                    clip_record["problems"].append(
                        f"no expected renderer configured for gpu_mode={gpu_mode!r} vo={vo_name!r}"
                    )
                else:
                    want_drv = expected_renderer.get("vk_driver_id") or ""
                    if not want_drv:
                        clip_record["problems"].append(
                            f"empty vk_driver_id expectation for gpu_mode={gpu_mode!r}"
                        )
                    elif driver_id != want_drv:
                        clip_record["problems"].append(
                            f"unexpected Vulkan Driver ID={driver_id!r} for gpu_mode={gpu_mode!r} "
                            f"(expected {want_drv!r})"
                        )
            else:
                clip_record["problems"].append(f"unhandled vo {vo_name!r}")
            if fullscreen:
                win_size = (clip_record.get("window") or {}).get("window_size")
                if win_size != primary_output["mode"]:
                    clip_record["problems"].append(
                        f"fullscreen window_size {win_size} != output mode {primary_output['mode']}"
                    )
            if not parsed["ao_ferrion"]:
                clip_record["problems"].append("mpv.log missing AO: [ferrion]")
            if not parsed["eof"]:
                clip_record["problems"].append("mpv did not reach End of file")
            if parsed["avsync"] is None:
                clip_record["problems"].append("mpv.log missing parsed MPVSTAT telemetry lines")
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
    if isinstance(wav_report, dict):
        wav_report["gap_count"] = len(wav_report.get("gap_blocks") or [])
        wav_report["phase_jump_count"] = len(wav_report.get("phase_jumps") or [])
    clip_record["wav"] = wav_report
    mpv_parsed = clip_record.get("mpv") or {}
    gl_renderer = (mpv_parsed.get("gl_renderer") or "").lower()
    vk_driver_id = mpv_parsed.get("vk_driver_id") or ""
    is_sw_gl = (
        WAV_CONTINUITY_EXEMPT_GL_RENDERER in gl_renderer
        and "zink" not in gl_renderer
    )
    is_sw_vk = vk_driver_id == WAV_CONTINUITY_EXEMPT_VK_DRIVER_ID
    exempt_wav_continuity = (
        width >= WAV_CONTINUITY_EXEMPT_MIN_WIDTH
        and (is_sw_gl or is_sw_vk)
    )
    clip_record["wav_continuity_asserted"] = not exempt_wav_continuity
    if exempt_wav_continuity:
        if not wav_report.get("freq_ok"):
            clip_record["problems"].append(f"WAV frequency/gain check failed: {wav_report}")
    elif not wav_report.get("ok"):
        clip_record["problems"].append(f"WAV verification failed: {wav_report}")
    return clip_record


def run_mpv_window(args, out, helpers):
    Blocked = helpers["Blocked"]
    if not shutil.which("ffmpeg"):
        raise Blocked("host lacks ffmpeg")
    encoder = detect_av1_encoder()
    if encoder is None:
        raise Blocked("host ffmpeg lacks libsvtav1 and libaom-av1")

    fullscreen = bool(getattr(args, "mpv_fullscreen", False))
    gpu_mode = (getattr(args, "mpv_gpu", None) or DEFAULT_GPU_MODE).strip()
    vo_names, vo_err = parse_vo_selection(getattr(args, "mpv_vo", ""))
    if vo_err:
        return False, vo_err

    resolved_vos = []
    for vo_name in vo_names:
        vo_flags, expected_renderer, err = resolve_vo_spec(vo_name, gpu_mode)
        if err:
            return False, err
        resolved_vos.append((vo_name, vo_flags, expected_renderer))

    out.mkdir(parents=True, exist_ok=True)
    clips_dir = out / "clips"
    clips_dir.mkdir(parents=True, exist_ok=True)

    record = {
        "encoder": encoder,
        "clip_duration_s": CLIP_DURATION_S,
        "clip_fps": CLIP_FPS,
        "fullscreen": fullscreen,
        "gpu_mode": gpu_mode,
        "vos": list(vo_names),
        "clips": {},
    }

    def save_record():
        (out / "mpv-window.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")

    problems = []
    summaries = []
    clip_shas = {}
    for clip_label, width, height, clip_name in CLIP_SPECS:
        clip_path = clips_dir / clip_name
        clip_shas[clip_label] = generate_clip(clip_path, width, height, encoder)

    stop_early = False
    for clip_label, width, height, clip_name in CLIP_SPECS:
        if stop_early:
            break
        for vo_name, vo_flags, expected_renderer in resolved_vos:
            case_label = f"{clip_label}-{vo_name}-fs" if fullscreen else f"{clip_label}-{vo_name}"
            clip_record = run_single_clip(
                args,
                out / case_label,
                case_label,
                width,
                height,
                clip_name,
                clip_shas[clip_label],
                vo_name,
                vo_flags,
                gpu_mode,
                expected_renderer,
                fullscreen,
                clips_dir,
                helpers,
            )
            record["clips"][case_label] = clip_record
            save_record()
            if clip_record["problems"]:
                problems.extend(f"{case_label}: {p}" for p in clip_record["problems"])
                if clip_record.get("red_missing_vo"):
                    stop_early = True
                    break
            else:
                mpv_info = clip_record["mpv"]
                win_size = (clip_record.get("window") or {}).get("window_size")
                load1 = (clip_record.get("host_loadavg_after") or {}).get("load1")
                av_max = (mpv_info.get("avsync") or {}).get("max_abs_s")
                if vo_name == "gpu-next":
                    renderer_note = f" gl={mpv_info['gl_renderer']!r}"
                elif vo_name == "gpu-next-vk":
                    renderer_note = (
                        f" vk={mpv_info['vk_driver_id']}"
                        f" dev={mpv_info['vk_device_name']!r}"
                        f" present={mpv_info['vk_present_mode']}"
                    )
                else:
                    renderer_note = ""
                summaries.append(
                    f"{case_label} win={win_size} mode={clip_record.get('output_mode')} "
                    f"counters={clip_record['picture']['decoded_counters']} "
                    f"drop={mpv_info['frame_drop_count']}/{mpv_info['decoder_frame_drop_count']}/"
                    f"{mpv_info['vo_delayed_frame_count']} "
                    f"av_max={av_max}s{renderer_note} wav={clip_record['wav']['duration_s']:.3f}s load1={load1}"
                )

    record["problems"] = problems
    save_record()
    if problems:
        return False, "; ".join(problems) + f"; see {out}"
    return True, " | ".join(summaries) + f"; see {out}"
