#!/usr/bin/env python3

import functools
import hashlib
import http.server
import importlib.util
import json
import math
import shutil
import struct
import subprocess
import threading
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MAX_UPLOAD_BYTES = 16 * 1024 * 1024
MAX_CLIP_BYTES = 64 * 1024 * 1024
UPLOAD_CHUNK_BYTES = 16384
MAX_UPLOAD_CHUNKS = 1024
MAX_CLIPS = 32
MAX_CODEC_LINES = 2048
MAX_PCM_SAMPLES = 4_000_000
LOSSY_MIN_SNR_DB = 60.0
BENCH_4K_SECONDS = 5
BENCH_4K_FPS = 30
BENCH_4K_FRAMES = BENCH_4K_SECONDS * BENCH_4K_FPS

FORBIDDEN_DECODERS = ("av1", "h264", "hevc", "vvc", "aac", "ac3", "eac3", "dca", "truehd")


def load_policy():
    spec = importlib.util.spec_from_file_location("media_codec_check", ROOT / "scripts" / "media-codec-check.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sha256_file(path):
    digest = hashlib.sha256()
    total = 0
    with open(path, "rb") as fh:
        for _ in range(MAX_CLIP_BYTES // 65536 + 1):
            chunk = fh.read(65536)
            if not chunk:
                return digest.hexdigest()
            total += len(chunk)
            if total > MAX_CLIP_BYTES:
                raise ValueError(f"{path} exceeds bound {MAX_CLIP_BYTES}")
            digest.update(chunk)
    raise ValueError(f"{path} exceeded chunk read bound")


def run_host_ffmpeg(cmd_args, capture_stdout=False, timeout=180):
    proc = subprocess.run(
        ["ffmpeg", "-hide_banner", "-nostdin", "-y", *cmd_args],
        capture_output=True,
        timeout=timeout,
    )
    if proc.returncode != 0:
        err = proc.stderr.decode("utf-8", errors="replace")[-1000:]
        raise RuntimeError(f"host ffmpeg failed ({proc.returncode}): {' '.join(cmd_args)}\n{err}")
    return proc.stdout if capture_stdout else proc.stderr.decode("utf-8", errors="replace")


def parse_codec_list(text):
    lines = text.splitlines()
    if len(lines) > MAX_CODEC_LINES:
        raise ValueError(f"codec listing exceeded bound {MAX_CODEC_LINES}")
    names = set()
    in_body = False
    for line in lines:
        if line.strip().startswith("------"):
            in_body = True
            continue
        if not in_body:
            continue
        match = re.match(r"^\s+[VASD\.][F\.][S\.][X\.][B\.][D\.]\s+(\S+)", line)
        if match:
            names.add(match.group(1))
    return names


def framemd5_lines(text):
    return [line.strip() for line in text.splitlines() if line.strip() and not line.lstrip().startswith("#")]


def compute_f32_snr_db(ref_bytes, test_bytes):
    if len(ref_bytes) != len(test_bytes) or len(ref_bytes) == 0 or (len(ref_bytes) % 4) != 0:
        return None, f"length mismatch (ref={len(ref_bytes)}, guest={len(test_bytes)})"
    count = len(ref_bytes) // 4
    if count > MAX_PCM_SAMPLES:
        raise ValueError(f"PCM sample count {count} exceeds bound {MAX_PCM_SAMPLES}")
    ref = struct.unpack(f"<{count}f", ref_bytes)
    got = struct.unpack(f"<{count}f", test_bytes)
    sig_power = 0.0
    err_power = 0.0
    for a, b in zip(ref, got):
        if not (math.isfinite(a) and math.isfinite(b)):
            return None, "non-finite sample"
        sig_power += a * a
        d = a - b
        err_power += d * d
    if sig_power == 0.0:
        return None, "zero reference signal"
    if err_power == 0.0:
        return float("inf"), "exact"
    return 10.0 * math.log10(sig_power / err_power), "ok"


def generate_clips(clips_dir, ref_dir):
    clips_dir.mkdir(parents=True, exist_ok=True)
    ref_dir.mkdir(parents=True, exist_ok=True)

    video_specs = [
        {
            "name": "av1_8bit.mkv",
            "encode": ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=1",
                       "-c:v", "libsvtav1", "-pix_fmt", "yuv420p", "-preset", "10"],
            "vcodec": "libdav1d",
            "pix_fmt": "yuv420p",
        },
        {
            "name": "av1_10bit.mp4",
            "encode": ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=1",
                       "-c:v", "libsvtav1", "-pix_fmt", "yuv420p10le", "-preset", "10"],
            "vcodec": "libdav1d",
            "pix_fmt": "yuv420p10le",
        },
        {
            "name": "vp9.webm",
            "encode": ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=1",
                       "-c:v", "libvpx-vp9", "-pix_fmt", "yuv420p", "-b:v", "200k"],
            "vcodec": "vp9",
            "pix_fmt": "yuv420p",
        },
        {
            "name": "vp8.webm",
            "encode": ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=1",
                       "-c:v", "libvpx", "-pix_fmt", "yuv420p", "-b:v", "200k"],
            "vcodec": "vp8",
            "pix_fmt": "yuv420p",
        },
        {
            "name": "theora.ogv",
            "encode": ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=1",
                       "-c:v", "libtheora", "-pix_fmt", "yuv420p", "-q:v", "5"],
            "vcodec": "theora",
            "pix_fmt": "yuv420p",
        },
        {
            "name": "mpeg2.ts",
            "encode": ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=1",
                       "-c:v", "mpeg2video", "-pix_fmt", "yuv420p", "-b:v", "400k"],
            "vcodec": "mpeg2video",
            "pix_fmt": "yuv420p",
        },
        {
            "name": "mpeg1_mp2.mpg",
            "encode": ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25:duration=1",
                       "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=1",
                       "-ac", "2", "-c:v", "mpeg1video", "-pix_fmt", "yuv420p", "-b:v", "400k",
                       "-c:a", "mp2", "-b:a", "192k"],
            "vcodec": "mpeg1video",
            "pix_fmt": "yuv420p",
            "acodec": "mp2float",
            "exact_audio": False,
        },
    ]

    audio_specs = [
        {
            "name": "flac.mkv",
            "encode": ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1",
                       "-ac", "2", "-c:a", "flac"],
            "acodec": "flac",
            "exact_audio": True,
        },
        {
            "name": "opus.ogg",
            "encode": ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1",
                       "-ac", "2", "-c:a", "libopus", "-b:a", "128k"],
            "acodec": "opus",
            "exact_audio": False,
        },
        {
            "name": "vorbis.ogg",
            "encode": ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=1",
                       "-ac", "2", "-c:a", "libvorbis", "-q:a", "4"],
            "acodec": "vorbis",
            "exact_audio": False,
        },
        {
            "name": "mp3.mp3",
            "encode": ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=1",
                       "-ac", "2", "-c:a", "libmp3lame", "-b:a", "192k"],
            "acodec": "mp3float",
            "exact_audio": False,
        },
    ]

    bench_spec = {
        "name": "av1_4k.mkv",
        "encode": ["-f", "lavfi", "-i", f"testsrc2=size=3840x2160:rate={BENCH_4K_FPS}:duration={BENCH_4K_SECONDS}",
                   "-c:v", "libsvtav1", "-pix_fmt", "yuv420p", "-preset", "10"],
        "vcodec": "libdav1d",
        "frames": BENCH_4K_FRAMES,
    }

    all_specs = video_specs + audio_specs + [bench_spec]
    assert len(all_specs) <= MAX_CLIPS

    hashes = {}
    for spec in all_specs:
        clip_path = clips_dir / spec["name"]
        run_host_ffmpeg([*spec["encode"], str(clip_path)])
        hashes[spec["name"]] = sha256_file(clip_path)

    for spec in video_specs:
        clip_path = clips_dir / spec["name"]
        md5_out = run_host_ffmpeg(
            ["-c:v", spec["vcodec"], "-i", str(clip_path), "-map", "0:v:0",
             "-pix_fmt", spec["pix_fmt"], "-f", "framemd5", "-"],
            capture_stdout=True,
        ).decode("utf-8", errors="replace")
        (ref_dir / f"{spec['name']}.framemd5").write_text(md5_out, encoding="utf-8")
        if "acodec" in spec:
            pcm_out = run_host_ffmpeg(
                ["-c:a", spec["acodec"], "-i", str(clip_path), "-map", "0:a:0",
                 "-f", "f32le", "-c:a", "pcm_f32le", "-"],
                capture_stdout=True,
            )
            (ref_dir / f"{spec['name']}.f32le").write_bytes(pcm_out)

    for spec in audio_specs:
        clip_path = clips_dir / spec["name"]
        fmt = "s16le" if spec["exact_audio"] else "f32le"
        enc = "pcm_s16le" if spec["exact_audio"] else "pcm_f32le"
        pcm_out = run_host_ffmpeg(
            ["-c:a", spec["acodec"], "-i", str(clip_path), "-map", "0:a:0",
             "-f", fmt, "-c:a", enc, "-"],
            capture_stdout=True,
        )
        (ref_dir / f"{spec['name']}.{fmt}").write_bytes(pcm_out)

    return video_specs, audio_specs, bench_spec, hashes


