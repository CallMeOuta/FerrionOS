"""M3b check: glycin sandboxed image loaders, pipe frame handoff, and isolation checks."""
import http.server
import json
import re
import shutil
import subprocess
import tempfile
import threading
import time
from pathlib import Path

MAX_HTTP_REQUESTS = 128
REQUIRED_MIMES = (
    "image/jpeg",
    "image/png",
    "image/webp",
    "image/tiff",
    "image/avif",
    "image/heif",
    "image/jxl",
    "image/svg+xml",
)
FORBIDDEN_MIMES = (
    "image/jp2",
    "image/x-jp2-codestream",
)


def make_quadrant_ppm(path, width=64, height=64):
    pixels = bytearray(width * height * 3)
    for y in range(height):
        for x in range(width):
            if y < height // 2 and x < width // 2:
                rgb = (220, 40, 40)
            elif y < height // 2:
                rgb = (40, 200, 40)
            elif x < width // 2:
                rgb = (40, 60, 220)
            else:
                rgb = (230, 220, 40)
            off = (y * width + x) * 3
            pixels[off : off + 3] = rgb
    header = f"P6\n{width} {height}\n255\n".encode("ascii")
    path.write_bytes(header + bytes(pixels))


def run_host(cmd):
    res = subprocess.run(cmd, capture_output=True, text=True)
    if res.returncode != 0:
        raise RuntimeError(f"host command failed ({res.returncode}): {cmd}\n{res.stderr}")
    return res


def generate_fixtures(tmp_dir):
    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        raise RuntimeError("host lacks ffmpeg for M3b fixture generation")
    avifenc = shutil.which("avifenc")
    cjxl = shutil.which("cjxl")
    heif_enc = shutil.which("heif-enc")
    opj_compress = shutil.which("opj_compress")

    base_ppm = tmp_dir / "base.ppm"
    make_quadrant_ppm(base_ppm, 64, 64)

    fixtures = {}

    png_path = tmp_dir / "sample.png"
    run_host([ffmpeg, "-y", "-f", "image2", "-i", str(base_ppm), "-c:v", "png", str(png_path)])
    fixtures["sample.png"] = png_path.read_bytes()

    jpg_path = tmp_dir / "sample.jpg"
    run_host([ffmpeg, "-y", "-f", "image2", "-i", str(base_ppm), "-c:v", "mjpeg", "-q:v", "2", str(jpg_path)])
    fixtures["sample.jpg"] = jpg_path.read_bytes()

    webp_path = tmp_dir / "sample.webp"
    run_host([ffmpeg, "-y", "-f", "image2", "-i", str(base_ppm), "-c:v", "libwebp", "-lossless", "1", str(webp_path)])
    fixtures["sample.webp"] = webp_path.read_bytes()

    tiff_path = tmp_dir / "sample.tiff"
    run_host([ffmpeg, "-y", "-f", "image2", "-i", str(base_ppm), "-c:v", "tiff", str(tiff_path)])
    fixtures["sample.tiff"] = tiff_path.read_bytes()

    avif_path = tmp_dir / "sample.avif"
    if avifenc:
        run_host([avifenc, "--lossless", str(png_path), str(avif_path)])
    elif heif_enc:
        run_host([heif_enc, "--avif", "-L", str(png_path), "-o", str(avif_path)])
    else:
        raise RuntimeError("host lacks avifenc and heif-enc")
    fixtures["sample.avif"] = avif_path.read_bytes()
    fixtures["sample_av1.heif"] = fixtures["sample.avif"]

    jxl_path = tmp_dir / "sample.jxl"
    if cjxl:
        run_host([cjxl, "-d", "0", str(png_path), str(jxl_path)])
    else:
        run_host([ffmpeg, "-y", "-f", "image2", "-i", str(base_ppm), "-c:v", "libjxl", str(jxl_path)])
    fixtures["sample.jxl"] = jxl_path.read_bytes()

    svg_text = (
        '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">\n'
        '  <rect x="0" y="0" width="32" height="32" fill="#dc2828"/>\n'
        '  <rect x="32" y="0" width="32" height="32" fill="#28c828"/>\n'
        '  <rect x="0" y="32" width="32" height="32" fill="#283cdc"/>\n'
        '  <rect x="32" y="32" width="32" height="32" fill="#e6dc28"/>\n'
        '  <text x="6" y="18" font-family="sans-serif" font-size="10" fill="#ffffff">OK</text>\n'
        '</svg>\n'
    )
    fixtures["sample.svg"] = svg_text.encode("utf-8")

    mp24_path = tmp_dir / "sample_24mp.png"
    run_host([
        ffmpeg, "-y", "-f", "lavfi",
        "-i", "color=c=#dc2828:s=6000x4000:d=0.04,format=rgba",
        "-frames:v", "1", "-c:v", "png", str(mp24_path),
    ])
    fixtures["sample_24mp.png"] = mp24_path.read_bytes()

    heic_path = tmp_dir / "bad_heic.heic"
    if heif_enc:
        run_host([heif_enc, str(png_path), "-o", str(heic_path)])
    else:
        raise RuntimeError("host lacks heif-enc for negative HEIC fixture")
    fixtures["bad_heic.heic"] = heic_path.read_bytes()

    jp2_path = tmp_dir / "bad_jp2.jp2"
    j2k_path = tmp_dir / "bad_j2k.j2k"
    if opj_compress:
        run_host([opj_compress, "-i", str(base_ppm), "-o", str(jp2_path)])
        run_host([opj_compress, "-i", str(base_ppm), "-o", str(j2k_path)])
    else:
        run_host([ffmpeg, "-y", "-f", "image2", "-i", str(base_ppm), "-c:v", "jpeg2000", str(jp2_path)])
        run_host([ffmpeg, "-y", "-f", "image2", "-i", str(base_ppm), "-c:v", "jpeg2000", str(j2k_path)])
    fixtures["bad_jp2.jp2"] = jp2_path.read_bytes()
    fixtures["bad_j2k.j2k"] = j2k_path.read_bytes()

    if heif_enc:
        j2k_heif_path = tmp_dir / "bad_j2k.heif"
        res = subprocess.run([heif_enc, "--j2k", str(png_path), "-o", str(j2k_heif_path)], capture_output=True, text=True)
        if res.returncode == 0 and j2k_heif_path.exists():
            fixtures["bad_j2k.heif"] = j2k_heif_path.read_bytes()

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
            if name in fixtures:
                data = fixtures[name]
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(data)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(data)
                return
            if self.path in ("/control", "/sandbox-leak"):
                body = b"ok\n"
                self.send_response(200)
                self.send_header("Content-Type", "text/plain")
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


