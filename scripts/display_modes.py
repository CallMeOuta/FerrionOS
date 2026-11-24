import re
import sys
import struct
import tempfile
import time
from pathlib import Path

from display_handoff import RFB_MSG_SET_DESKTOP_SIZE, VncResizeClient

MODES = ((1280, 800), (1920, 1080), (2560, 1600), (3840, 2160))
WINDOW_SIZES = ((1366, 768), (1918, 1047), (2556, 1384), (3838, 2054))
HEAD_SIZES = ((3840, 2160), (2560, 1600))
HOTPLUG_SIZE = (1920, 1080)
GPU_HEADS_SIZE = (1280, 800)
GROUND = (0x0F, 0x0E, 0x0C)
COLOR_STEP = 3
TASKBAR_MIN_ROWS = 32
TASKBAR_MAX_ROWS = 256
TASKBAR_SAMPLES = 64
CLOCK_MIN_LIT = 16
DESKTOP_POLL_S = 0.5
WINDOW_MOVE_STEPS = 400
SCREENDUMP_TRIES = 20
SCREENDUMP_RETRY_S = 0.25
WINDOW_OPEN_S = 30
WINDOW_OPEN_TRIES = 3
WINDOW_MIN_POINTS = 64
KEY_PRESS_S = 0.15
FRAMEBUFFER_ERRORS = re.compile(
    r"failed to create or map framebuffer|failed to resize display|failed to open display|"
    r"virtio-gpud: .*(?:panicked|error)"
)


def near(pixel, color):
    return all(abs(pixel[channel] - color[channel]) <= COLOR_STEP for channel in range(3))


def pixel(frame, x, y):
    offset = (y * frame.width + x) * 3
    return tuple(frame.rgb[offset:offset + 3])