def start_clip_server(clips_dir, upload_dir):
    upload_dir.mkdir(parents=True, exist_ok=True)
    chunk_counts = {}

    class Handler(http.server.SimpleHTTPRequestHandler):
        def do_POST(self):
            raw = self.path.lstrip("/")
            append = False
            if raw.startswith("append/"):
                append = True
                raw = raw[len("append/"):]
            name = Path(raw).name
            if not name or "/" in raw or name in (".", ".."):
                self.send_error(400, "invalid name")
                return
            length = int(self.headers.get("Content-Length", "0"))
            if length < 0 or length > MAX_UPLOAD_BYTES:
                self.send_error(413, "payload exceeds bound")
                return
            data = self.rfile.read(length)
            target = upload_dir / name
            if append:
                count = chunk_counts.get(name, 1) + 1
                if count > MAX_UPLOAD_CHUNKS:
                    self.send_error(413, "chunk count exceeds bound")
                    return
                existing = target.stat().st_size if target.exists() else 0
                if existing + len(data) > MAX_UPLOAD_BYTES:
                    self.send_error(413, "appended payload exceeds bound")
                    return
                with open(target, "ab") as fh:
                    fh.write(data)
                chunk_counts[name] = count
            else:
                target.write_bytes(data)
                chunk_counts[name] = 1
            self.send_response(200)
            self.end_headers()

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(
        ("127.0.0.1", 0),
        functools.partial(Handler, directory=str(clips_dir)),
    )
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def run_media_decode(vm, args, helpers):
    Blocked = helpers["Blocked"]
    root_shell = helpers["root_shell"]
    run_status = helpers["run_status"]

    if not shutil.which("ffmpeg"):
        raise Blocked("host lacks ffmpeg")

    policy = load_policy()
    clips_dir = vm.out / "clips"
    ref_dir = vm.out / "host-ref"
    guest_dir = vm.out / "guest-out"

    video_specs, audio_specs, bench_spec, clip_hashes = generate_clips(clips_dir, ref_dir)
    record = {
        "clip_sha256": clip_hashes,
        "video": {},
        "audio": {},
        "benchmark_4k_av1": {},
    }

    server = start_clip_server(clips_dir, guest_dir)
    port = server.server_address[1]
    try:
        if not root_shell(vm, args.timeout):
            return False, f"root serial login failed; see {vm.out}"

        code, net_text = run_status(
            vm,
            'bash -c \'deadline=$((SECONDS+60)); server=$(cat /etc/net/dns); '
            'while [[ -z "$server" || "$server" == 9.9.9.9 ]] && (( SECONDS < deadline )); do '
            'sleep 0.1; server=$(cat /etc/net/dns); done; '
            '[[ -n "$server" && "$server" != 9.9.9.9 ]]\'',
            timeout=70,
        )
        if code != 0:
            return False, f"DHCP did not finish: {net_text.strip()}; see {vm.out}"

        code, which_out = run_status(vm, "bash -c 'command -v ffmpeg'", timeout=30)
        if code != 0:
            record["failure"] = "ffmpeg not installed"
            (vm.out / "media-decode.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
            return False, f"ffmpeg is not installed in guest; see {vm.out}"

        code, pat_out = run_status(
            vm,
            "bash -c 'grep -q \"Alliance for Open Media Patent License 1.0\" /usr/share/licenses/dav1d/PATENTS'",
            timeout=30,
        )
        if code != 0:
            return False, f"missing or incomplete /usr/share/licenses/dav1d/PATENTS: {pat_out.strip()}; see {vm.out}"

        base = f"http://10.0.2.2:{port}"
        curl_post = "curl --http1.0 -H Expect: -fsS --max-time 30 --retry 2 --data-binary"
        curl_get = "curl --http1.0 -fsS --max-time 30 --retry 2"
        code, meta_out = run_status(
            vm,
            "bash -c '"
            "mkdir -p /tmp/media && "
            "ffmpeg -hide_banner -L > /tmp/media/license.txt 2>&1 && "
            "ffmpeg -hide_banner -buildconf > /tmp/media/buildconf.txt 2>&1 && "
            "ffmpeg -hide_banner -decoders > /tmp/media/decoders.txt 2>&1 && "
            "ffmpeg -hide_banner -encoders > /tmp/media/encoders.txt 2>&1 && "
            f"{curl_post} @/tmp/media/license.txt {base}/license.txt && "
            f"{curl_post} @/tmp/media/buildconf.txt {base}/buildconf.txt && "
            f"{curl_post} @/tmp/media/decoders.txt {base}/decoders.txt && "
            f"{curl_post} @/tmp/media/encoders.txt {base}/encoders.txt"
            "'",
            timeout=60,
        )
        if code != 0:
            return False, f"failed to collect guest ffmpeg metadata (rc={code}): {meta_out.strip()}; see {vm.out}"

        license_text = (guest_dir / "license.txt").read_text(encoding="utf-8", errors="replace")
        buildconf_text = (guest_dir / "buildconf.txt").read_text(encoding="utf-8", errors="replace")
        decoders_text = (guest_dir / "decoders.txt").read_text(encoding="utf-8", errors="replace")
        encoders_text = (guest_dir / "encoders.txt").read_text(encoding="utf-8", errors="replace")

        problems = []
        if "Lesser General Public License" not in license_text:
            problems.append("ffmpeg -L does not report LGPL")
        if "--disable-everything" not in buildconf_text or "--disable-autodetect" not in buildconf_text:
            problems.append("ffmpeg -buildconf missing --disable-everything or --disable-autodetect")
        if "--enable-libdav1d" not in buildconf_text:
            problems.append("ffmpeg -buildconf missing --enable-libdav1d")
        for bad_flag in ("--enable-gpl", "--enable-nonfree", "--enable-sdl2", "--enable-ffplay"):
            if bad_flag in buildconf_text:
                problems.append(f"ffmpeg -buildconf contains forbidden {bad_flag}")

        guest_decoders = parse_codec_list(decoders_text)
        guest_encoders = parse_codec_list(encoders_text)
        record["guest_decoder_count"] = len(guest_decoders)
        record["guest_encoder_count"] = len(guest_encoders)

        if "libdav1d" not in guest_decoders:
            problems.append("guest ffmpeg lacks libdav1d decoder")
        extra_dec = sorted(guest_decoders - policy.ALLOWED_IMAGE_DECODERS)
        if extra_dec:
            problems.append(f"guest decoders outside allow-list: {extra_dec}")
        bad_dec = sorted(guest_decoders & set(FORBIDDEN_DECODERS))
        if bad_dec:
            problems.append(f"guest exposes forbidden decoders: {bad_dec}")
        extra_enc = sorted(guest_encoders - policy.ALLOWED_IMAGE_ENCODERS)
        if extra_enc:
            problems.append(f"guest encoders outside allow-list: {extra_enc}")

        for name, expected_hash in clip_hashes.items():
            code, dl_out = run_status(
                vm,
                f"bash -c '{curl_get} {base}/{name} -o /tmp/media/{name} && "
                f"got=$(sha256sum /tmp/media/{name} | cut -d\" \" -f1) && "
                f"test \"$got\" = \"{expected_hash}\"'",
                timeout=90,
            )
            if code is None:
                return False, f"timeout downloading {name}; see {vm.out}"
            if code != 0:
                problems.append(f"download or sha256 verification failed for {name}: {dl_out.strip()}")

        if problems:
            record["problems"] = problems
            (vm.out / "media-decode.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
            return False, "; ".join(problems) + f"; see {vm.out}"

        for spec in video_specs:
            name = spec["name"]
            out_name = f"{name}.framemd5"
            code, cmd_out = run_status(
                vm,
                f"bash -c 'ffmpeg -hide_banner -nostdin -c:v {spec['vcodec']} -i /tmp/media/{name} "
                f"-map 0:v:0 -pix_fmt {spec['pix_fmt']} -f framemd5 /tmp/media/{out_name} && "
                f"{curl_post} @/tmp/media/{out_name} {base}/{out_name}'",
                timeout=120,
            )
            if code is None:
                return False, f"timeout decoding {name}; see {vm.out}"
            if code != 0:
                problems.append(f"video decode failed for {name} (rc={code}): {cmd_out.strip()[-300:]}")
                continue
            ref_lines = framemd5_lines((ref_dir / out_name).read_text(encoding="utf-8"))
            got_lines = framemd5_lines((guest_dir / out_name).read_text(encoding="utf-8"))
            matched = bool(ref_lines) and ref_lines == got_lines
            record["video"][name] = {
                "vcodec": spec["vcodec"],
                "pix_fmt": spec["pix_fmt"],
                "frames": len(ref_lines),
                "match": matched,
            }
            if not matched:
                problems.append(f"framemd5 mismatch for {name} (host={len(ref_lines)} guest={len(got_lines)})")

            if "acodec" in spec:
                a_out = f"{name}.f32le"
                ref_bytes = (ref_dir / a_out).read_bytes()
                ref_sha = hashlib.sha256(ref_bytes).hexdigest()
                code, a_cmd = run_status(
                    vm,
                    f"bash -c 'ffmpeg -hide_banner -nostdin -c:a {spec['acodec']} -i /tmp/media/{name} "
                    f"-map 0:a:0 -f f32le -c:a pcm_f32le /tmp/media/{a_out} && "
                    f"got=$(sha256sum /tmp/media/{a_out} | cut -d\" \" -f1) && "
                    f"if test \"$got\" = \"{ref_sha}\"; then echo MEDIA_AUDIO''_EXACT; else "
                    f"rm -f /tmp/media/{a_out}.part.*; split -b {UPLOAD_CHUNK_BYTES} /tmp/media/{a_out} /tmp/media/{a_out}.part. && "
                    f"first=1; for p in /tmp/media/{a_out}.part.*; do "
                    f"if test $first -eq 1; then {curl_post} \"@$p\" {base}/{a_out} || exit 1; first=0; "
                    f"else {curl_post} \"@$p\" {base}/append/{a_out} || exit 1; fi; done; fi'",
                    timeout=120,
                )
                if code is None:
                    return False, f"timeout decoding audio {name}; see {vm.out}"
                if code != 0:
                    problems.append(f"audio decode failed for {name} (rc={code}): {a_cmd.strip()[-300:]}")
                elif not (guest_dir / a_out).is_file() and re.search(r"(?m)^MEDIA_AUDIO_EXACT$", a_cmd):
                    record["audio"][name] = {
                        "acodec": spec["acodec"],
                        "bytes": len(ref_bytes),
                        "sha256": ref_sha,
                        "snr_db": "inf",
                        "note": "exact",
                        "ok": True,
                    }
                else:
                    snr_db, note = compute_f32_snr_db(ref_bytes, (guest_dir / a_out).read_bytes())
                    ok = snr_db is not None and snr_db >= LOSSY_MIN_SNR_DB
                    record["audio"][name] = {
                        "acodec": spec["acodec"],
                        "snr_db": "inf" if snr_db == float("inf") else (round(snr_db, 2) if snr_db is not None else None),
                        "note": note,
                        "ok": ok,
                    }
                    if not ok:
                        problems.append(f"audio SNR failed for {name}: {snr_db} dB ({note})")

        for spec in audio_specs:
            name = spec["name"]
            fmt = "s16le" if spec["exact_audio"] else "f32le"
            enc = "pcm_s16le" if spec["exact_audio"] else "pcm_f32le"
            a_out = f"{name}.{fmt}"
            ref_bytes = (ref_dir / a_out).read_bytes()
            ref_sha = hashlib.sha256(ref_bytes).hexdigest()
            if spec["exact_audio"]:
                code, a_cmd = run_status(
                    vm,
                    f"bash -c 'ffmpeg -hide_banner -nostdin -c:a {spec['acodec']} -i /tmp/media/{name} "
                    f"-map 0:a:0 -f {fmt} -c:a {enc} /tmp/media/{a_out} && "
                    f"got=$(sha256sum /tmp/media/{a_out} | cut -d\" \" -f1) && "
                    f"test \"$got\" = \"{ref_sha}\"'",
                    timeout=120,
                )
                if code is None:
                    return False, f"timeout decoding exact audio {name}; see {vm.out}"
                exact = code == 0 and len(ref_bytes) > 0
                record["audio"][name] = {
                    "acodec": spec["acodec"],
                    "bytes": len(ref_bytes),
                    "sha256": ref_sha,
                    "exact": exact,
                }
                if not exact:
                    problems.append(f"exact PCM mismatch for {name} (rc={code}): {a_cmd.strip()[-300:]}")
                continue
            code, a_cmd = run_status(
                vm,
                f"bash -c 'ffmpeg -hide_banner -nostdin -c:a {spec['acodec']} -i /tmp/media/{name} "
                f"-map 0:a:0 -f {fmt} -c:a {enc} /tmp/media/{a_out} && "
                f"got=$(sha256sum /tmp/media/{a_out} | cut -d\" \" -f1) && "
                f"if test \"$got\" = \"{ref_sha}\"; then echo MEDIA_AUDIO''_EXACT; else "
                f"rm -f /tmp/media/{a_out}.part.*; split -b {UPLOAD_CHUNK_BYTES} /tmp/media/{a_out} /tmp/media/{a_out}.part. && "
                f"first=1; for p in /tmp/media/{a_out}.part.*; do "
                f"if test $first -eq 1; then {curl_post} \"@$p\" {base}/{a_out} || exit 1; first=0; "
                f"else {curl_post} \"@$p\" {base}/append/{a_out} || exit 1; fi; done; fi'",
                timeout=120,
            )
            if code is None:
                return False, f"timeout decoding audio {name}; see {vm.out}"
            if code != 0:
                problems.append(f"audio decode failed for {name} (rc={code}): {a_cmd.strip()[-300:]}")
                continue
            if not (guest_dir / a_out).is_file() and re.search(r"(?m)^MEDIA_AUDIO_EXACT$", a_cmd):
                record["audio"][name] = {
                    "acodec": spec["acodec"],
                    "bytes": len(ref_bytes),
                    "sha256": ref_sha,
                    "snr_db": "inf",
                    "note": "exact",
                    "ok": True,
                }
                continue
            got_bytes = (guest_dir / a_out).read_bytes()
            snr_db, note = compute_f32_snr_db(ref_bytes, got_bytes)
            ok = snr_db is not None and snr_db >= LOSSY_MIN_SNR_DB
            record["audio"][name] = {
                "acodec": spec["acodec"],
                "snr_db": "inf" if snr_db == float("inf") else (round(snr_db, 2) if snr_db is not None else None),
                "note": note,
                "ok": ok,
            }
            if not ok:
                problems.append(f"audio SNR failed for {name}: {snr_db} dB ({note})")

        b_name = bench_spec["name"]
        code, bench_out = run_status(
            vm,
            f"bash -c 'ffmpeg -hide_banner -nostdin -benchmark -c:v libdav1d "
            f"-i /tmp/media/{b_name} -map 0:v:0 -f null - > /tmp/media/bench.txt 2>&1 && "
            f"{curl_post} @/tmp/media/bench.txt {base}/bench.txt'",
            timeout=180,
        )
        if code != 0:
            problems.append(f"4K AV1 benchmark decode failed (rc={code}): {bench_out.strip()[-300:]}")
        else:
            bench_text = (guest_dir / "bench.txt").read_text(encoding="utf-8", errors="replace")
            bench_match = re.search(
                r"bench:\s+utime=(-?[0-9.]+)s\s+stime=(-?[0-9.]+)s\s+rtime=([0-9.]+)s",
                bench_text,
            )
            frame_matches = re.findall(r"frame=\s*(\d+)", bench_text)
            decoded_frames = int(frame_matches[-1]) if frame_matches else 0
            if not bench_match:
                problems.append("4K AV1 benchmark output missing bench timing line")
            elif decoded_frames != bench_spec["frames"]:
                problems.append(
                    f"4K AV1 benchmark decoded {decoded_frames} frames, expected {bench_spec['frames']}"
                )
            else:
                utime = float(bench_match.group(1))
                stime = float(bench_match.group(2))
                rtime = float(bench_match.group(3))
                fps = decoded_frames / rtime if rtime > 0 else 0.0
                record["benchmark_4k_av1"] = {
                    "frames": decoded_frames,
                    "utime_s": utime,
                    "stime_s": stime,
                    "rtime_s": rtime,
                    "fps": round(fps, 2),
                }

        record["problems"] = problems
        (vm.out / "media-decode.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        if problems:
            return False, "; ".join(problems) + f"; see {vm.out}"
        fps_str = record["benchmark_4k_av1"].get("fps", 0.0)
        exact_audio = sum(1 for spec in audio_specs if spec["exact_audio"])
        lossy_audio = sum(1 for spec in audio_specs if not spec["exact_audio"]) + sum(
            1 for spec in video_specs if "acodec" in spec
        )
        return True, (
            f"{len(video_specs)} video framemd5 exact, {exact_audio} exact audio, {lossy_audio} lossy audio SNR>={LOSSY_MIN_SNR_DB:.0f} dB, "
            f"decoders={len(guest_decoders)}, encoders={len(guest_encoders)}, 4K AV1={fps_str} fps; see {vm.out}"
        )
    finally:
        server.shutdown()
        server.server_close()


def generate_mpv_clips(clips_dir, ref_dir):
    clips_dir.mkdir(parents=True, exist_ok=True)
    ref_dir.mkdir(parents=True, exist_ok=True)

    video_specs = [
        {
            "name": "av1_8bit.mkv",
            "encode": ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=1",
                       "-c:v", "libsvtav1", "-pix_fmt", "yuv420p", "-preset", "10"],
            "vcodec": "libdav1d",
            "pix_fmt": "yuv420p",
        },
        {
            "name": "av1_10bit.mp4",
            "encode": ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=1",
                       "-c:v", "libsvtav1", "-pix_fmt", "yuv420p10le", "-preset", "10"],
            "vcodec": "libdav1d",
            "pix_fmt": "yuv420p10le",
        },
        {
            "name": "vp9.webm",
            "encode": ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=1",
                       "-c:v", "libvpx-vp9", "-pix_fmt", "yuv420p", "-b:v", "200k"],
            "vcodec": "vp9",
            "pix_fmt": "yuv420p",
        },
        {
            "name": "mpeg1_mp2.mpg",
            "encode": ["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25:duration=1",
                       "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=1",
                       "-ac", "2", "-c:v", "mpeg1video", "-pix_fmt", "yuv420p", "-b:v", "400k",
                       "-c:a", "mp2", "-b:a", "192k"],
            "vcodec": "mpeg1video",
            "pix_fmt": "yuv420p",
        },
    ]

    audio_specs = [
        {
            "name": "opus.ogg",
            "encode": ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1",
                       "-ac", "2", "-c:a", "libopus", "-b:a", "128k"],
        },
    ]

    hashes = {}
    ref_frames = {}
    for spec in video_specs + audio_specs:
        clip_path = clips_dir / spec["name"]
        run_host_ffmpeg([*spec["encode"], str(clip_path)])
        hashes[spec["name"]] = sha256_file(clip_path)

    for spec in video_specs:
        clip_path = clips_dir / spec["name"]
        md5_out = run_host_ffmpeg(
            ["-c:v", spec["vcodec"], "-i", str(clip_path), "-map", "0:v:0",
             "-pix_fmt", spec["pix_fmt"], "-f", "framemd5", "-"],
            capture_stdout=True,
        ).decode("utf-8", errors="replace")
        (ref_dir / f"{spec['name']}.framemd5").write_text(md5_out, encoding="utf-8")
        ref_frames[spec["name"]] = len(framemd5_lines(md5_out))

    return video_specs, audio_specs, hashes, ref_frames


