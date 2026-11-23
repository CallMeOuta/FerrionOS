#!/usr/bin/env python3

import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FFMPEG_RECIPE = ROOT / "recipes" / "libs" / "ffmpeg" / "recipe.toml"
DAV1D_RECIPE = ROOT / "recipes" / "libs" / "dav1d" / "recipe.toml"
OLD_DAV1D_RECIPE = ROOT / "recipes" / "wip" / "codecs" / "dav1d" / "recipe.toml"

MAX_ARRAY_ITEMS = 256
MAX_CONFIG_DEPTH = 32
MAX_CONFIG_FILES = 512

ALLOWED_IMAGE_DECODERS = frozenset({
    "libdav1d",
    "vp9",
    "vp8",
    "theora",
    "vp3",
    "mpeg1video",
    "mpeg2video",
    "png",
    "mjpeg",
    "gif",
    "bmp",
    "webp",
    "webp_anim",
    "ffv1",
    "huffyuv",
    "rawvideo",
    "flac",
    "opus",
    "vorbis",
    "mp2",
    "mp2float",
    "mp3",
    "mp3float",
    "mp3adu",
    "mp3adufloat",
    "mp3on4",
    "mp3on4float",
    "pcm_u8",
    "pcm_s8",
    "pcm_s16le",
    "pcm_s16be",
    "pcm_s16le_planar",
    "pcm_s16be_planar",
    "pcm_s24le",
    "pcm_s24be",
    "pcm_s24le_planar",
    "pcm_s24be_planar",
    "pcm_s32le",
    "pcm_s32be",
    "pcm_s32le_planar",
    "pcm_s32be_planar",
    "pcm_f32le",
    "pcm_f32be",
    "pcm_f64le",
    "pcm_f64be",
    "pcm_alaw",
    "pcm_mulaw",
})

ALLOWED_IMAGE_ENCODERS = frozenset({
    "rawvideo",
    "wrapped_avframe",
    "pcm_s16le",
    "pcm_f32le",
})

REQUIRED_FFMPEG_FLAGS = (
    "--disable-everything",
    "--disable-autodetect",
    "--disable-ffplay",
    "--disable-sdl2",
    "--disable-network",
    "--enable-zlib",
    "--enable-libdav1d",
)

FORBIDDEN_FFMPEG_FLAGS = (
    "--enable-gpl",
    "--enable-nonfree",
    "--enable-version3",
    "--enable-ffplay",
    "--enable-sdl2",
    "--enable-network",
)

FORBIDDEN_FFMPEG_DEPS = frozenset({
    "liborbital",
    "mesa",
    "sdl1",
    "sdl2",
    "libvulkan",
})


def parse_bash_array(script, name):
    match = re.search(rf"(?m)^{re.escape(name)}=\(\s*(.*?)\s*\)", script, re.DOTALL)
    if not match:
        return None
    items = [tok for tok in match.group(1).split() if tok]
    if len(items) > MAX_ARRAY_ITEMS:
        raise ValueError(f"{name} exceeds bound {MAX_ARRAY_ITEMS}")
    return items


def check_ffmpeg_recipe():
    errors = []
    if not FFMPEG_RECIPE.is_file():
        return [f"{FFMPEG_RECIPE.relative_to(ROOT).as_posix()}: missing recipe"]
    data = tomllib.loads(FFMPEG_RECIPE.read_text(encoding="utf-8"))
    source = data.get("source", {})
    if "tar" not in source or not source.get("blake3"):
        errors.append("recipes/libs/ffmpeg/recipe.toml: tar source must be pinned by blake3")

    build = data.get("build", {})
    deps = set(build.get("dependencies", [])) | set(build.get("dev-dependencies", []))
    for dep in sorted(deps & FORBIDDEN_FFMPEG_DEPS):
        errors.append(f"recipes/libs/ffmpeg/recipe.toml: forbidden dependency {dep}")
    if "dav1d" not in deps:
        errors.append("recipes/libs/ffmpeg/recipe.toml: missing dependency dav1d")

    script = build.get("script", "")
    for flag in REQUIRED_FFMPEG_FLAGS:
        if flag not in script:
            errors.append(f"recipes/libs/ffmpeg/recipe.toml: missing required flag {flag}")
    for flag in FORBIDDEN_FFMPEG_FLAGS:
        if flag in script:
            errors.append(f"recipes/libs/ffmpeg/recipe.toml: forbidden flag {flag}")

    decoders = parse_bash_array(script, "DECODERS")
    if decoders is None:
        errors.append("recipes/libs/ffmpeg/recipe.toml: missing DECODERS array")
        decoders = []
    for m in re.finditer(r"--enable-decoder=([^\s\"']+)", script):
        val = m.group(1)
        if val != "${name}":
            decoders.append(val)
    if "libdav1d" not in decoders:
        errors.append("recipes/libs/ffmpeg/recipe.toml: libdav1d missing from enabled decoders")
    for dec in sorted(set(decoders) - ALLOWED_IMAGE_DECODERS):
        errors.append(f"recipes/libs/ffmpeg/recipe.toml: decoder outside image allow-list: {dec}")

    encoders = parse_bash_array(script, "ENCODERS")
    if encoders is None:
        errors.append("recipes/libs/ffmpeg/recipe.toml: missing ENCODERS array")
        encoders = []
    for m in re.finditer(r"--enable-encoder=([^\s\"']+)", script):
        val = m.group(1)
        if val != "${name}":
            encoders.append(val)
    for enc in sorted(set(encoders) - ALLOWED_IMAGE_ENCODERS):
        errors.append(f"recipes/libs/ffmpeg/recipe.toml: encoder outside image allow-list: {enc}")

    return errors