def parse_rgb(text, fmt=""):
    parts = [int(x) for x in text.split(",")]
    if fmt.startswith("B8g8r8"):
        return (parts[2], parts[1], parts[0])
    return tuple(parts[:3])


def close_rgb(got, expected, tol):
    return all(abs(a - b) <= tol for a, b in zip(got, expected))


def run_glycin_load(vm_cls, args, out):
    with tempfile.TemporaryDirectory(prefix="ferrion-glycin-fixtures-") as tmp:
        fixtures = generate_fixtures(Path(tmp))
        server, requests_log, lock = start_fixture_server(fixtures)
        port = server.server_address[1]
        transcript = []
        problems = []
        results = {}

        def run(vm, tag, cmd, timeout=120):
            since = vm.mark()
            vm.type(f"bash -c '{cmd}; echo gly-end-{tag}-$?'\n")
            end = rf"(?s)\n(.*)gly-end-{tag}-(\d+)"
            m = vm.wait_serial(end, timeout, since)
            if not m:
                tail = vm.text(since)
                transcript.append(f"$ {cmd}\n[TIMEOUT {timeout}s]\n{tail}\n")
                return None, tail
            status = int(m.group(2))
            output = m.group(1)
            transcript.append(f"$ {cmd}\n{output}exit {status}\n")
            return status, output

        vm = vm_cls(args, out)
        try:
            if not vm.wait_serial(r"login: ", args.timeout):
                return False, f"no serial login prompt within {args.timeout}s"
            vm.type("user\n")
            time.sleep(3)

            rc, text = run(vm, "supported", "glycin-load-probe supported", timeout=60)
            if rc != 0:
                problems.append(f"glycin-load-probe supported failed (exit {rc})")
            else:
                mimes = {line.split(None, 1)[1].strip() for line in text.splitlines() if line.startswith("MIME ")}
                for req in REQUIRED_MIMES:
                    if req not in mimes:
                        problems.append(f"missing supported MIME type {req}")
                for bad in FORBIDDEN_MIMES:
                    if bad in mimes:
                        problems.append(f"forbidden JPEG 2000 MIME type present: {bad}")

            dl_names = " ".join(sorted(fixtures.keys()))
            rc, text = run(
                vm,
                "download",
                f"mkdir -p /tmp/glycin-fx && for f in {dl_names}; do curl -fsS http://10.0.2.2:{port}/$f -o /tmp/glycin-fx/$f || exit 1; done",
                timeout=120,
            )
            if rc != 0:
                problems.append(f"fixture download failed (exit {rc})")

            positive_specs = [
                ("sample.png", 64, 64, 4),
                ("sample.jpg", 64, 64, 24),
                ("sample.webp", 64, 64, 4),
                ("sample.tiff", 64, 64, 4),
                ("sample.avif", 64, 64, 8),
                ("sample_av1.heif", 64, 64, 8),
                ("sample.jxl", 64, 64, 4),
                ("sample.svg", 64, 64, 16),
                ("sample_24mp.png", 6000, 4000, 4),
            ]
            load_re = re.compile(
                r"GLYCIN_LOAD_OK path=(\S+) mime=(\S+) sandbox=(\S+) w=(\d+) h=(\d+) stride=(\d+) fmt=(\S+) bytes=(\d+) ms=(\d+) sha256=([0-9a-f]{64}) tl=(\S+) mid=(\S+) br=(\S+)"
            )
            for name, exp_w, exp_h, tol in positive_specs:
                rc, text = run(vm, f"load-{name.replace('.', '_')}", f"glycin-load-probe load /tmp/glycin-fx/{name}", timeout=120)
                m = load_re.search(text or "")
                if rc != 0 or not m:
                    problems.append(f"positive load {name} failed (exit {rc})")
                    continue
                mime, sandbox, w, h, stride, fmt, nbytes, ms, sha, tl, mid, br = (
                    m.group(2), m.group(3), int(m.group(4)), int(m.group(5)),
                    int(m.group(6)), m.group(7), int(m.group(8)), int(m.group(9)),
                    m.group(10), m.group(11), m.group(12), m.group(13),
                )
                results[name] = {
                    "mime": mime, "sandbox": sandbox, "w": w, "h": h,
                    "stride": stride, "fmt": fmt, "bytes": nbytes,
                    "ms": ms, "sha256": sha, "tl": tl, "br": br,
                }
                if sandbox != "FerrionRun":
                    problems.append(f"{name}: wrong sandbox {sandbox}")
                if (w, h) != (exp_w, exp_h):
                    problems.append(f"{name}: wrong dimensions {w}x{h} != {exp_w}x{exp_h}")
                if name == "sample_24mp.png":
                    if nbytes != 6000 * 4000 * 4:
                        problems.append(f"{name}: expected 96000000 bytes, got {nbytes}")
                    if not close_rgb(parse_rgb(tl, fmt), (220, 40, 40), tol):
                        problems.append(f"{name}: tl pixel {tl} mismatch")
                else:
                    if not close_rgb(parse_rgb(tl, fmt), (220, 40, 40), tol):
                        problems.append(f"{name}: tl pixel {tl} mismatch")
                    if not close_rgb(parse_rgb(br, fmt), (230, 220, 40), tol):
                        problems.append(f"{name}: br pixel {br} mismatch")

            negative_files = ["bad_heic.heic", "bad_jp2.jp2", "bad_j2k.j2k"]
            if "bad_j2k.heif" in fixtures:
                negative_files.append("bad_j2k.heif")
            for name in negative_files:
                rc, text = run(vm, f"neg-{name.replace('.', '_')}", f"glycin-load-probe load /tmp/glycin-fx/{name} --expect-fail", timeout=60)
                if rc != 0 or "GLYCIN_REFUSED" not in text:
                    problems.append(f"negative fixture {name} was not refused (exit {rc})")

            rc, text = run(
                vm,
                "setup-escape",
                (
                    "printf FERRION_SECRET_M3B > /home/user/glycin-secret.txt && "
                    "chmod 600 /home/user/glycin-secret.txt && "
                    "rm -f /tmp/glycin-trap.sock /tmp/glycin-trap.count && "
                    "(glycin-load-probe unix-listener /tmp/glycin-trap.sock /tmp/glycin-trap.count 90 > /tmp/glycin-listener.log 2>&1 &) && "
                    "for i in $(seq 1 40); do grep -q UNIX_LISTENER_READY /tmp/glycin-listener.log && break; sleep 0.2; done && "
                    "grep UNIX_LISTENER_READY /tmp/glycin-listener.log && "
                    "mkdir -p /tmp/glycin-xdg/glycin-loaders/2+/conf.d && "
                    "printf \"[loader:image/x-ferrion-escape]\\nExec=/usr/bin/glycin-load-probe\\nIdentifiers=0:FERRION_ESCAPE;\\n\" "
                    "> /tmp/glycin-xdg/glycin-loaders/2+/conf.d/ferrion-escape.conf && "
                    f"printf \"FERRION_ESCAPE\\n/home/user/glycin-secret.txt\\n10.0.2.2:{port}\\n/tmp/glycin-trap.sock\\n\" > /tmp/escape.bin"
                ),
                timeout=60,
            )
            if rc != 0:
                problems.append(f"escape test setup failed (exit {rc})")

            rc, text = run(
                vm,
                "escape-control",
                f"glycin-load-probe sandbox-control /home/user/glycin-secret.txt 10.0.2.2:{port} /tmp/glycin-trap.sock",
                timeout=60,
            )
            if rc != 0 or "SANDBOX_CONTROL" not in text:
                problems.append(f"unsandboxed escape control failed (exit {rc}): {text!r}")

            rc, text = run(
                vm,
                "escape-sandbox",
                "XDG_DATA_HOME=/tmp/glycin-xdg glycin-load-probe sandbox-probe /tmp/escape.bin",
                timeout=60,
            )
            if rc != 0 or "SANDBOX_PROBE_OK mech=FerrionRun" not in text:
                problems.append(f"sandboxed escape probe failed (exit {rc}): {text!r}")

            rc, text = run(
                vm,
                "escape-disable-env",
                "GLYCIN_DISABLE_SANDBOX=i-know-the-risks XDG_DATA_HOME=/tmp/glycin-xdg glycin-load-probe sandbox-probe /tmp/escape.bin",
                timeout=60,
            )
            if rc != 0 or "SANDBOX_PROBE_OK mech=FerrionRun" not in text:
                problems.append(f"GLYCIN_DISABLE_SANDBOX bypass check failed (exit {rc}): {text!r}")

            rc, text = run(vm, "unix-count", "cat /tmp/glycin-trap.count && rm -f /tmp/glycin-trap.sock", timeout=30)
            count_val = text.strip().splitlines()[0].strip() if text and text.strip() else ""
            if count_val != "1":
                problems.append(f"planted Unix socket saw unexpected connection count {count_val!r} (expected 1 from control only)")

            with lock:
                leak_reqs = [r for r in requests_log if r["path"] == "/sandbox-leak"]
                ctrl_reqs = [r for r in requests_log if r["path"] == "/control"]
            if not ctrl_reqs:
                problems.append("host HTTP server did not see /control request")
            if leak_reqs:
                problems.append(f"host HTTP server saw {len(leak_reqs)} /sandbox-leak requests from sandboxed loader")

            (out / "glycin-load.txt").write_text("".join(transcript))
            (out / "glycin-load.json").write_text(json.dumps({
                "results": results,
                "http_requests": requests_log,
                "problems": problems,
            }, indent=2))

            if problems:
                return False, "; ".join(problems) + f"; see {out}"
            mp24_ms = results.get("sample_24mp.png", {}).get("ms", "?")
            return True, (
                f"{len(positive_specs)}/{len(positive_specs)} positive formats decoded under FerrionRun "
                f"(24MP 96MB pipe in {mp24_ms}ms), {len(negative_files)} negative formats refused, "
                f"sandbox blocked secret/TCP(0 leaks)/Unix(0 leaks) with and without GLYCIN_DISABLE_SANDBOX; see {out}"
            )
        finally:
            vm.close()
            server.shutdown()
            server.server_close()