def parse_mpv_log(text):
    frametime_count = len(re.findall(r"\[cplayer\]\s+frametime=", text))
    eof = "Exiting... (End of file)" in text
    drop_match = re.search(r"Dropped:\s*(\d+)", text)
    dropped = int(drop_match.group(1)) if drop_match else 0
    vo_null = "VO: [null]" in text
    ao_null = "AO: [null]" in text
    return {
        "frametime_count": frametime_count,
        "eof": eof,
        "dropped": dropped,
        "vo_null": vo_null,
        "ao_null": ao_null,
    }


def run_mpv_headless(vm, args, helpers):
    Blocked = helpers["Blocked"]
    root_shell = helpers["root_shell"]
    run_status = helpers["run_status"]

    if not shutil.which("ffmpeg"):
        raise Blocked("host lacks ffmpeg")

    clips_dir = vm.out / "clips"
    ref_dir = vm.out / "host-ref"
    guest_dir = vm.out / "guest-out"

    video_specs, audio_specs, clip_hashes, ref_frames = generate_mpv_clips(clips_dir, ref_dir)
    record = {
        "clip_sha256": clip_hashes,
        "ref_frames": ref_frames,
        "video": {},
        "audio": {},
    }

    server = start_clip_server(clips_dir, guest_dir)
    port = server.server_address[1]
    try:
        if not root_shell(vm, args.timeout):
            return False, f"root serial login failed; see {vm.out}"

        code, net_text = run_status(
            vm,
            'bash -c \'deadline=$((SECONDS+60)); server=$(cat /etc/net/dns); '
            'while [[ -z "$server" || "$server" == 9.9.9.9 ]] && (( SECONDS < deadline )); do '
            'sleep 0.1; server=$(cat /etc/net/dns); done; '
            '[[ -n "$server" && "$server" != 9.9.9.9 ]]\'',
            timeout=70,
        )
        if code != 0:
            return False, f"DHCP did not finish: {net_text.strip()}; see {vm.out}"

        code, _ = run_status(vm, "bash -c 'command -v mpv'", timeout=30)
        if code != 0:
            record["failure"] = "mpv not installed"
            (vm.out / "mpv-headless.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
            return False, f"mpv is not installed in guest; see {vm.out}"

        base = f"http://10.0.2.2:{port}"
        curl_post = "curl --http1.0 -H Expect: -fsS --max-time 30 --retry 2 --data-binary"
        curl_get = "curl --http1.0 -fsS --max-time 30 --retry 2"

        code, ver_out = run_status(
            vm,
            "bash -c '"
            "mkdir -p /tmp/media && "
            "test -s /usr/share/licenses/mpv/LICENSE.LGPL && "
            "test -s /usr/share/licenses/mpv/Copyright && "
            "test ! -e /usr/share/licenses/mpv/LICENSE.GPL && "
            "test -s /usr/share/licenses/libplacebo/LICENSE && "
            "test -s /usr/share/licenses/libass/COPYING && "
            "mpv --version > /tmp/media/mpv-version.txt 2>&1 && "
            f"{curl_post} @/tmp/media/mpv-version.txt {base}/mpv-version.txt"
            "'",
            timeout=60,
        )
        if code != 0:
            return False, f"mpv version or license check failed (rc={code}): {ver_out.strip()[-300:]}; see {vm.out}"

        record["version"] = (guest_dir / "mpv-version.txt").read_text(encoding="utf-8", errors="replace").splitlines()[0]

        problems = []
        for name, expected_hash in clip_hashes.items():
            code, dl_out = run_status(
                vm,
                f"bash -c '{curl_get} {base}/{name} -o /tmp/media/{name} && "
                f"got=$(sha256sum /tmp/media/{name} | cut -d\" \" -f1) && "
                f"test \"$got\" = \"{expected_hash}\"'",
                timeout=90,
            )
            if code is None:
                return False, f"timeout downloading {name}; see {vm.out}"
            if code != 0:
                problems.append(f"download or sha256 failed for {name}: {dl_out.strip()}")

        if problems:
            record["problems"] = problems
            (vm.out / "mpv-headless.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
            return False, "; ".join(problems) + f"; see {vm.out}"

        for spec in video_specs:
            name = spec["name"]
            log_name = f"{name}.mpv.log"
            code, cmd_out = run_status(
                vm,
                f"bash -c 'rc=0; mpv --no-config --vo=null --ao=null --untimed --framedrop=no "
                f"--vd={spec['vcodec']} --msg-level=all=info,cplayer=trace /tmp/media/{name} "
                f"> /tmp/media/{log_name} 2>&1 || rc=$?; "
                f"{curl_post} @/tmp/media/{log_name} {base}/{log_name} || true; exit $rc'",
                timeout=120,
            )
            if code is None:
                return False, f"timeout running mpv on {name}; see {vm.out}"
            log_path = guest_dir / log_name
            log_text = log_path.read_text(encoding="utf-8", errors="replace") if log_path.is_file() else ""
            if code != 0:
                tail = (log_text or cmd_out).strip()[-400:]
                problems.append(f"mpv failed on {name} (rc={code}): {tail}")
                continue
            parsed = parse_mpv_log(log_text)
            expected = ref_frames[name]
            ok = (
                parsed["eof"]
                and parsed["vo_null"]
                and parsed["dropped"] == 0
                and parsed["frametime_count"] > 0
                and parsed["frametime_count"] == expected
            )
            parsed["expected_frames"] = expected
            parsed["ok"] = ok
            record["video"][name] = parsed
            if not ok:
                problems.append(
                    f"mpv check failed for {name}: frames={parsed['frametime_count']}/{expected} "
                    f"eof={parsed['eof']} vo_null={parsed['vo_null']} dropped={parsed['dropped']}"
                )

        for spec in audio_specs:
            name = spec["name"]
            log_name = f"{name}.mpv.log"
            code, cmd_out = run_status(
                vm,
                f"bash -c 'rc=0; mpv --no-config --vo=null --ao=null --untimed "
                f"--msg-level=all=info,cplayer=trace /tmp/media/{name} "
                f"> /tmp/media/{log_name} 2>&1 || rc=$?; "
                f"{curl_post} @/tmp/media/{log_name} {base}/{log_name} || true; exit $rc'",
                timeout=120,
            )
            if code is None:
                return False, f"timeout running mpv on {name}; see {vm.out}"
            log_path = guest_dir / log_name
            log_text = log_path.read_text(encoding="utf-8", errors="replace") if log_path.is_file() else ""
            if code != 0:
                tail = (log_text or cmd_out).strip()[-400:]
                problems.append(f"mpv failed on {name} (rc={code}): {tail}")
                continue
            parsed = parse_mpv_log(log_text)
            ok = parsed["eof"] and parsed["ao_null"]
            parsed["ok"] = ok
            record["audio"][name] = parsed
            if not ok:
                problems.append(f"mpv audio check failed for {name}: eof={parsed['eof']} ao_null={parsed['ao_null']}")

        record["problems"] = problems
        (vm.out / "mpv-headless.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        if problems:
            return False, "; ".join(problems) + f"; see {vm.out}"
        return True, (
            f"{len(video_specs)} video clips exact frame count + EOF, "
            f"{len(audio_specs)} audio clip EOF via mpv --vo=null --ao=null; see {vm.out}"
        )
    finally:
        server.shutdown()
        server.server_close()


WAV_HEADER_BYTES = 44
AUDIO_RATE = 44100
AUDIO_PERIOD_FRAMES = 512
SOUND_TONE_HZ = 1499
SOUND_ABSENT_HZ = 997
SOUND_DURATION_S = 3.0
SOUND_DURATION_TOL_S = 0.050
SOUND_TARGET_AMP = 0.25
SOUND_AMP_TOL_DB = 0.5
SOUND_ACTIVE_RMS = 0.05
SOUND_EDGE_GUARD_BLOCKS = 3
SOUND_MAX_PHASE_JUMP_RAD = 0.35
MAX_WAV_FRAMES = AUDIO_RATE * 60


MAX_WAV_CHUNKS = 32


def extract_wav_pcm(recording):
    if len(recording) < 12 or recording[:4] != b"RIFF" or recording[8:12] != b"WAVE":
        return None
    pos = 12
    for _ in range(MAX_WAV_CHUNKS):
        if pos + 8 > len(recording):
            return None
        cid = recording[pos:pos + 4]
        size = struct.unpack_from("<I", recording, pos + 4)[0]
        pos += 8
        if cid == b"data":
            end = len(recording) if size == 0 or size == 0xFFFFFFFF else min(len(recording), pos + size)
            return recording[pos:end]
        pos += size + (size & 1)
    return None


def analyze_mpv_wav(recording, expected_duration_s=SOUND_DURATION_S):
    data = extract_wav_pcm(recording)
    if not data:
        return {"ok": False, "reason": "empty or invalid WAV recording"}
    frame_count = len(data) // 4
    if frame_count > MAX_WAV_FRAMES:
        raise ValueError(f"WAV frame count {frame_count} exceeds bound {MAX_WAV_FRAMES}")
    samples = list(struct.iter_unpack("<hh", data[:frame_count * 4]))

    block_count = frame_count // AUDIO_PERIOD_FRAMES
    block_rms = []
    for b in range(block_count):
        base = b * AUDIO_PERIOD_FRAMES
        acc_l = 0.0
        acc_r = 0.0
        for i in range(AUDIO_PERIOD_FRAMES):
            l, r = samples[base + i]
            fl = l / 32767.0
            fr = r / 32767.0
            acc_l += fl * fl
            acc_r += fr * fr
        rms_l = math.sqrt(acc_l / AUDIO_PERIOD_FRAMES)
        rms_r = math.sqrt(acc_r / AUDIO_PERIOD_FRAMES)
        block_rms.append(min(rms_l, rms_r))

    active_blocks = [i for i, r in enumerate(block_rms) if r >= SOUND_ACTIVE_RMS]
    if not active_blocks:
        return {"ok": False, "reason": "no active tone detected in WAV", "frames": frame_count}

    first_block = active_blocks[0]
    last_block = active_blocks[-1]
    span_blocks = last_block - first_block + 1
    duration_s = span_blocks * AUDIO_PERIOD_FRAMES / AUDIO_RATE

    interior_start = first_block + SOUND_EDGE_GUARD_BLOCKS
    interior_end = last_block - SOUND_EDGE_GUARD_BLOCKS
    gap_blocks = []
    phase_jumps = []
    expected_step = (math.tau * SOUND_TONE_HZ * AUDIO_PERIOD_FRAMES / AUDIO_RATE) % math.tau
    cos_b = [math.cos(math.tau * SOUND_TONE_HZ * n / AUDIO_RATE) for n in range(AUDIO_PERIOD_FRAMES)]
    sin_b = [math.sin(math.tau * SOUND_TONE_HZ * n / AUDIO_RATE) for n in range(AUDIO_PERIOD_FRAMES)]

    prev_phase = None
    for b in range(interior_start, interior_end + 1):
        if block_rms[b] < SOUND_ACTIVE_RMS:
            gap_blocks.append(b)
            prev_phase = None
            continue
        base = b * AUDIO_PERIOD_FRAMES
        re_l = 0.0
        im_l = 0.0
        for i in range(AUDIO_PERIOD_FRAMES):
            s = samples[base + i][0] / 32767.0
            re_l += s * cos_b[i]
            im_l -= s * sin_b[i]
        phase = math.atan2(im_l, re_l)
        if prev_phase is not None:
            actual_step = (phase - prev_phase) % math.tau
            diff = (actual_step - expected_step + math.pi) % math.tau - math.pi
            if abs(diff) > SOUND_MAX_PHASE_JUMP_RAD:
                phase_jumps.append({"block": b, "diff_rad": round(diff, 4)})
        prev_phase = phase

    peaks = {}
    for hz in (SOUND_TONE_HZ, SOUND_ABSENT_HZ):
        cos_w = [math.cos(math.tau * hz * n / AUDIO_RATE) for n in range(AUDIO_RATE)]
        sin_w = [math.sin(math.tau * hz * n / AUDIO_RATE) for n in range(AUDIO_RATE)]
        ch_peaks = []
        for ch in (0, 1):
            amps = []
            start_frame = interior_start * AUDIO_PERIOD_FRAMES
            end_frame = (interior_end + 1) * AUDIO_PERIOD_FRAMES
            for s0 in range(start_frame, end_frame - AUDIO_RATE + 1, AUDIO_RATE // 2):
                real = 0.0
                imag = 0.0
                for i in range(AUDIO_RATE):
                    v = samples[s0 + i][ch]
                    real += v * cos_w[i]
                    imag += v * sin_w[i]
                amps.append(2.0 * math.hypot(real, imag) / AUDIO_RATE / 32767.0)
            ch_peaks.append(round(max(amps, default=0.0), 6))
        peaks[hz] = ch_peaks

    gain_error_db = [
        round(20.0 * math.log10(v / SOUND_TARGET_AMP), 3) if v > 0 else -999.0
        for v in peaks[SOUND_TONE_HZ]
    ]
    duration_ok = abs(duration_s - expected_duration_s) <= SOUND_DURATION_TOL_S
    freq_ok = (
        all(abs(err) <= SOUND_AMP_TOL_DB for err in gain_error_db)
        and max(peaks[SOUND_ABSENT_HZ]) <= 0.002
    )
    continuity_ok = len(gap_blocks) == 0 and len(phase_jumps) == 0
    return {
        "frames": frame_count,
        "duration_s": round(duration_s, 4),
        "duration_ok": duration_ok,
        "amplitudes": peaks,
        "gain_error_db": gain_error_db,
        "freq_ok": freq_ok,
        "gap_blocks": gap_blocks[:10],
        "phase_jumps": phase_jumps[:10],
        "continuity_ok": continuity_ok,
        "ok": duration_ok and freq_ok and continuity_ok,
    }


def run_mpv_sound(args, out, helpers):
    Blocked = helpers["Blocked"]
    audio_vm = helpers["audio_vm"]
    root_shell = helpers["root_shell"]
    run_status = helpers["run_status"]

    if not shutil.which("ffmpeg"):
        raise Blocked("host lacks ffmpeg")

    out.mkdir(parents=True, exist_ok=True)
    clips_dir = out / "clips"
    guest_dir = out / "guest-out"
    clips_dir.mkdir(parents=True, exist_ok=True)
    guest_dir.mkdir(parents=True, exist_ok=True)

    clip_name = "tone_1499.ogg"
    clip_path = clips_dir / clip_name
    run_host_ffmpeg([
        "-f", "lavfi",
        "-i", f"sine=frequency={SOUND_TONE_HZ}:sample_rate=48000:duration={SOUND_DURATION_S}",
        "-af", "volume=2.0,pan=stereo|c0=c0|c1=c0",
        "-c:a", "libopus",
        "-b:a", "192k",
        str(clip_path),
    ])
    clip_sha = sha256_file(clip_path)

    wav = out / "audio.wav"
    wav.unlink(missing_ok=True)
    record = {
        "clip": clip_name,
        "clip_sha256": clip_sha,
        "expected_hz": SOUND_TONE_HZ,
        "absent_hz": SOUND_ABSENT_HZ,
        "expected_duration_s": SOUND_DURATION_S,
    }

    server = start_clip_server(clips_dir, guest_dir)
    port = server.server_address[1]
    base = f"http://10.0.2.2:{port}"
    curl_post = "curl --http1.0 -H Expect: -fsS --max-time 30 --retry 2 --data-binary"
    curl_get = "curl --http1.0 -fsS --max-time 30 --retry 2"
    problems = []

    try:
        vm = audio_vm(args, out, wav)
        try:
            if not root_shell(vm, args.timeout):
                return False, f"root serial login failed; see {out}"

            code, net_text = run_status(
                vm,
                'bash -c \'deadline=$((SECONDS+60)); server=$(cat /etc/net/dns); '
                'while [[ -z "$server" || "$server" == 9.9.9.9 ]] && (( SECONDS < deadline )); do '
                'sleep 0.1; server=$(cat /etc/net/dns); done; '
                '[[ -n "$server" && "$server" != 9.9.9.9 ]]\'',
                timeout=70,
            )
            if code != 0:
                return False, f"DHCP did not finish: {net_text.strip()}; see {out}"

            code, help_out = run_status(
                vm,
                "bash -c 'mkdir -p /tmp/media && mpv --ao=help > /tmp/media/ao-help.txt 2>&1 && "
                f"{curl_post} @/tmp/media/ao-help.txt {base}/ao-help.txt'",
                timeout=60,
            )
            if code != 0:
                return False, f"mpv --ao=help failed (rc={code}): {help_out.strip()[-300:]}; see {out}"

            ao_help = (guest_dir / "ao-help.txt").read_text(encoding="utf-8", errors="replace")
            has_ferrion_ao = any(line.strip().startswith("ferrion") for line in ao_help.splitlines())
            record["ao_help_lists_ferrion"] = has_ferrion_ao
            if not has_ferrion_ao:
                record["problems"] = ["mpv --ao=help does not list ferrion"]
                (out / "mpv-sound.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
                return False, f"mpv --ao=help does not list ferrion; see {out}"

            code, vol_out = run_status(vm, "bash -c 'echo 100 > /scheme/audio/volume'", timeout=30)
            if code != 0:
                return False, f"setting /scheme/audio/volume to 100 failed: {vol_out.strip()}; see {out}"

            code, dl_out = run_status(
                vm,
                f"bash -c '{curl_get} {base}/{clip_name} -o /tmp/media/{clip_name} && "
                f"got=$(sha256sum /tmp/media/{clip_name} | cut -d\" \" -f1) && "
                f"test \"$got\" = \"{clip_sha}\"'",
                timeout=90,
            )
            if code != 0:
                return False, f"download or sha256 failed for {clip_name}: {dl_out.strip()}; see {out}"

            code, play_out = run_status(
                vm,
                f"bash -c 'rc=0; mpv --no-config --vo=null --msg-level=all=v /tmp/media/{clip_name} "
                f"> /tmp/media/mpv-sound.log 2>&1 || rc=$?; "
                f"{curl_post} @/tmp/media/mpv-sound.log {base}/mpv-sound.log || true; sleep 0.5; exit $rc'",
                timeout=120,
            )
            if code is None:
                return False, f"timeout playing {clip_name} with mpv; see {out}"

            log_path = guest_dir / "mpv-sound.log"
            log_text = log_path.read_text(encoding="utf-8", errors="replace") if log_path.is_file() else ""
            ao_ferrion = "AO: [ferrion]" in log_text
            eof = "Exiting... (End of file)" in log_text
            drop_match = re.search(r"Dropped:\s*(\d+)", log_text)
            dropped = int(drop_match.group(1)) if drop_match else 0
            clamp_match = re.search(r"clamped samples:\s*(\d+)", log_text)
            clamped = int(clamp_match.group(1)) if clamp_match else None
            record["mpv"] = {
                "exit_code": code,
                "ao_ferrion": ao_ferrion,
                "eof": eof,
                "dropped": dropped,
                "clamped_samples": clamped,
            }
            if code != 0:
                problems.append(f"mpv exited {code}: {(log_text or play_out).strip()[-300:]}")
            if not ao_ferrion:
                problems.append("mpv did not select AO: [ferrion] by default")
            if not eof:
                problems.append("mpv did not reach End of file")
            if dropped != 0:
                problems.append(f"mpv reported {dropped} dropped frames")
        finally:
            vm.close()
    finally:
        server.shutdown()
        server.server_close()

    wav_report = analyze_mpv_wav(wav.read_bytes()) if wav.is_file() else {"ok": False, "reason": "audio.wav missing"}
    record["wav"] = wav_report
    if not wav_report.get("ok"):
        problems.append(f"WAV verification failed: {wav_report}")

    record["problems"] = problems
    (out / "mpv-sound.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    if problems:
        return False, "; ".join(problems) + f"; see {out}"
    return True, (
        f"AO: [ferrion] default + EOF, {wav_report['duration_s']:.3f}s tone at {SOUND_TONE_HZ} Hz "
        f"(gain_err={wav_report['gain_error_db']} dB, absent={wav_report['amplitudes'][SOUND_ABSENT_HZ]}, "
        f"gaps=0, phase_jumps=0); see {out}"
    )