def check_dav1d_recipe():
    errors = []
    if OLD_DAV1D_RECIPE.exists():
        errors.append("recipes/wip/codecs/dav1d/recipe.toml: duplicate wip dav1d recipe still exists")
    if not DAV1D_RECIPE.is_file():
        return errors + ["recipes/libs/dav1d/recipe.toml: missing recipe"]
    data = tomllib.loads(DAV1D_RECIPE.read_text(encoding="utf-8"))
    source = data.get("source", {})
    if "tar" not in source or not source.get("blake3"):
        errors.append("recipes/libs/dav1d/recipe.toml: tar source must be pinned by blake3")
    script = data.get("build", {}).get("script", "")
    if "usr/share/licenses/dav1d/PATENTS" not in script:
        errors.append("recipes/libs/dav1d/recipe.toml: must install doc/PATENTS to /usr/share/licenses/dav1d/PATENTS")
    return errors


def is_unfetched_recipe_source(path):
    try:
        parts = path.resolve().relative_to(ROOT).parts
    except ValueError:
        return False
    return len(parts) >= 4 and parts[0] == "recipes" and "source" in parts and not path.exists()


def resolve_packages(path, stack=()):
    resolved = path.resolve()
    if len(stack) >= MAX_CONFIG_DEPTH:
        raise ValueError(f"include depth exceeded {MAX_CONFIG_DEPTH} at {resolved}")
    if resolved in stack:
        raise ValueError(f"include cycle at {resolved}")
    data = tomllib.loads(resolved.read_text(encoding="utf-8"))
    packages = {}
    for inc in data.get("include", []):
        target = resolved.parent / inc
        if is_unfetched_recipe_source(target):
            continue
        packages.update(resolve_packages(target, stack + (resolved,)))
    for pkg, rule in data.get("packages", {}).items():
        if rule == "ignore":
            packages.pop(pkg, None)
        else:
            packages[pkg] = rule
    return packages


def tracked_configs():
    out = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "config/**/*.toml", "config/*.toml"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    paths = [ROOT / p for p in sorted(set(out.stdout.splitlines())) if (ROOT / p).is_file()]
    if len(paths) > MAX_CONFIG_FILES:
        raise ValueError(f"config count {len(paths)} exceeds bound {MAX_CONFIG_FILES}")
    return paths


def check_config_conflicts():
    errors = []
    for path in tracked_configs():
        rel = path.relative_to(ROOT).as_posix()
        try:
            packages = resolve_packages(path)
        except (OSError, ValueError, tomllib.TOMLDecodeError) as err:
            errors.append(f"{rel}: failed to resolve packages: {err}")
            continue
        if "ffmpeg" in packages and "ffmpeg6" in packages:
            errors.append(f"{rel}: installs both ffmpeg and ffmpeg6")
    return errors


def main():
    errors = []
    errors.extend(check_ffmpeg_recipe())
    errors.extend(check_dav1d_recipe())
    errors.extend(check_config_conflicts())
    for line in errors:
        print(line)
    if errors:
        print(f"{len(errors)} media codec policy issue(s) found")
        return 1
    print("Media codec policy check passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