def taskbar_rows(frame):
    """Rows of the bar along the bottom edge, or 0 when the bottom edge has no full-width bar."""
    middle = frame.width // 2
    bar = pixel(frame, middle, frame.height - 2)
    if near(bar, GROUND):
        return 0
    rows = 0
    while rows < TASKBAR_MAX_ROWS and rows < frame.height and near(pixel(frame, middle, frame.height - 1 - rows), bar):
        rows += 1
    if not TASKBAR_MIN_ROWS <= rows < TASKBAR_MAX_ROWS:
        return 0
    if not near(pixel(frame, middle, frame.height - rows - 8), GROUND):
        return 0
    row = frame.height - rows // 2
    left, right = frame.width // 2, frame.width * 17 // 20
    step = max(1, (right - left) // TASKBAR_SAMPLES)
    samples = [near(pixel(frame, x, row), bar) for x in range(left, right, step)]
    if sum(samples) * 10 < len(samples) * 9:
        return 0
    if frame.lit_in(frame.width * 17 // 20, frame.height - rows, frame.width, frame.height) < CLOCK_MIN_LIT:
        return 0
    return rows


def desktop_drawn(frame):
    """The desktop's ground fills the middle of the frame."""
    points = [(frame.width * column // 8, frame.height * row // 8) for column in range(1, 8) for row in (2, 4)]
    return sum(near(pixel(frame, x, y), GROUND) for x, y in points) * 4 >= len(points) * 3


def set_head_size(path, width, height):
    """Asks QEMU, as the head's VNC client, for a size; 0x0 turns the head off."""
    client = VncResizeClient(path)
    try:
        client.sock.sendall(struct.pack("!BBHHBBIHHHHI", RFB_MSG_SET_DESKTOP_SIZE, 0, width, height, 1, 0,
                                        0, 0, 0, width, height, 0))
        time.sleep(0.2)
    finally:
        client.close()


def read_ppm(frame_cls, data):
    """A frame from QEMU's PPM, which pads each row to four bytes when the width times three is not a multiple of four."""
    match = re.match(rb"P6\s+(\d+)\s+(\d+)\s+255\s", data)
    if not match:
        return None
    width, height = int(match.group(1)), int(match.group(2))
    body = data[match.end():]
    row = width * 3
    stride = (row + 3) & ~3
    if len(body) == row * height:
        return frame_cls(width, height, body)
    if len(body) == stride * height:
        return frame_cls(width, height, b"".join(body[y * stride:y * stride + row] for y in range(height)))
    return None


def shot(vm, head):
    """A screendump of `head`, read once QEMU has finished writing it."""
    path = vm.scratch / f"head{head}.ppm"
    path.unlink(missing_ok=True)
    reply = vm.command(f"screendump {path} gpu0 {head}")
    frame_cls = sys.modules[type(vm).__module__].Frame
    for _ in range(SCREENDUMP_TRIES):
        if path.exists():
            frame = read_ppm(frame_cls, path.read_bytes())
            if frame is not None:
                return frame
        time.sleep(SCREENDUMP_RETRY_S)
    raise RuntimeError(f"screendump of head {head} incomplete after {SCREENDUMP_TRIES} reads: {reply[-200:].strip()}")


def framebuffer_errors(vm):
    return sorted({match.group(0) for match in FRAMEBUFFER_ERRORS.finditer(vm.text())})


def wait_login(vm, timeout, head=None):
    while vm.elapsed() < timeout:
        if shot(vm, head=head).kind() == "login":
            return round(vm.elapsed(), 1)
        time.sleep(0.2)
    return None


def wait_frame(vm, head, size, accept, timeout):
    """Screenshots `head` until a frame of exactly `size` passes `accept`; returns (frame, seconds) or (last, None)."""
    deadline = time.monotonic() + timeout
    frame = None
    while True:
        frame = shot(vm, head=head)
        if (frame.width, frame.height) == size and accept(frame):
            return frame, round(vm.elapsed(), 1)
        if time.monotonic() >= deadline:
            return frame, None
        time.sleep(DESKTOP_POLL_S)


def one_mode(vm_cls, args, out, size, resize_to):
    width, height = size
    with tempfile.TemporaryDirectory(prefix="ferrion-display-modes-") as directory:
        vnc = Path(directory) / "head0.sock"
        vm = vm_cls(args, out, qemu_flags=f"-vnc unix:{vnc},display=gpu0,head=0",
                    make=("gpu=virtio", f"QEMU_GPU_OPTS=,xres={width},yres={height}"))
        try:
            login = wait_login(vm, args.timeout, head=0)
            if login is None:
                shot(vm, head=0).save_png(out / "no-login.png")
                return False, {"size": f"{width}x{height}", "note": f"no graphical login within {args.timeout}s"}
            vm.command("sendkey ret")
            frame, desktop = wait_frame(vm, 0, size, taskbar_rows, args.timeout)
            frame.save_png(out / f"desktop-{width}x{height}.png")
            result = {"size": f"{width}x{height}", "login_s": login, "desktop_s": desktop,
                      "frame": f"{frame.width}x{frame.height}", "taskbar_rows": taskbar_rows(frame)}
            ok = desktop is not None
            if ok:
                set_head_size(vnc, *resize_to)
                started = vm.elapsed()
                frame, followed = wait_frame(vm, 0, resize_to, taskbar_rows, 60)
                frame.save_png(out / f"resized-{resize_to[0]}x{resize_to[1]}.png")
                result["resized"] = f"{frame.width}x{frame.height}"
                result["resize_s"] = None if followed is None else round(followed - started, 1)
                ok = followed is not None
            result["errors"] = framebuffer_errors(vm)
            return ok and not result["errors"], result
        finally:
            vm.close()


def run_display_modes(args, out, vm_cls):
    """For each preferred size the desktop comes up at exactly that size with its taskbar on the bottom edge, then follows a host resize."""
    results, passed = [], 0
    for index, size in enumerate(MODES):
        resize_to = WINDOW_SIZES[index]
        run_out = Path(out) / f"{size[0]}x{size[1]}"
        run_out.mkdir(parents=True, exist_ok=True)
        ok, result = one_mode(vm_cls, args, run_out, size, resize_to)
        passed += ok
        results.append(result)
        print(f"{'PASS' if ok else 'FAIL'} display-modes {result}", flush=True)
    summary = "; ".join(f"{r['size']}: frame {r.get('frame')}, desktop {r.get('desktop_s')}s, "
                        f"resize to {r.get('resized')} in {r.get('resize_s')}s, errors {r.get('errors')}"
                        for r in results)
    return passed == len(MODES), f"{passed}/{len(MODES)} sizes; {summary}"


def head_state(vm, out, name, expected):
    frames = []
    for head, size in enumerate(expected):
        frame = shot(vm, head=head)
        frame.save_png(Path(out) / f"{name}-head{head}.png")
        frames.append(frame)
    return frames


def run_display_heads(args, out, vm_cls):
    """Two heads at their own sizes, desktop on both, taskbar on the first, a window moved across, head 1 off and on again."""
    out = Path(out)
    notes = []
    with tempfile.TemporaryDirectory(prefix="ferrion-display-heads-") as directory:
        sockets = [Path(directory) / f"head{head}.sock" for head in range(2)]
        flags = " ".join(f"-vnc unix:{path},display=gpu0,head={head}" for head, path in enumerate(sockets))
        vm = vm_cls(args, out, qemu_flags=flags, make=("gpu=multi",))
        try:
            for path, size in zip(sockets, HEAD_SIZES):
                set_head_size(path, *size)
            login = wait_login(vm, args.timeout, head=0)
            if login is None:
                shot(vm, head=0).save_png(out / "no-login.png")
                return False, f"no graphical login within {args.timeout}s"
            vm.command("sendkey ret")
            first, desktop = wait_frame(vm, 0, HEAD_SIZES[0], taskbar_rows, args.timeout)
            second, second_s = wait_frame(vm, 1, HEAD_SIZES[1], desktop_drawn, 30)
            first.save_png(out / "boot-head0.png")
            second.save_png(out / "boot-head1.png")
            boot_ok = desktop is not None and second_s is not None and not taskbar_rows(second)
            notes.append(f"boot: head 0 {first.width}x{first.height} taskbar {taskbar_rows(first)} rows at {desktop}s, "
                         f"head 1 {second.width}x{second.height} desktop {second_s is not None}, "
                         f"taskbar {taskbar_rows(second)} rows")

            moved = move_window_across(vm, out)
            notes.append(f"window on head 1: {moved}")

            set_head_size(sockets[1], 0, 0)
            off = vm.wait_serial(r"virtio-gpu: display 1 \(0x0px\)", 30)
            kept, kept_s = wait_frame(vm, 0, HEAD_SIZES[0], taskbar_rows, 30)
            kept.save_png(out / "off-head0.png")
            notes.append(f"head 1 off: guest saw it {bool(off)}, head 0 kept {kept_s is not None}")

            set_head_size(sockets[1], *HOTPLUG_SIZE)
            back, back_s = wait_frame(vm, 1, HOTPLUG_SIZE, desktop_drawn, 60)
            back.save_png(out / "on-head1.png")
            again, again_s = wait_frame(vm, 0, HEAD_SIZES[0], taskbar_rows, 30)
            again.save_png(out / "on-head0.png")
            notes.append(f"head 1 on: {back.width}x{back.height} desktop {back_s is not None}, "
                         f"head 0 kept {again_s is not None}")

            errors = framebuffer_errors(vm)
            notes.append(f"errors {errors}")
            ok = (boot_ok and moved and bool(off) and kept_s is not None and back_s is not None
                  and again_s is not None and not errors)
            return ok, "; ".join(notes)
        finally:
            vm.close()


def run_gpu_heads(args, out, vm_cls, check):
    """gpu-heads on gpu=multi with head 1 given a size first: QEMU starts a second head turned off, which the driver reports as disconnected."""
    with tempfile.TemporaryDirectory(prefix="ferrion-gpu-heads-") as directory:
        socket_path = Path(directory) / "head1.sock"
        vm = vm_cls(args, out, qemu_flags=f"-vnc unix:{socket_path},display=gpu0,head=1", make=("gpu=multi",))
        try:
            set_head_size(socket_path, *GPU_HEADS_SIZE)
            return check(vm, args)
        finally:
            vm.close()


def move_window_across(vm, out):
    """Opens a terminal with Super+T on head 0 and moves it with Super+Right onto head 1."""
    before = shot(vm, head=1)
    desktop = shot(vm, head=0)
    opened, opened_s = desktop, None
    for _ in range(WINDOW_OPEN_TRIES):
        vm.command("sendkey meta_l-t")
        opened, opened_s = wait_frame(vm, 0, HEAD_SIZES[0],
                                      lambda frame: center_changed(frame, desktop) >= WINDOW_MIN_POINTS, WINDOW_OPEN_S)
        if opened_s is not None:
            break
    opened.save_png(Path(out) / "window-head0.png")
    if opened_s is None:
        return False
    for _ in range(WINDOW_MOVE_STEPS // 16):
        for _ in range(16):
            vm.command("sendkey meta_l-right")
            time.sleep(KEY_PRESS_S)
        frame = shot(vm, head=1)
        if frame.rgb != before.rgb and not desktop_drawn_left(frame):
            frame.save_png(Path(out) / "window-head1.png")
            return True
    shot(vm, head=1).save_png(Path(out) / "window-head1.png")
    return False


def center_changed(frame, reference):
    """Points of a grid over the middle half of the frame whose color differs from `reference`."""
    points = [(frame.width * (16 + column) // 64, frame.height * (16 + row) // 64)
              for column in range(32) for row in range(32)]
    return sum(pixel(frame, x, y) != pixel(reference, x, y) for x, y in points)


def desktop_drawn_left(frame):
    """True while the left eighth of the frame is still all ground, so no window has arrived from head 0."""
    points = [(frame.width * column // 64, frame.height * row // 64) for column in range(1, 8) for row in range(4, 60)]
    return all(near(pixel(frame, x, y), GROUND) for x, y in points)
