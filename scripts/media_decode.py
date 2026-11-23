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
                    f"if test \"$got\" = \"{ref_sha}\"; then echo MEDIA_AUDIO_EXACT; else "
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
                elif "MEDIA_AUDIO_EXACT" in a_cmd:
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
                f"if test \"$got\" = \"{ref_sha}\"; then echo MEDIA_AUDIO_EXACT; else "
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
            if "MEDIA_AUDIO_EXACT" in a_cmd:
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
