# A GPU's render handle, its Venus context, and the check that proves them

Status: accepted

## Context

Slice GP3 of the GPU work gives FerrionOS's virtio-gpu driver a render side: Mesa's Venus driver needs a render node that answers the version, get-parameter, get-capset, context-init and close requests the way Linux's virtio-gpu driver does, and a context on the host per client. The display scheme already exists and only a virtual terminal's compositor should drive it. The per-call conventions are in [`docs/gpu-conventions.md`](../gpu-conventions.md) and the calls in [`docs/gpu-needs-ledger.md`](../gpu-needs-ledger.md); this record holds the choices in the code that a reader would otherwise undo.

## Decision

### The driver (base's `ferrion-virtio-gpu-render.patch`)

- One scheme, two names: `virtio-gpud` registers `gpu.virtio-gpu` beside `display.virtio-gpu` on the same socket, and only when the device offers 3D. On a 2D device Mesa would skip a render node that reports no 3D anyway, so a name there would serve nothing.
- Each open of the render root is a render handle with at most one context. No request names a context, so no other handle can use, initialize or close it, and closing the handle destroys it on the device.
- A render handle answers the version request as `virtio_gpu` 0.1.0, the name and major version Mesa matches. It refuses DRM's own and mode-setting requests with `EACCES`, as Linux does on a render node, and unknown driver requests with `EINVAL`.
- The device's answers are read with their type as a number, never as the driver's command enumeration, and the capsets are read once at start within bounds: 64 capsets, ids up to 63, 64 KiB and eight versions each. Get-capset therefore never waits on the device.
- A context's name stops at its first zero byte and is kept to printable ASCII before the device sees it, because it reaches the host's logs and traces. It is at most 63 characters, as many as Linux's `strscpy` leaves in the device command's 64-byte field.
- At most 256 contexts live on a device, each being a worker in the host's render server. While only root's namespace holds the render name, a share per process would protect nothing; quotas per handle come with the GPU grant (slice GP12).
- Context create and destroy wait for the device with no timeout. Measured on the build host, the device answered in about a millisecond or less, and QEMU answers the control queue in order, so a timeout could not free the driver. Submissions and fences, which can wait far longer, are the next slice's to make asynchronous, with the context commands moved onto the same path.
- The shared memory region of the host-visible window stays unmapped until blobs exist.

### relibc (`ferrion-virtgpu-requests.patch`)

- Context-init's `DEBUG_NAME` parameter holds the address of a string inside an array element, which no payload carries. relibc reads the parameters by value and sends the name after them: the string the parameter names, or else the name the program runs under, as Linux names a context after the process. The virtgpu module's context-init payload is written by hand for this, and its `LAYOUT` stays the part Linux's structure has, which the layout check compares.

### The base recipe

- base lists relibc in `dev-dependencies`. Since `ferrion-redox-ioctl-path.patch`, base compiles relibc's `redox-ioctl` from relibc's fetched source, so the dependency makes the cookbook fetch, patch and cook relibc first and cook base again when relibc changes. Without it the cookbook was seen to cook base before relibc even when asked for `relibc,base`, and on a fresh checkout base would then find no relibc source. Nothing in base compiles C for the target, so relibc's headers and libraries in base's sysroot reach no compiler.

### The probe and the check

- `gpu-probe-context` speaks the DRM requests only, with no library but the C library, so it tests the driver and relibc and not Mesa. It links the C library dynamically, so that it runs against the image's relibc, whose translation carries the requests; a static link would take the prefix's relibc.
- It runs as root: the render name is the GPU grant and is not in a `user` login's namespace, and widening that is the grant's slice.
- It expects `EACCES` from mode-setting requests on a render handle. relibc refuses a request it does not forward with `EINVAL`, so `EACCES` shows that the driver itself refused.
- It prints the name relibc will give its first context (the basename of the program, cut to 63 characters), which the check matches in QEMU's trace, and it names the second and third contexts through `DEBUG_NAME`, so both paths are seen.
- The check is in `context.c` as `gpu_probe_context()`, with a `main.c` that makes it a program of its own. A `gpu-probe` with subcommands can compile `context.c` and call it for `gpu-probe context`; its context is then named `gpu-probe`, which the check reads from the probe's output rather than assuming.
- `scripts/vm-test.py gpu-context` enables QEMU's `virtio_gpu_features` and context trace events itself, whatever `--trace` says, boots with `gpu=venus` unless `--make` names another device, and waits briefly for the last close to reach the trace, which QEMU writes as events happen.
- The Venus capset's sha256 is compared with the newest record of `scripts/gpu-reference-guest.py` under `build/gpu-reference/`, because only the host's own QEMU can say what its capset is; without a record it falls back to the value that script read on the build host on 2026-11-21.

## Consequences

- Mesa's Venus driver finds no render node until enumeration lands; the probe proves the requests without it.
- A program without the render name in its namespace cannot reach the GPU, and none but root has it today.
- Reverting the dev-dependency breaks a full build from a fresh checkout; reverting the dynamic link makes the probe test the prefix's relibc instead of the image's.

## Where

- `recipes/core/base/ferrion-virtio-gpu-render.patch` (new files `driver-graphics/src/render.rs`, `driver-graphics/src/ioctl/virtgpu.rs`, `virtio-gpud/src/render.rs`), `recipes/core/base/recipe.toml`
- `recipes/core/relibc/ferrion-virtgpu-requests.patch`
- `recipes/tests/gpu-probe-context/`, `config/x86_64/desktop.toml`
- `scripts/vm-test.py`, `test_gpu_context` and `reference_venus_capset`
